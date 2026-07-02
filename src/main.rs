mod admin;
mod auth;
mod bot;
mod catalog;
mod cron;
mod db;
mod ingest;
mod jobs;
mod margin_notes;
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
}

impl Ctx {
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
        Some("refresh-notes") => refresh_notes(pool).await,
        Some("bot-once") => bot_once(pool).await,
        Some("bot-weekly") => bot_weekly(pool).await,
        Some("gen-oauth-key") => {
            println!("{}", auth::generate_private_key()?);
            Ok(())
        }
        Some(other) => anyhow::bail!(
            "unknown subcommand {other:?} — expected `serve`, `import`, `ingest-once`, `refresh-notes`, `bot-once`, `bot-weekly`, or `gen-oauth-key`"
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
    let state: SharedState = AppState {
        pool,
        media_mode,
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
        .route("/room/{author}/{rkey}", get(thread_room))
        .route("/specimen/{rkey}", get(specimen))
        .route("/tag/{tag}", get(tag_page))
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
        .route("/admin/tags/add", axum::routing::post(admin::add_tag))
        .route("/admin/tags/remove", axum::routing::post(admin::remove_tag))
        .route("/static/style.css", get(stylesheet))
        .route("/static/admin.css", get(admin_css))
        .route("/static/gallery.js", get(gallery_js))
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

async fn gallery_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../static/gallery.js"),
    )
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, views::not_found()).into_response()
}
