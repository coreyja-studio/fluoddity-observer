//! The whole gallery is one binary: this file owns the CLI subcommands
//! (`serve` is the default; `import`, `ingest-once`, `bot-once`, … are
//! operator tools), the shared app state, and the router. Page rendering
//! lives in `views.rs`; background work is registered in `cron.rs`.

mod admin;
mod auth;
mod backup;
mod bot;
mod catalog;
mod cron;
mod db;
mod dimensions;
mod feed;
mod ingest;
mod jobs;
mod margin_notes;
mod storage;
mod suggestions;
mod threads;
mod views;

use axum::{
    Router,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use catalog::Catalog;
use sqlx::PgPool;
use std::sync::Arc as StdArc;
use tower_http::services::ServeDir;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaMode {
    /// Serve mp4s from the local archive directory under /media, falling
    /// back to the Bluesky CDN for specimens without a local file.
    Local,
    /// Point everything at Bluesky's video CDN (HLS) — no local media needed.
    Cdn,
}

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub media_mode: MediaMode,
    /// Pull-zone base URL in front of the vault; `None` = Bluesky CDN only.
    pub media_base: Option<String>,
    /// Vault upload credentials — the masters upload needs them; serving
    /// doesn't.
    pub bunny: Option<StdArc<storage::BunnyConfig>>,
    pub http: reqwest::Client,
    pub oauth: StdArc<auth::AtriumOAuthClient>,
    pub oauth_mode: StdArc<auth::OauthMode>,
    pub threads: StdArc<threads::ThreadFetcher>,
    pub cookie_key: cja::server::cookies::CookieKey,
}

impl cja::app_state::AppState for AppState {
    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn db(&self) -> &PgPool {
        &self.pool
    }

    fn cookie_key(&self) -> &cja::server::cookies::CookieKey {
        &self.cookie_key
    }
}

type SharedState = AppState;

/// Per-request rendering context: a fresh catalog snapshot plus media config.
pub struct Ctx {
    pub catalog: Catalog,
    pub media_mode: MediaMode,
    pub media_base: Option<String>,
}

impl Ctx {
    /// The best archival copy of a video, served from the vault's pull
    /// zone: the artist's master if we hold one, else the PDS original.
    /// `None` means the Bluesky CDN re-encode is all there is. Grids keep
    /// the CDN source regardless — a room autoplays a dozen loops at
    /// thumbnail size; behold, specimen pages, and ambient earn this one.
    pub fn full_video_src(&self, s: &catalog::Specimen) -> Option<String> {
        let base = self.media_base.as_deref()?;
        let key = s.master_key.as_deref().or(s.pds_key.as_deref())?;
        Some(format!("{base}/{key}"))
    }

    /// (video src, hls url for hls.js, poster url)
    pub fn video_sources(&self, s: &catalog::Specimen) -> (String, Option<String>, String) {
        let local = match self.media_mode {
            MediaMode::Local => s.file.as_deref(),
            MediaMode::Cdn => None,
        };
        match local {
            Some(file) => (
                format!("/media/{file}"),
                None,
                format!("/media/posters/{}.jpg", s.rkey),
            ),
            None => {
                let did = &self.catalog.editorial.artist.did;
                let playlist =
                    format!("https://video.bsky.app/watch/{did}/{}/playlist.m3u8", s.cid);
                let poster = format!("https://video.bsky.app/watch/{did}/{}/thumbnail.jpg", s.cid);
                (playlist.clone(), Some(playlist), poster)
            }
        }
    }

    fn image_cdn(&self, cid: &str, variant: &str) -> String {
        let did = &self.catalog.editorial.artist.did;
        format!("https://cdn.bsky.app/img/{variant}/plain/{did}/{cid}@jpeg")
    }

    /// Full-quality src for one image of an image specimen.
    pub fn image_src(&self, img: &catalog::SpecimenImage) -> String {
        let local = match self.media_mode {
            MediaMode::Local => img.file.as_deref(),
            MediaMode::Cdn => None,
        };
        match local {
            Some(file) => format!("/media/{file}"),
            None => self.image_cdn(&img.cid, "feed_fullsize"),
        }
    }

    /// Grid thumbnail for any specimen, whatever its kind.
    pub fn thumb(&self, s: &catalog::Specimen) -> String {
        match s.kind {
            catalog::MediaKind::Video => self.video_sources(s).2,
            catalog::MediaKind::Image => {
                let local = match self.media_mode {
                    MediaMode::Local => s.file.as_deref(),
                    MediaMode::Cdn => None,
                };
                match local {
                    Some(file) => format!("/media/{file}"),
                    // The first image's cid is mirrored onto the specimen.
                    None => self.image_cdn(&s.cid, "feed_thumbnail"),
                }
            }
        }
    }
}

/// A registered thread room joined with its live thread data.
pub struct HungRoom {
    pub row: db::ThreadRoomRow,
    pub room: StdArc<threads::ThreadRoom>,
}

impl HungRoom {
    pub fn is_by(&self, did: &str) -> bool {
        self.row.author_did == did
    }
}

impl AppState {
    /// All registered thread rooms with live (cached) thread data. Rooms
    /// whose threads can't be fetched right now are skipped with a warning.
    async fn registered_rooms(&self, ctx: &Ctx) -> Vec<HungRoom> {
        let artist = &ctx.catalog.editorial.artist;
        let rows = match db::thread_rooms(&self.pool).await {
            Ok(rows) => rows,
            Err(err) => {
                tracing::error!(?err, "loading thread room registry failed");
                return Vec::new();
            }
        };
        let mut hung = Vec::new();
        for row in rows {
            match self
                .threads
                .fetch(&row.author_did, &row.rkey, &artist.did, &artist.handle)
                .await
            {
                Ok(Some(room)) => hung.push(HungRoom { row, room }),
                Ok(None) => {
                    tracing::warn!(rkey = %row.rkey, "registered thread room no longer resolves");
                }
                Err(err) => {
                    tracing::warn!(?err, rkey = %row.rkey, "thread room fetch failed");
                }
            }
        }
        hung
    }

    async fn ctx(&self) -> anyhow::Result<Ctx> {
        Ok(Ctx {
            catalog: db::load_catalog(&self.pool).await?,
            media_mode: self.media_mode,
            media_base: self.media_base.clone(),
        })
    }
}

/// Anyhow-backed handler error that renders as a plain 500.
struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!(error = ?self.0, "request failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "The guide's pages are stuck together. Try again shortly.",
        )
            .into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

fn media_dir() -> String {
    std::env::var("PCG_MEDIA_DIR")
        .unwrap_or_else(|_| "/home/coreyja.linux/paperclips-media/oops".to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        // Logs on stderr; stdout stays clean for subcommand output
        // (gen-oauth-key prints the key).
        .with_writer(std::io::stderr)
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .map_err(|_| anyhow::anyhow!("DATABASE_URL must be set (see .mise.toml)"))?;
    let pool = PgPool::connect(&database_url).await?;
    sqlx::migrate!().run(&pool).await?;

    match std::env::args().nth(1).as_deref() {
        None | Some("serve") => serve(pool).await,
        Some("import") => import(pool).await,
        Some("ingest-once") => ingest_once(pool).await,
        Some("pull-media") => pull_media(pool).await,
        Some("refresh-notes") => refresh_notes(pool).await,
        Some("harvest-once") => harvest_once(pool).await,
        Some("classify-dimensions") => classify_dimensions(pool).await,
        Some("gen-posters") => gen_posters(pool).await,
        Some("bot-once") => bot_once(pool).await,
        Some("bot-weekly") => bot_weekly(pool).await,
        Some("gen-oauth-key") => {
            println!("{}", auth::generate_private_key()?);
            Ok(())
        }
        Some(other) => anyhow::bail!(
            "unknown subcommand {other:?} — expected `serve`, `import`, `ingest-once`, `pull-media`, `refresh-notes`, `harvest-once`, `classify-dimensions`, `gen-posters`, `bot-once`, `bot-weekly`, or `gen-oauth-key`"
        ),
    }
}

/// One manual poll of the artist's feed — useful for cron or debugging.
async fn ingest_once(pool: PgPool) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .user_agent("paperclips-gallery/0.1 (fluoddity field guide)")
        .build()?;
    let added = ingest::poll_once(&pool, &client).await?;
    tracing::info!(count = added.len(), rkeys = ?added, "ingest-once complete");
    Ok(())
}

/// Cold storage: download original blobs the local archive is missing
/// (live-ingested specimens) from the artist's PDS. Run where the media
/// dir lives — the Fly machines have no volume.
async fn pull_media(pool: PgPool) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .user_agent("paperclips-gallery/0.1 (fluoddity field guide)")
        .build()?;
    let bunny = storage::BunnyConfig::from_env();
    if bunny.is_none() {
        tracing::info!("vault sync disabled — PCG_BUNNY_STORAGE_ZONE/KEY unset");
    }
    let stats = backup::pull_media(&pool, &client, &media_dir(), bunny.as_ref()).await?;
    tracing::info!(
        pulled = stats.pulled,
        synced = stats.synced,
        failed = stats.failed,
        "pull-media complete"
    );
    if stats.failed > 0 {
        anyhow::bail!(
            "{} blob(s) failed to pull or sync — rerun to retry",
            stats.failed
        );
    }
    Ok(())
}

/// Seed the database from the flat-file era (metadata.jsonl + catalog.json).
async fn import(pool: PgPool) -> anyhow::Result<()> {
    let metadata_path =
        std::env::var("PCG_METADATA").unwrap_or_else(|_| format!("{}/metadata.jsonl", media_dir()));
    let catalog_path = std::env::var("PCG_CATALOG").unwrap_or_else(|_| "catalog.json".to_string());
    let stats = db::import(&pool, &metadata_path, &catalog_path).await?;
    tracing::info!(
        specimens = stats.specimens,
        lineage_tags = stats.lineage_tags,
        margin_notes = stats.margin_notes,
        "import complete"
    );
    Ok(())
}

/// First-pass 2d/3d tagging of untagged specimens (idempotent; never
/// overrides curator corrections).
async fn classify_dimensions(pool: PgPool) -> anyhow::Result<()> {
    let added = dimensions::classify_archive(&pool).await?;
    tracing::info!(added, "dimension classification complete");
    Ok(())
}

/// Generate 1200x630 OG poster JPEGs for all video specimens, using ffmpeg
/// to extract a mid-clip frame. Uploads to the Bunny vault under og/{rkey}.jpg
/// and records the key. Run on the local VM (where ffmpeg + media files live).
/// Pass `--force` to regenerate all posters (default: only specimens without
/// og_poster_key).
async fn gen_posters(pool: PgPool) -> anyhow::Result<()> {
    let force = std::env::args().any(|a| a == "--force" || a == "-f");
    let media_dir = media_dir();
    let og_dir = format!("{media_dir}/og");
    tokio::fs::create_dir_all(&og_dir).await?;

    // Bunny vault: file uploads use the zone-scoped password (NOT the account
    // API key). BunnyConfig::from_env() reads PCG_BUNNY_STORAGE_ZONE/KEY.
    let client = reqwest::Client::builder()
        .user_agent("paperclips-gallery/0.1 (fluoddity field guide)")
        .build()?;
    let bunny = storage::BunnyConfig::from_env();
    if bunny.is_none() {
        tracing::info!("vault upload disabled — PCG_BUNNY_STORAGE_ZONE/KEY unset");
    }

    let meta = sqlx::query!("SELECT artist_did FROM gallery_meta")
        .fetch_one(&pool)
        .await?;

    // A single query with a `force` bind avoids the `sqlx::query!` macro
    // generating two distinct anonymous `Record` types (one per call site,
    // which won't unify across an if/else).
    let rows = sqlx::query!(
        "SELECT rkey, cid, file, og_poster_key FROM specimens
         WHERE kind = 'video' AND ($1 OR og_poster_key IS NULL)
         ORDER BY collected_on, rkey",
        force,
    )
    .fetch_all(&pool)
    .await?;
    tracing::info!(count = rows.len(), force, "specimens to process");

    let mut generated = 0usize;
    let mut uploaded = 0usize;
    let mut failed = 0usize;

    for row in &rows {
        let rkey = &row.rkey;
        let poster_path = format!("{og_dir}/{rkey}.jpg");

        let hls = format!(
            "https://video.bsky.app/watch/{}/{}/playlist.m3u8",
            meta.artist_did, row.cid
        );
        let source = match &row.file {
            Some(file) => {
                let local = format!("{media_dir}/{file}");
                if std::path::Path::new(&local).exists() {
                    local
                } else {
                    tracing::info!(%rkey, "local file missing, falling back to HLS");
                    hls
                }
            }
            None => hls,
        };

        let duration = ffprobe_duration(&source).await.unwrap_or(2.0);
        let midpoint = (duration / 2.0).max(0.5);

        // .kill_on_drop(true) prevents orphaned ffmpeg processes on cancel/panic.
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::process::Command::new("ffmpeg")
                .kill_on_drop(true)
                .arg("-y")
                .arg("-ss")
                .arg(format!("{midpoint:.1}"))
                .arg("-i")
                .arg(&source)
                .arg("-frames:v")
                .arg("1")
                .arg("-vf")
                .arg("scale=1200:630:force_original_aspect_ratio=increase,crop=1200:630")
                .arg("-q:v")
                .arg("2")
                .arg(&poster_path)
                .output(),
        )
        .await;

        match output {
            Ok(Ok(o)) if o.status.success() => {
                generated += 1;
                let vault_key = format!("og/{rkey}.jpg");
                if let Some(bunny) = &bunny {
                    match bunny
                        .put_file(&client, &vault_key, std::path::Path::new(&poster_path))
                        .await
                    {
                        Ok(()) => {
                            sqlx::query!(
                                "UPDATE specimens SET og_poster_key = $1 WHERE rkey = $2",
                                vault_key,
                                rkey,
                            )
                            .execute(&pool)
                            .await?;
                            uploaded += 1;
                        }
                        Err(err) => {
                            tracing::warn!(?err, %rkey, "vault upload failed");
                            failed += 1;
                        }
                    }
                } else {
                    // No vault: still record that a 1200x630 poster was generated.
                    sqlx::query!(
                        "UPDATE specimens SET og_poster_key = $1 WHERE rkey = $2",
                        vault_key,
                        rkey,
                    )
                    .execute(&pool)
                    .await?;
                }
            }
            Ok(Ok(o)) => {
                tracing::warn!(
                    %rkey,
                    stderr = %String::from_utf8_lossy(&o.stderr),
                    "ffmpeg failed"
                );
                failed += 1;
            }
            Ok(Err(err)) => {
                tracing::warn!(?err, %rkey, "ffmpeg spawn failed");
                failed += 1;
            }
            Err(_) => {
                tracing::warn!(%rkey, "ffmpeg timed out after 30s");
                failed += 1;
            }
        }
    }

    tracing::info!(generated, uploaded, failed, "gen-posters complete");
    if failed > 0 {
        anyhow::bail!("{failed} poster(s) failed — rerun to retry");
    }
    Ok(())
}

/// Probe video duration in seconds via ffprobe.
async fn ffprobe_duration(source: &str) -> anyhow::Result<f64> {
    // .output() drains both stdout and stderr fully — no pipe-buffer deadlock.
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        tokio::process::Command::new("ffprobe")
            .kill_on_drop(true)
            .arg("-v")
            .arg("error")
            .arg("-show_entries")
            .arg("format=duration")
            .arg("-of")
            .arg("csv=p=0")
            .arg(source)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("ffprobe timed out after 15s"))??;
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .trim()
        .parse::<f64>()
        .map_err(|e| anyhow::anyhow!("parsing ffprobe duration: {e}"))
}

/// One manual suggestion harvest across the whole archive.
async fn harvest_once(pool: PgPool) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .user_agent("paperclips-gallery/0.1 (fluoddity field guide)")
        .build()?;
    let stats = suggestions::harvest_once(&pool, &client).await?;
    tracing::info!(
        suggested = stats.suggested,
        artist_tagged = stats.artist_tagged,
        "harvest-once complete"
    );
    Ok(())
}

/// One manual margin-note refresh across the whole archive.
async fn refresh_notes(pool: PgPool) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .user_agent("paperclips-gallery/0.1 (fluoddity field guide)")
        .build()?;
    let added = margin_notes::refresh_once(&pool, &client).await?;
    tracing::info!(added, "margin note refresh complete");
    Ok(())
}

/// One manual bot poll — answer pending mentions and exit.
async fn bot_once(pool: PgPool) -> anyhow::Result<()> {
    let cfg = bot::BotConfig::from_env()
        .ok_or_else(|| anyhow::anyhow!("PCG_BOT_HANDLE and PCG_BOT_PASSWORD must be set"))?;
    let client = reqwest::Client::builder()
        .user_agent("paperclips-gallery-bot/0.1 (fluoddity field guide)")
        .build()?;
    let threads = threads::ThreadFetcher::new(client.clone());
    let replied = bot::poll_once(&pool, &client, &threads, &cfg).await?;
    tracing::info!(replied, "bot-once complete");
    Ok(())
}

/// One manual weekly wrap-up check. PCG_BOT_DRY_RUN=1 composes and logs
/// without posting (and works without bot credentials).
async fn bot_weekly(pool: PgPool) -> anyhow::Result<()> {
    let dry_run = std::env::var("PCG_BOT_DRY_RUN").is_ok();
    let cfg = if dry_run {
        bot::BotConfig::from_env().unwrap_or_else(|| bot::BotConfig {
            handle: String::new(),
            password: String::new(),
            pds: "https://bsky.social".to_string(),
            public_url: bot::public_url(),
        })
    } else {
        bot::BotConfig::from_env()
            .ok_or_else(|| anyhow::anyhow!("PCG_BOT_HANDLE and PCG_BOT_PASSWORD must be set"))?
    };
    let client = reqwest::Client::builder()
        .user_agent("paperclips-gallery-bot/0.1 (fluoddity field guide)")
        .build()?;
    match bot::weekly_once(&pool, &client, &cfg, dry_run).await? {
        Some(picks) => tracing::info!(picks, dry_run, "weekly wrap-up handled"),
        None => tracing::info!("weekly wrap-up: staying silent"),
    }
    Ok(())
}

async fn serve(pool: PgPool) -> anyhow::Result<()> {
    let media_mode = match std::env::var("PCG_MEDIA_MODE").as_deref() {
        Ok("cdn") => MediaMode::Cdn,
        _ => MediaMode::Local,
    };
    let port: u16 = std::env::var("PCG_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(4601);

    let catalog = db::load_catalog(&pool).await?;
    tracing::info!(
        archive = catalog.archive.len(),
        tagged = catalog.editorial.tags.len(),
        ?media_mode,
        "catalog loaded from database"
    );

    auth::seed_curators(&pool).await?;
    let oauth_mode = auth::OauthMode::from_env(port)?;
    tracing::info!(
        confidential = oauth_mode.is_confidential(),
        "oauth client mode"
    );
    let oauth = auth::build_oauth_client(pool.clone(), &oauth_mode)?;

    let http = reqwest::Client::builder()
        .user_agent("paperclips-gallery/0.1 (fluoddity field guide)")
        .build()?;
    let media_base = storage::media_base_from_env();
    let bunny = storage::BunnyConfig::from_env().map(StdArc::new);
    tracing::info!(
        media_base = media_base.as_deref().unwrap_or("(bluesky cdn only)"),
        vault_uploads = bunny.is_some(),
        "media vault"
    );
    let state: SharedState = AppState {
        pool,
        media_mode,
        media_base,
        bunny,
        http: http.clone(),
        oauth: StdArc::new(oauth),
        oauth_mode: StdArc::new(oauth_mode),
        threads: StdArc::new(threads::ThreadFetcher::new(http)),
        cookie_key: cja::server::cookies::CookieKey::from_env_or_generate()
            .map_err(|e| anyhow::anyhow!("cookie key: {e}"))?,
    };

    // cja background workers: the job worker drains the durable queue, the
    // cron worker enqueues on schedule (ingest, bot mentions, weekly
    // wrap-up — see cron.rs).
    tokio::spawn(cja::jobs::worker::job_worker(
        state.clone(),
        jobs::Jobs,
        std::time::Duration::from_secs(5),
        cja::jobs::DEFAULT_MAX_RETRIES,
        cja::jobs::CancellationToken::new(),
        cja::jobs::DEFAULT_LOCK_TIMEOUT,
    ));
    tokio::spawn(cron::run_cron(state.clone()));

    let app = Router::new()
        .route("/", get(index))
        .route("/archive", get(archive))
        .route("/search", get(search))
        .route("/feed.xml", get(atom_feed))
        .route("/sitemap.xml", get(sitemap))
        .route("/room/{author}/{rkey}", get(thread_room))
        .route("/specimen/{rkey}", get(specimen))
        .route("/tag/{tag}", get(tag_page))
        .route("/ambient", get(ambient))
        .route("/colophon", get(colophon))
        .route("/admin", get(admin::dashboard))
        .route(
            "/admin/login",
            get(admin::login_page).post(admin::login_submit),
        )
        .route("/admin/oauth/callback", get(admin::oauth_callback))
        .route("/oauth/client-metadata.json", get(oauth_client_metadata))
        .route("/oauth/jwks.json", get(oauth_jwks))
        .route("/admin/logout", axum::routing::post(admin::logout))
        .route(
            "/admin/thread-rooms/add",
            axum::routing::post(admin::add_thread_room),
        )
        .route(
            "/admin/thread-rooms/remove",
            axum::routing::post(admin::remove_thread_room),
        )
        .route(
            "/admin/suggestions/resolve",
            axum::routing::post(admin::resolve_suggestion),
        )
        .route(
            "/admin/room-suggestions/resolve",
            axum::routing::post(admin::resolve_room_suggestion),
        )
        .route("/admin/tags/add", axum::routing::post(admin::add_tag))
        .route("/admin/tags/remove", axum::routing::post(admin::remove_tag))
        .route("/admin/specimens", get(admin::specimens_page))
        .route(
            "/admin/specimens/remove",
            axum::routing::post(admin::remove_specimen),
        )
        .route(
            "/admin/specimens/batch-remove",
            axum::routing::post(admin::batch_remove_specimens),
        )
        .route(
            "/admin/specimens/restore",
            axum::routing::post(admin::restore_specimen),
        )
        .route("/admin/vault", get(admin::masters_page))
        .route(
            "/admin/vault/upload",
            axum::routing::post(admin::upload_master)
                // Source renders come straight off the render node, far past
                // the default 2 MB body cap.
                .layer(axum::extract::DefaultBodyLimit::max(
                    admin::MAX_MASTER_BYTES as usize,
                )),
        )
        .route("/static/style.css", get(stylesheet))
        .route("/static/admin.css", get(admin_css))
        .route("/static/gallery.js", get(gallery_js))
        .route("/static/ambient.js", get(ambient_js))
        .nest_service("/media", ServeDir::new(media_dir()))
        .with_state(state);

    let addr = format!("0.0.0.0:{port}");
    tracing::info!("listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

async fn index(State(state): State<SharedState>) -> Result<maud::Markup, AppError> {
    let ctx = state.ctx().await?;
    let rooms = state.registered_rooms(&ctx).await;
    Ok(views::index(&ctx, &rooms))
}

async fn thread_room(
    State(state): State<SharedState>,
    Path((author, rkey)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let ctx = state.ctx().await?;
    let artist = &ctx.catalog.editorial.artist;
    let Some(room) = state
        .threads
        .fetch(&author, &rkey, &artist.did, &artist.handle)
        .await?
    else {
        return Ok(not_found());
    };
    // Registered artist rooms get plate numerals in registry order.
    let plate = state
        .registered_rooms(&ctx)
        .await
        .iter()
        .filter(|h| h.is_by(&artist.did))
        .position(|h| h.row.rkey == room.rkey && h.row.author_did == room.author_did)
        .map(|i| i + 1);
    Ok(views::thread_room(&ctx, &room, plate).into_response())
}

async fn archive(State(state): State<SharedState>) -> Result<maud::Markup, AppError> {
    Ok(views::archive(&state.ctx().await?))
}

#[derive(serde::Deserialize)]
struct SearchParams {
    #[serde(default)]
    q: String,
}

async fn search(
    State(state): State<SharedState>,
    axum::extract::Query(params): axum::extract::Query<SearchParams>,
) -> Result<maud::Markup, AppError> {
    Ok(views::search(&state.ctx().await?, params.q.trim()))
}

#[derive(serde::Deserialize)]
struct AmbientParams {
    /// Filter to one tag/lineage.
    tag: Option<String>,
    /// Filter to one thread room: "author/rkey".
    room: Option<String>,
}

async fn ambient(
    State(state): State<SharedState>,
    axum::extract::Query(params): axum::extract::Query<AmbientParams>,
) -> Result<Response, AppError> {
    let ctx = state.ctx().await?;
    let (title, specimens): (String, Vec<&catalog::Specimen>) = if let Some(tag) = &params.tag {
        (catalog::tag_display(tag), ctx.catalog.tagged(tag))
    } else if let Some(room_ref) = &params.room {
        let Some((author, rkey)) = room_ref.split_once('/') else {
            return Ok(not_found());
        };
        let artist = &ctx.catalog.editorial.artist;
        let Some(room) = state
            .threads
            .fetch(author, rkey, &artist.did, &artist.handle)
            .await?
        else {
            return Ok(not_found());
        };
        (
            room.title.clone(),
            room.entries
                .iter()
                .filter_map(|e| ctx.catalog.archive.get(&e.specimen_rkey))
                .collect(),
        )
    } else {
        (
            "the whole collection".to_string(),
            ctx.catalog.archive.all().iter().collect(),
        )
    };
    // Stills join the projection as held slides, videos as loops.
    let entries: Vec<views::AmbientEntry> = specimens
        .iter()
        .map(|s| views::ambient_entry(&ctx, s))
        .collect();
    if entries.is_empty() {
        return Ok(not_found());
    }
    Ok(views::ambient(&title, &entries).into_response())
}

async fn tag_page(
    State(state): State<SharedState>,
    Path(tag): Path<String>,
) -> Result<Response, AppError> {
    let ctx = state.ctx().await?;
    Ok(match ctx.catalog.tag_kind(&tag) {
        Some(kind) => views::tag_page(&ctx, &tag, kind).into_response(),
        None => not_found(),
    })
}

async fn specimen(
    State(state): State<SharedState>,
    Path(rkey): Path<String>,
    curator: Option<auth::Curator>,
) -> Result<Response, AppError> {
    let ctx = state.ctx().await?;
    if ctx.catalog.archive.get(&rkey).is_none() {
        return Ok(not_found());
    }
    let rooms = state.registered_rooms(&ctx).await;
    let hung_in: Vec<&HungRoom> = rooms
        .iter()
        .filter(|h| h.room.entries.iter().any(|e| e.specimen_rkey == rkey))
        .collect();
    let specimen = ctx.catalog.archive.get(&rkey).expect("checked above");
    Ok(views::specimen(&ctx, &hung_in, specimen, curator.as_ref()).into_response())
}

async fn colophon(State(state): State<SharedState>) -> Result<maud::Markup, AppError> {
    Ok(views::colophon(&state.ctx().await?))
}

async fn atom_feed(State(state): State<SharedState>) -> Result<Response, AppError> {
    let ctx = state.ctx().await?;
    let body = feed::atom(&ctx.catalog, &bot::public_url());
    Ok((
        [(header::CONTENT_TYPE, "application/atom+xml; charset=utf-8")],
        body,
    )
        .into_response())
}

async fn sitemap(State(state): State<SharedState>) -> Result<Response, AppError> {
    let ctx = state.ctx().await?;
    let rooms = db::thread_rooms(&state.pool).await?;
    let body = feed::sitemap(&ctx.catalog, &rooms, &bot::public_url());
    Ok((
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        body,
    )
        .into_response())
}

/// Confidential-client documents; 404 in loopback mode.
async fn oauth_client_metadata(State(state): State<SharedState>) -> Response {
    match state.oauth_mode.client_metadata_doc() {
        Some(doc) => axum::Json(doc).into_response(),
        None => not_found(),
    }
}

async fn oauth_jwks(State(state): State<SharedState>) -> Response {
    match state.oauth_mode.jwks() {
        Some(jwks) => axum::Json(jwks).into_response(),
        None => not_found(),
    }
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../static/style.css"),
    )
}

async fn admin_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../static/admin.css"),
    )
}

async fn ambient_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../static/ambient.js"),
    )
}

async fn gallery_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../static/gallery.js"),
    )
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, views::not_found()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Archive, Artist, Editorial, MediaKind, Origin, Specimen};

    fn ctx(media_base: Option<&str>, specimen: Specimen) -> Ctx {
        Ctx {
            catalog: Catalog {
                archive: Archive::new(vec![specimen]),
                editorial: Editorial {
                    artist: Artist {
                        handle: "artist.test".into(),
                        did: "did:plc:artist".into(),
                        name: "Test Artist".into(),
                    },
                    origin: Origin {
                        handle: "o".into(),
                        text: "wish".into(),
                        url: "u".into(),
                    },
                    tags: Default::default(),
                    margin_notes: Default::default(),
                },
            },
            media_mode: MediaMode::Cdn,
            media_base: media_base.map(str::to_string),
        }
    }

    fn video(pds_key: Option<&str>, master_key: Option<&str>) -> Specimen {
        Specimen {
            rkey: "3mtest".into(),
            cid: "bafytest".into(),
            kind: MediaKind::Video,
            file: None,
            pds_key: pds_key.map(str::to_string),
            master_key: master_key.map(str::to_string),
            og_poster_key: None,
            caption: "A specimen".into(),
            date: "2026-06-04".into(),
            url: "https://example.test".into(),
            images: Vec::new(),
        }
    }

    #[test]
    fn full_src_prefers_master_over_pds_original() {
        let base = Some("https://media.fluoddity.observer");
        let c = ctx(base, video(Some("pds/3mtest_bafytest.mp4"), None));
        assert_eq!(
            c.full_video_src(c.catalog.archive.get("3mtest").unwrap()),
            Some("https://media.fluoddity.observer/pds/3mtest_bafytest.mp4".into())
        );

        let c = ctx(
            base,
            video(Some("pds/3mtest_bafytest.mp4"), Some("masters/3mtest.mp4")),
        );
        assert_eq!(
            c.full_video_src(c.catalog.archive.get("3mtest").unwrap()),
            Some("https://media.fluoddity.observer/masters/3mtest.mp4".into())
        );
    }

    #[test]
    fn full_src_needs_both_a_base_and_a_key() {
        // No vault keys recorded → only the Bluesky CDN exists.
        let c = ctx(Some("https://media.fluoddity.observer"), video(None, None));
        assert_eq!(
            c.full_video_src(c.catalog.archive.get("3mtest").unwrap()),
            None
        );

        // Keys recorded but no pull zone configured → keep falling back.
        let c = ctx(None, video(Some("pds/3mtest_bafytest.mp4"), None));
        assert_eq!(
            c.full_video_src(c.catalog.archive.get("3mtest").unwrap()),
            None
        );

        // The grid source is untouched by vault keys either way.
        let c = ctx(
            Some("https://media.fluoddity.observer"),
            video(Some("pds/3mtest_bafytest.mp4"), None),
        );
        let (src, hls, _) = c.video_sources(c.catalog.archive.get("3mtest").unwrap());
        assert!(src.contains("video.bsky.app"));
        assert!(hls.is_some());
    }
}
