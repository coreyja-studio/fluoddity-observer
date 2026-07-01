//! The curator's desk: admin portal for managing rooms and hanging
//! specimens. Identity via Bluesky OAuth (see auth.rs); authorization via
//! the curators roster.

use atrium_oauth::{AuthorizeOptions, CallbackParams, KnownScope, Scope};
use axum::{
    Form,
    extract::{Path, Query, State},
    response::{IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use maud::{DOCTYPE, Markup, html};

use crate::{
    SharedState,
    auth::{self, Curator, SESSION_COOKIE},
    catalog::Catalog,
    db,
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
    let catalog = db::load_catalog(&state.pool).await?;
    let guest_rooms = db::guest_rooms(&state.pool).await?;
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
                @for room in &catalog.editorial.rooms {
                    div .admin-room {
                        form method="post" action=(format!("/admin/room/{}/update", room.slug)) .admin-form .room-edit {
                            input type="text" name="title" value=(room.title) required;
                            input type="text" name="description" value=(room.description);
                            button type="submit" { "save" }
                        }
                        div .admin-room-specimens {
                            @for s in catalog.room_specimens(room) {
                                div .admin-specimen {
                                    a href=(format!("/specimen/{}", s.rkey)) { (s.label()) }
                                    form method="post" action="/admin/unhang" .inline-form {
                                        input type="hidden" name="room_slug" value=(room.slug);
                                        input type="hidden" name="rkey" value=(s.rkey);
                                        button type="submit" .link-button title="take down" { "✕" }
                                    }
                                }
                            }
                        }
                    }
                }
                form method="post" action="/admin/rooms/create" .admin-form .room-create {
                    input type="text" name="title" placeholder="New room title" required;
                    input type="text" name="description" placeholder="description (vibe, in the artist's words)";
                    button type="submit" { "open a new room" }
                }
            }

            section {
                h2 .room-label { "Guest Rooms" }
                p .room-sublabel {
                    "a guest room is a Bluesky thread — paste the head post's URL to hang it "
                    "on the front page; it renders live from the thread"
                }
                (guest_rooms_section(&guest_rooms))
            }

            section {
                h2 .room-label { "Not Yet Hung" }
                p .room-sublabel { "archive specimens awaiting a room — newest first" }
                (unclassified_list(&catalog))
            }
        },
    ))
}

fn display_name(curator: &Curator) -> String {
    if curator.handle.is_empty() {
        curator.did.clone()
    } else {
        format!("@{}", curator.handle)
    }
}

fn unclassified_list(catalog: &Catalog) -> Markup {
    let unhung: Vec<_> = catalog
        .archive
        .all()
        .iter()
        .rev()
        .filter(|s| catalog.room_of(&s.rkey).is_none())
        .collect();
    html! {
        p .room-sublabel { (unhung.len()) " unclassified" }
        div .unhung-list {
            @for s in unhung.iter().take(60) {
                div .admin-specimen {
                    a href=(format!("/specimen/{}", s.rkey)) { (s.label()) }
                    span .admin-date { (s.date) }
                    form method="post" action="/admin/hang" .inline-form {
                        input type="hidden" name="rkey" value=(s.rkey);
                        select name="room_slug" {
                            @for room in &catalog.editorial.rooms {
                                option value=(room.slug) { (room.title) }
                            }
                        }
                        button type="submit" { "hang" }
                    }
                }
            }
            @if unhung.len() > 60 {
                p .room-sublabel { "… and " (unhung.len() - 60) " more, older" }
            }
        }
    }
}

fn guest_rooms_section(guest_rooms: &[crate::db::GuestRoomRow]) -> Markup {
    html! {
        @for gr in guest_rooms {
            div .admin-specimen {
                a href=(format!("/guest/{}/{}", gr.author_handle, gr.rkey)) { (gr.title) }
                span .admin-date { "@" (gr.author_handle) }
                form method="post" action="/admin/guest-rooms/remove" .inline-form {
                    input type="hidden" name="author_did" value=(gr.author_did);
                    input type="hidden" name="rkey" value=(gr.rkey);
                    button type="submit" .link-button title="take down" { "✕" }
                }
            }
        }
        form method="post" action="/admin/guest-rooms/add" .admin-form .room-create {
            input type="text" name="url" placeholder="https://bsky.app/profile/curator/post/…" required;
            button type="submit" { "hang this thread" }
        }
    }
}

// ---- mutations ----

#[derive(serde::Deserialize)]
pub struct HangForm {
    rkey: String,
    room_slug: String,
}

pub async fn hang(
    State(state): State<SharedState>,
    curator: Curator,
    Form(form): Form<HangForm>,
) -> Result<Redirect, AdminError> {
    let result = sqlx::query!(
        "INSERT INTO room_specimens (room_slug, rkey, position)
         SELECT $1, $2, COALESCE(MAX(position) + 1, 0)
         FROM room_specimens WHERE room_slug = $1
         ON CONFLICT (room_slug, rkey) DO NOTHING",
        form.room_slug,
        form.rkey,
    )
    .execute(&state.pool)
    .await?;
    if result.rows_affected() > 0 {
        tracing::info!(curator = %curator.did, rkey = %form.rkey, room = %form.room_slug, "specimen hung");
    }
    Ok(Redirect::to("/admin"))
}

pub async fn unhang(
    State(state): State<SharedState>,
    curator: Curator,
    Form(form): Form<HangForm>,
) -> Result<Redirect, AdminError> {
    sqlx::query!(
        "DELETE FROM room_specimens WHERE room_slug = $1 AND rkey = $2",
        form.room_slug,
        form.rkey,
    )
    .execute(&state.pool)
    .await?;
    tracing::info!(curator = %curator.did, rkey = %form.rkey, room = %form.room_slug, "specimen taken down");
    Ok(Redirect::to("/admin"))
}

#[derive(serde::Deserialize)]
pub struct RoomForm {
    title: String,
    #[serde(default)]
    description: String,
}

pub async fn create_room(
    State(state): State<SharedState>,
    curator: Curator,
    Form(form): Form<RoomForm>,
) -> Result<Redirect, AdminError> {
    let slug = slugify(&form.title);
    if slug.is_empty() {
        return Err(anyhow::anyhow!("room title must contain letters").into());
    }
    sqlx::query!(
        "INSERT INTO rooms (slug, title, description, position)
         SELECT $1, $2, $3, COALESCE(MAX(position) + 1, 0) FROM rooms
         ON CONFLICT (slug) DO NOTHING",
        slug,
        form.title.trim(),
        form.description.trim(),
    )
    .execute(&state.pool)
    .await?;
    tracing::info!(curator = %curator.did, %slug, "room opened");
    Ok(Redirect::to("/admin"))
}

pub async fn update_room(
    State(state): State<SharedState>,
    curator: Curator,
    Path(slug): Path<String>,
    Form(form): Form<RoomForm>,
) -> Result<Redirect, AdminError> {
    sqlx::query!(
        "UPDATE rooms SET title = $2, description = $3 WHERE slug = $1",
        slug,
        form.title.trim(),
        form.description.trim(),
    )
    .execute(&state.pool)
    .await?;
    tracing::info!(curator = %curator.did, %slug, "room updated");
    Ok(Redirect::to("/admin"))
}

#[derive(serde::Deserialize)]
pub struct GuestRoomAddForm {
    url: String,
}

pub async fn add_guest_room(
    State(state): State<SharedState>,
    curator: Curator,
    Form(form): Form<GuestRoomAddForm>,
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
        "INSERT INTO guest_rooms (author_did, rkey, author_handle, title, added_by)
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
    tracing::info!(curator = %curator.did, author = %room.author_handle, rkey = %room.rkey, "guest room hung");
    Ok(Redirect::to("/admin").into_response())
}

#[derive(serde::Deserialize)]
pub struct GuestRoomRemoveForm {
    author_did: String,
    rkey: String,
}

pub async fn remove_guest_room(
    State(state): State<SharedState>,
    curator: Curator,
    Form(form): Form<GuestRoomRemoveForm>,
) -> Result<Redirect, AdminError> {
    sqlx::query!(
        "DELETE FROM guest_rooms WHERE author_did = $1 AND rkey = $2",
        form.author_did,
        form.rkey,
    )
    .execute(&state.pool)
    .await?;
    tracing::info!(curator = %curator.did, rkey = %form.rkey, "guest room taken down");
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

pub fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for c in title.trim().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_end_matches('-').to_string()
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

    #[test]
    fn slugify_handles_punctuation_and_case() {
        assert_eq!(slugify("The Koosh & the Velvet"), "the-koosh-the-velvet");
        assert_eq!(slugify("  Pools!  "), "pools");
        assert_eq!(slugify("!!!"), "");
    }
}
