//! The curator's desk: admin portal for managing rooms and hanging
//! specimens. Identity via Bluesky OAuth (see auth.rs); authorization via
//! the curators roster.

use atrium_oauth::{AuthorizeOptions, CallbackParams, KnownScope, Scope};
use axum::{
    Form,
    extract::{Multipart, Query, State},
    response::{IntoResponse, Redirect, Response},
};
use axum_extra::extract::Form as HtmlForm;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use maud::{DOCTYPE, Markup, html};
use tokio::io::AsyncWriteExt;

use crate::{
    SharedState,
    auth::{self, Curator, SESSION_COOKIE},
    catalog::MediaKind,
    db, storage, suggestions,
};

/// Errors render as a plain 500; auth failures redirect in the extractor.
pub struct AdminError(anyhow::Error);

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        tracing::error!(error = ?self.0, "admin request failed");
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "The curator's desk is jammed. Check the logs.",
        )
            .into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AdminError {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

fn admin_base(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " — the curator's desk" }
                link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=IM+Fell+English:ital@0;1&family=EB+Garamond:ital,wght@0,400;0,500;1,400&family=Caveat:wght@400;600&display=swap";
                link rel="stylesheet" href="/static/style.css";
                link rel="stylesheet" href="/static/admin.css";
            }
            body {
                main .sheet .admin {
                    header .masthead {
                        p .plate-no { "The Curator's Desk" }
                        h1 { a href="/" { "Fluoddity" } }
                    }
                    (body)
                }
            }
        }
    }
}

// ---- login flow ----

pub async fn login_page() -> Markup {
    admin_base(
        "Sign in",
        html! {
            section .login-box {
                h2 .room-label { "Sign in with Bluesky" }
                p .room-sublabel {
                    "Enter your handle. You'll be sent to your own PDS to approve — "
                    "no password ever touches this site."
                }
                form method="post" action="/admin/login" .admin-form {
                    input type="text" name="handle" placeholder="you.bsky.social"
                        required autocapitalize="none" autocorrect="off";
                    button type="submit" { "Continue →" }
                }
            }
        },
    )
}

#[derive(serde::Deserialize)]
pub struct LoginForm {
    handle: String,
}

pub async fn login_submit(
    State(state): State<SharedState>,
    Form(form): Form<LoginForm>,
) -> Result<Response, AdminError> {
    let handle = form.handle.trim().trim_start_matches('@').to_string();
    let url = state
        .oauth
        .authorize(
            &handle,
            AuthorizeOptions {
                scopes: vec![Scope::Known(KnownScope::Atproto)],
                ..Default::default()
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("authorize failed for {handle}: {e}"))?;
    Ok(Redirect::to(&url).into_response())
}

pub async fn oauth_callback(
    State(state): State<SharedState>,
    jar: CookieJar,
    Query(params): Query<CallbackParams>,
) -> Result<(CookieJar, Response), AdminError> {
    let (session, _) = state
        .oauth
        .callback(params)
        .await
        .map_err(|e| anyhow::anyhow!("oauth callback failed: {e}"))?;

    use atrium_api::agent::SessionManager;
    let did = session
        .did()
        .await
        .ok_or_else(|| anyhow::anyhow!("oauth session missing did"))?;
    let did = did.to_string();

    // Identity confirmed — we never act on the PDS, so drop the tokens now.
    if let Err(err) = sqlx::query!("DELETE FROM oauth_sessions WHERE did = $1", did)
        .execute(&state.pool)
        .await
    {
        tracing::warn!(?err, "failed to clear oauth session");
    }

    let allowed = sqlx::query!("SELECT did FROM curators WHERE did = $1", did)
        .fetch_optional(&state.pool)
        .await?
        .is_some();
    if !allowed {
        tracing::info!(%did, "login attempt from DID not on the roster");
        return Ok((jar, not_on_roster(&did).into_response()));
    }

    let token = auth::create_admin_session(&state.pool, &did).await?;
    let cookie = Cookie::build((SESSION_COOKIE, token))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .build();
    Ok((jar.add(cookie), Redirect::to("/admin").into_response()))
}

fn not_on_roster(did: &str) -> Markup {
    admin_base(
        "Not on the roster",
        html! {
            section .login-box {
                h2 .room-label { "Verified, but not on the roster" }
                p .room-sublabel {
                    "You are " code { (did) } " — but the survey hasn't added you as a "
                    "curator yet. Ask the keeper of the guide."
                }
                p { a href="/" { "← back to the guide" } }
            }
        },
    )
}

pub async fn logout(
    State(state): State<SharedState>,
    jar: CookieJar,
) -> Result<(CookieJar, Redirect), AdminError> {
    if let Some(token) = jar.get(SESSION_COOKIE).map(|c| c.value().to_string()) {
        sqlx::query!("DELETE FROM admin_sessions WHERE token = $1", token)
            .execute(&state.pool)
            .await?;
    }
    Ok((
        jar.remove(Cookie::build(SESSION_COOKIE).path("/").build()),
        Redirect::to("/"),
    ))
}

// ---- the desk ----

pub async fn dashboard(
    State(state): State<SharedState>,
    curator: Curator,
) -> Result<Markup, AdminError> {
    let thread_rooms = db::thread_rooms(&state.pool).await?;
    let pending = suggestions::pending(&state.pool).await?;
    let pending_rooms = suggestions::pending_rooms(&state.pool).await?;
    let catalog = db::load_catalog(&state.pool).await?;
    let artist_did = catalog.editorial.artist.did.clone();
    let removed = db::load_removed_specimens(&state.pool).await?;
    Ok(admin_base(
        "Rooms",
        html! {
            p .signed-in {
                "signed in as " strong { (display_name(&curator)) }
                " (" (curator.role) ") · "
                form method="post" action="/admin/logout" .inline-form {
                    button type="submit" .link-button { "sign out" }
                }
            }

            section {
                h2 .room-label { "The Rooms" }
                p .room-sublabel {
                    "a room is a Bluesky thread — paste a head post's URL to hang it. "
                    "The artist's threads become the museum's plates; anyone else's hang "
                    "as guest rooms. Rooms render live: edit the thread, the room follows."
                }
                @for tr in &thread_rooms {
                    div .admin-specimen {
                        a href=(format!("/room/{}/{}", tr.author_handle, tr.rkey)) { (tr.title) }
                        span .admin-date {
                            "@" (tr.author_handle)
                            @if tr.author_did == artist_did { " · plate" } @else { " · guest" }
                        }
                        form method="post" action="/admin/thread-rooms/remove" .inline-form {
                            input type="hidden" name="author_did" value=(tr.author_did);
                            input type="hidden" name="rkey" value=(tr.rkey);
                            button type="submit" .link-button title="take down" { "✕" }
                        }
                    }
                }
                form method="post" action="/admin/thread-rooms/add" .admin-form .room-create {
                    input type="text" name="url" placeholder="https://bsky.app/profile/curator/post/…" required;
                    button type="submit" { "hang this thread" }
                }
            }

            @if !pending_rooms.is_empty() {
                section {
                    h2 .room-label { "Rooms Awaiting the Wall" }
                    p .room-sublabel {
                        "threads the bot answered that hold real specimens — approve to hang "
                        "them on the homepage"
                    }
                    @for r in &pending_rooms {
                        div .admin-specimen {
                            a href=(format!("/room/{}/{}", r.author_handle, r.rkey)) { (r.title) }
                            span .admin-date {
                                "@" (r.author_handle) " · " (r.hung_count) " specimen"
                                @if r.hung_count != 1 { "s" }
                            }
                            form method="post" action="/admin/room-suggestions/resolve" .inline-form {
                                input type="hidden" name="suggestion_id" value=(r.suggestion_id);
                                button type="submit" name="action" value="approve" { "hang it" }
                                " "
                                button type="submit" name="action" value="decline" .link-button { "decline" }
                            }
                        }
                    }
                }
            }

            section {
                h2 .room-label { "The Vault" }
                p .room-sublabel {
                    "full-rate originals and the artist's source renders — "
                    a href="/admin/vault" { "see what's still wanted →" }
                }
            }

            section {
                h2 .room-label { "The Catalogue" }
                p .room-sublabel {
                    "every live specimen, with take-down controls — "
                    a href="/admin/specimens" { "curate the gallery →" }
                }
            }

            section {
                h2 .room-label { "The Suggestion Box" }
                p .room-sublabel {
                    "hashtags left in replies to (and quote-posts of) the artist's originals, "
                    "harvested daily. Approve and the tag hangs with the suggester's name on "
                    "the wall label; decline and it never comes back."
                }
                @if pending.is_empty() {
                    p .room-sublabel { "the box is empty — the community hasn't spoken since the last sweep" }
                }
                @for s in &pending {
                    div .admin-specimen {
                        @let label = catalog.archive.get(&s.rkey).map(|sp| sp.label()).unwrap_or_else(|| s.rkey.clone());
                        a href=(format!("/specimen/{}", s.rkey)) { (label) }
                        @let suggester = if s.suggester_handle.is_empty() { s.suggester_did.clone() } else { format!("@{}", s.suggester_handle) };
                        span .admin-date {
                            "#" (s.tag) " · by " (suggester) " · via " (s.via)
                        }
                        form method="post" action="/admin/suggestions/resolve" .inline-form {
                            input type="hidden" name="suggestion_id" value=(s.suggestion_id);
                            button type="submit" name="action" value="approve" { "hang it" }
                            " "
                            button type="submit" name="action" value="decline" .link-button { "decline" }
                        }
                    }
                }
            }

            @if !removed.is_empty() {
                section {
                    h2 .room-label { "Removed from the Gallery" }
                    p .room-sublabel {
                        "specimens taken down by curators — restored specimens return to the gallery immediately"
                    }
                    @for r in &removed {
                        div .admin-specimen {
                            a href=(format!("/specimen/{}", r.rkey)) { (r.label()) }
                            span .admin-date {
                                "removed " (r.removed_at.format("%Y-%m-%d %H:%M UTC"))
                                @if let Some(by) = &r.removed_by { " · by " (by) }
                            }
                            form method="post" action="/admin/specimens/restore" .inline-form {
                                input type="hidden" name="rkey" value=(r.rkey);
                                button type="submit" { "restore" }
                            }
                        }
                    }
                }
            }
        },
    ))
}

// ---- the vault (masters) ----

/// Upload cap for one master. Render-node originals run far past the loops'
/// few MB, but a master is still one ten-second clip, not a feature film.
pub const MAX_MASTER_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Extension for an accepted master upload; also the whitelist. Bunny's CDN
/// types objects by extension, so only extensions browsers can play get in.
fn master_extension(content_type: Option<&str>, file_name: Option<&str>) -> Option<&'static str> {
    match content_type
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
    {
        "video/mp4" => return Some("mp4"),
        "video/quicktime" => return Some("mov"),
        "video/webm" => return Some("webm"),
        _ => {}
    }
    match file_name?.rsplit('.').next()?.to_ascii_lowercase().as_str() {
        "mp4" | "m4v" => Some("mp4"),
        "mov" => Some("mov"),
        "webm" => Some("webm"),
        _ => None,
    }
}

/// The desiderata list: every video specimen the vault holds no master for,
/// each with an upload slot. This is the page the artist works through.
pub async fn masters_page(
    State(state): State<SharedState>,
    curator: Curator,
) -> Result<Markup, AdminError> {
    let catalog = db::load_catalog(&state.pool).await?;
    let videos: Vec<_> = catalog
        .archive
        .all()
        .iter()
        .filter(|s| s.kind == MediaKind::Video)
        .collect();
    let held = videos.iter().filter(|s| s.master_key.is_some()).count();
    let wanted: Vec<_> = videos
        .iter()
        .filter(|s| s.master_key.is_none())
        .rev() // newest sightings first — the freshest are easiest to find
        .collect();
    let uploads_ready = state.bunny.is_some();
    Ok(admin_base(
        "The Vault",
        html! {
            p .signed-in {
                "signed in as " strong { (display_name(&curator)) }
                " · " a href="/admin" { "← the desk" }
            }

            section {
                h2 .room-label { "The Vault · Wanted: Source Renders" }
                p .room-sublabel {
                    "the museum keeps the best copy of every specimen it can get. "
                    "Bluesky's CDN re-encodes what the timeline sees; the vault below "
                    "holds the originals. A source render straight from the render "
                    "node is better still — upload one here and every wall in the "
                    "guide upgrades on the spot."
                }
                p .room-sublabel {
                    strong { (held) } " source render"
                    @if held != 1 { "s" }
                    " held · " strong { (wanted.len()) } " still wanted"
                }
                @if !uploads_ready {
                    p .room-sublabel {
                        "⚠ the vault isn't accepting uploads — PCG_BUNNY_STORAGE_ZONE / "
                        "PCG_BUNNY_STORAGE_KEY are unset on this deployment"
                    }
                }
                @for s in &wanted {
                    div .admin-specimen {
                        a href=(format!("/specimen/{}", s.rkey)) { (s.label()) }
                        span .admin-date {
                            (s.date)
                            @if s.pds_key.is_some() { " · PDS original held" }
                            @else { " · Bluesky re-encode only" }
                        }
                        @if uploads_ready {
                            form method="post" action="/admin/vault/upload"
                                enctype="multipart/form-data" .inline-form {
                                input type="hidden" name="rkey" value=(s.rkey);
                                input type="file" name="master" accept="video/mp4,video/quicktime,video/webm" required;
                                button type="submit" { "into the vault" }
                            }
                        }
                    }
                }
                @if wanted.is_empty() {
                    p .room-sublabel { "the vault holds a source render for every specimen — the desiderata list is empty 🎉" }
                }
            }
        },
    ))
}

/// Take one master into the vault: spool the upload to a temp file (so
/// Bunny gets a Content-Length and a dropped connection never half-writes
/// the vault), PUT it, then record the key. One slot per specimen — a
/// re-upload replaces the master.
pub async fn upload_master(
    State(state): State<SharedState>,
    curator: Curator,
    mut multipart: Multipart,
) -> Result<Response, AdminError> {
    let Some(bunny) = state.bunny.clone() else {
        return Ok((
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "The vault isn't accepting uploads on this deployment.",
        )
            .into_response());
    };

    let mut rkey: Option<String> = None;
    while let Some(mut field) = multipart.next_field().await.map_err(anyhow::Error::from)? {
        match field.name() {
            Some("rkey") => rkey = Some(field.text().await.map_err(anyhow::Error::from)?),
            Some("master") => {
                // The form puts rkey before the file, so it's known by now.
                let Some(rkey) = rkey.as_deref().map(str::to_string) else {
                    return Ok(bad_request("The upload arrived without a specimen rkey."));
                };
                let is_video = sqlx::query!("SELECT kind FROM specimens WHERE rkey = $1", rkey)
                    .fetch_optional(&state.pool)
                    .await?
                    .is_some_and(|row| row.kind == "video");
                if !is_video {
                    return Ok(bad_request("No video specimen by that rkey."));
                }
                let Some(ext) = master_extension(field.content_type(), field.file_name()) else {
                    return Ok(bad_request(
                        "That doesn't look like a video file (mp4, mov, or webm).",
                    ));
                };

                let tmp =
                    std::env::temp_dir().join(format!("pcg-master-{}.part", uuid::Uuid::new_v4()));
                let spooled = spool_to_file(&mut field, &tmp).await;
                let result = match spooled {
                    Ok(bytes) => {
                        let key = storage::master_key(&rkey, ext);
                        let put = bunny.put_file(&state.http, &key, &tmp).await;
                        match put {
                            Ok(()) => {
                                sqlx::query!(
                                    "UPDATE specimens SET master_key = $1 WHERE rkey = $2",
                                    key,
                                    rkey
                                )
                                .execute(&state.pool)
                                .await?;
                                tracing::info!(curator = %curator.did, %rkey, %key, bytes, "master taken into the vault");
                                Ok(())
                            }
                            Err(err) => Err(err),
                        }
                    }
                    Err(err) => Err(err),
                };
                tokio::fs::remove_file(&tmp).await.ok();
                result?;
                return Ok(Redirect::to("/admin/vault").into_response());
            }
            _ => {}
        }
    }
    Ok(bad_request("The upload arrived without a file."))
}

/// Stream one multipart field to disk, enforcing the size cap. Returns the
/// byte count.
async fn spool_to_file(
    field: &mut axum::extract::multipart::Field<'_>,
    path: &std::path::Path,
) -> anyhow::Result<u64> {
    let mut out = tokio::fs::File::create(path).await?;
    let mut written: u64 = 0;
    while let Some(chunk) = field.chunk().await.map_err(anyhow::Error::from)? {
        written += chunk.len() as u64;
        anyhow::ensure!(
            written <= MAX_MASTER_BYTES,
            "upload exceeds {MAX_MASTER_BYTES} bytes"
        );
        out.write_all(&chunk).await?;
    }
    out.flush().await?;
    Ok(written)
}

fn bad_request(msg: &'static str) -> Response {
    (axum::http::StatusCode::BAD_REQUEST, msg).into_response()
}

#[derive(serde::Deserialize)]
pub struct ResolveSuggestionForm {
    suggestion_id: i64,
    action: String,
}

pub async fn resolve_room_suggestion(
    State(state): State<SharedState>,
    curator: Curator,
    Form(form): Form<ResolveSuggestionForm>,
) -> Result<Redirect, AdminError> {
    let approve = form.action == "approve";
    if suggestions::resolve_room(&state.pool, form.suggestion_id, approve, &curator.did).await? {
        tracing::info!(curator = %curator.did, id = form.suggestion_id, approve, "room suggestion resolved");
    }
    Ok(Redirect::to("/admin"))
}

pub async fn resolve_suggestion(
    State(state): State<SharedState>,
    curator: Curator,
    Form(form): Form<ResolveSuggestionForm>,
) -> Result<Redirect, AdminError> {
    let approve = form.action == "approve";
    let resolved =
        suggestions::resolve(&state.pool, form.suggestion_id, approve, &curator.did).await?;
    if let Some(rkey) = resolved {
        tracing::info!(curator = %curator.did, %rkey, id = form.suggestion_id, approve, "suggestion resolved");
    }
    Ok(Redirect::to("/admin"))
}

fn display_name(curator: &Curator) -> String {
    if curator.handle.is_empty() {
        curator.did.clone()
    } else {
        format!("@{}", curator.handle)
    }
}

// ---- the catalogue (batch removal) ----

/// The full specimen list with batch-removal controls.
pub async fn specimens_page(
    State(state): State<SharedState>,
    curator: Curator,
) -> Result<Markup, AdminError> {
    // A full Ctx (not just the catalog) so thumbnails resolve through the
    // exact same path the public grids use — CDN vs local, video poster vs
    // image thumbnail — instead of reinventing the URL logic here.
    let ctx = state.ctx().await?;
    Ok(admin_base(
        "Specimens",
        html! {
            p .signed-in {
                "signed in as " strong { (display_name(&curator)) }
                " · " a href="/admin" { "← the desk" }
            }

            section {
                h2 .room-label { "All Specimens" }
                p .room-sublabel {
                    "check any specimen to take it down from the gallery — removed "
                    "specimens disappear from every public surface and survive re-ingest. "
                    "Click a thumbnail to toggle it; open its label to inspect the specimen."
                }
                form method="post" action="/admin/specimens/batch-remove" .admin-form {
                    div .audit-grid {
                        @for s in ctx.catalog.archive.all().iter().rev() {
                            label .audit-card {
                                input type="checkbox" name="rkeys" value=(s.rkey);
                                img .audit-thumb src=(ctx.thumb(s)) alt=(s.label()) loading="lazy";
                                span .audit-label { (s.label()) }
                                span .admin-date {
                                    (s.date)
                                    " · " a href=(format!("/specimen/{}", s.rkey)) target="_blank" rel="noopener" { "open ↗" }
                                }
                            }
                        }
                    }
                    button type="submit" { "remove selected from gallery" }
                }
            }
        },
    ))
}

// ---- mutations ----

#[derive(serde::Deserialize)]
pub struct ThreadRoomAddForm {
    url: String,
}

pub async fn add_thread_room(
    State(state): State<SharedState>,
    curator: Curator,
    Form(form): Form<ThreadRoomAddForm>,
) -> Result<Response, AdminError> {
    let Some((author, rkey)) = parse_thread_url(form.url.trim()) else {
        return Ok((
            axum::http::StatusCode::BAD_REQUEST,
            "That doesn't look like a Bluesky post URL.",
        )
            .into_response());
    };

    let catalog = db::load_catalog(&state.pool).await?;
    let artist = &catalog.editorial.artist;
    let Some(room) = state
        .threads
        .fetch(&author, &rkey, &artist.did, &artist.handle)
        .await?
    else {
        return Ok((
            axum::http::StatusCode::BAD_REQUEST,
            "Couldn't fetch that thread from Bluesky.",
        )
            .into_response());
    };

    sqlx::query!(
        "INSERT INTO thread_rooms (author_did, rkey, author_handle, title, added_by)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (author_did, rkey)
         DO UPDATE SET author_handle = EXCLUDED.author_handle, title = EXCLUDED.title",
        room.author_did,
        room.rkey,
        room.author_handle,
        room.title,
        curator.did,
    )
    .execute(&state.pool)
    .await?;
    tracing::info!(curator = %curator.did, author = %room.author_handle, rkey = %room.rkey, "thread room hung");
    Ok(Redirect::to("/admin").into_response())
}

#[derive(serde::Deserialize)]
pub struct ThreadRoomRemoveForm {
    author_did: String,
    rkey: String,
}

pub async fn remove_thread_room(
    State(state): State<SharedState>,
    curator: Curator,
    Form(form): Form<ThreadRoomRemoveForm>,
) -> Result<Redirect, AdminError> {
    sqlx::query!(
        "DELETE FROM thread_rooms WHERE author_did = $1 AND rkey = $2",
        form.author_did,
        form.rkey,
    )
    .execute(&state.pool)
    .await?;
    tracing::info!(curator = %curator.did, rkey = %form.rkey, "thread room taken down");
    Ok(Redirect::to("/admin"))
}

#[derive(serde::Deserialize)]
pub struct TagForm {
    rkey: String,
    tag: String,
    #[serde(default)]
    kind: Option<String>,
}

pub async fn add_tag(
    State(state): State<SharedState>,
    curator: Curator,
    Form(form): Form<TagForm>,
) -> Result<Response, AdminError> {
    // Comma-separated input adds several tags in one submit
    // (e.g. "3d, koosh, living metal").
    let tags: Vec<String> = form
        .tag
        .split(',')
        .map(crate::catalog::slugify)
        .filter(|t| !t.is_empty())
        .collect();
    if tags.is_empty() {
        return Ok((
            axum::http::StatusCode::BAD_REQUEST,
            "A tag needs at least one letter.",
        )
            .into_response());
    }
    let kind = match form.kind.as_deref() {
        Some("lineage") => "lineage",
        _ => "tag",
    };
    for tag in &tags {
        sqlx::query!(
            "INSERT INTO specimen_tags (rkey, tag, kind, source, added_by)
             VALUES ($1, $2, $3, 'curator', $4)
             ON CONFLICT (rkey, tag) DO UPDATE SET kind = EXCLUDED.kind",
            form.rkey,
            tag,
            kind,
            curator.did,
        )
        .execute(&state.pool)
        .await?;
    }
    tracing::info!(curator = %curator.did, rkey = %form.rkey, ?tags, kind, "specimen tagged");
    Ok(Redirect::to(&format!("/specimen/{}", form.rkey)).into_response())
}

pub async fn remove_tag(
    State(state): State<SharedState>,
    curator: Curator,
    Form(form): Form<TagForm>,
) -> Result<Redirect, AdminError> {
    sqlx::query!(
        "DELETE FROM specimen_tags WHERE rkey = $1 AND tag = $2",
        form.rkey,
        form.tag,
    )
    .execute(&state.pool)
    .await?;
    tracing::info!(curator = %curator.did, rkey = %form.rkey, tag = %form.tag, "tag removed");
    Ok(Redirect::to(&format!("/specimen/{}", form.rkey)))
}

// ---- specimen removal ----

#[derive(serde::Deserialize)]
pub struct RemoveSpecimenForm {
    rkey: String,
}

pub async fn remove_specimen(
    State(state): State<SharedState>,
    curator: Curator,
    Form(form): Form<RemoveSpecimenForm>,
) -> Result<Redirect, AdminError> {
    sqlx::query!(
        "UPDATE specimens SET removed_at = now(), removed_by = $2
         WHERE rkey = $1 AND removed_at IS NULL",
        form.rkey,
        curator.did,
    )
    .execute(&state.pool)
    .await?;
    tracing::info!(curator = %curator.did, rkey = %form.rkey, "specimen removed from gallery");
    // Redirect to /admin (not /specimen/{rkey}) because the specimen page
    // now 404s — it's been filtered out of the catalog by load_catalog.
    Ok(Redirect::to("/admin"))
}

#[derive(serde::Deserialize)]
pub struct BatchRemoveForm {
    // #[serde(default)] ensures a zero-checkbox submit deserializes to an
    // empty Vec instead of erroring on the missing field.
    #[serde(default)]
    rkeys: Vec<String>,
}

pub async fn batch_remove_specimens(
    State(state): State<SharedState>,
    curator: Curator,
    HtmlForm(form): HtmlForm<BatchRemoveForm>,
) -> Result<Redirect, AdminError> {
    if form.rkeys.is_empty() {
        return Ok(Redirect::to("/admin/specimens"));
    }
    sqlx::query!(
        "UPDATE specimens SET removed_at = now(), removed_by = $2
         WHERE rkey = ANY($1) AND removed_at IS NULL",
        &form.rkeys,
        curator.did,
    )
    .execute(&state.pool)
    .await?;
    tracing::info!(curator = %curator.did, rkeys = ?form.rkeys, "specimens batch-removed from gallery");
    Ok(Redirect::to("/admin/specimens"))
}

#[derive(serde::Deserialize)]
pub struct RestoreSpecimenForm {
    rkey: String,
}

pub async fn restore_specimen(
    State(state): State<SharedState>,
    curator: Curator,
    Form(form): Form<RestoreSpecimenForm>,
) -> Result<Redirect, AdminError> {
    // Restore must explicitly clear BOTH removed_at AND removed_by — fields
    // not listed in SET retain their current values, so clearing only
    // removed_at would leave stale removed_by data.
    sqlx::query!(
        "UPDATE specimens SET removed_at = NULL, removed_by = NULL
         WHERE rkey = $1 AND removed_at IS NOT NULL",
        form.rkey,
    )
    .execute(&state.pool)
    .await?;
    tracing::info!(curator = %curator.did, rkey = %form.rkey, "specimen restored to gallery");
    Ok(Redirect::to("/admin"))
}

/// Accepts bsky.app post URLs or at-uris; returns (author, rkey).
pub fn parse_thread_url(url: &str) -> Option<(String, String)> {
    if let Some(rest) = url.strip_prefix("at://") {
        let mut parts = rest.splitn(3, '/');
        let (author, collection, rkey) = (parts.next()?, parts.next()?, parts.next()?);
        return (collection == "app.bsky.feed.post")
            .then(|| (author.to_string(), rkey.to_string()));
    }
    let rest = url
        .strip_prefix("https://bsky.app/profile/")
        .or_else(|| url.strip_prefix("http://bsky.app/profile/"))?;
    let (author, rest) = rest.split_once("/post/")?;
    let rkey = rest.split(['?', '/', '#']).next()?;
    (!author.is_empty() && !rkey.is_empty()).then(|| (author.to_string(), rkey.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_thread_url_accepts_web_and_at_uris() {
        assert_eq!(
            parse_thread_url(
                "https://bsky.app/profile/norvid-studies.bsky.social/post/3mpli4fvzns22"
            ),
            Some(("norvid-studies.bsky.social".into(), "3mpli4fvzns22".into()))
        );
        assert_eq!(
            parse_thread_url("at://did:plc:abc/app.bsky.feed.post/3xyz"),
            Some(("did:plc:abc".into(), "3xyz".into()))
        );
        assert_eq!(parse_thread_url("https://example.com/nope"), None);
    }
}
