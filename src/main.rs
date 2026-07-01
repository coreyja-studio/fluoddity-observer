mod admin;
mod auth;
mod catalog;
mod db;
mod ingest;
mod threads;
mod views;

use std::sync::Arc;

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

pub struct AppState {
    pub pool: PgPool,
    pub media_mode: MediaMode,
    pub oauth: auth::AtriumOAuthClient,
    pub threads: threads::ThreadFetcher,
}

type SharedState = Arc<AppState>;

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
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .map_err(|_| anyhow::anyhow!("DATABASE_URL must be set (see .mise.toml)"))?;
    let pool = PgPool::connect(&database_url).await?;
    sqlx::migrate!().run(&pool).await?;

    match std::env::args().nth(1).as_deref() {
        None | Some("serve") => serve(pool).await,
        Some("import") => import(pool).await,
        Some("ingest-once") => ingest_once(pool).await,
        Some(other) => anyhow::bail!(
            "unknown subcommand {other:?} — expected `serve`, `import`, or `ingest-once`"
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
        families = stats.families,
        margin_notes = stats.margin_notes,
        "import complete"
    );
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
        families = catalog.editorial.families.len(),
        ?media_mode,
        "catalog loaded from database"
    );

    // Live ingest: poll for new posts in the background. PCG_POLL_SECS=0
    // disables it.
    let poll_secs: u64 = std::env::var("PCG_POLL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    if poll_secs > 0 {
        tracing::info!(interval_secs = poll_secs, "starting ingest poller");
        tokio::spawn(ingest::run(pool.clone(), poll_secs));
    }

    auth::seed_curators(&pool).await?;
    // Loopback OAuth client: the callback host must stay 127.0.0.1 until the
    // hosted (confidential-client) metadata lands with a public domain.
    let callback_url = std::env::var("PCG_OAUTH_CALLBACK_URL")
        .unwrap_or_else(|_| format!("http://127.0.0.1:{port}/admin/oauth/callback"));
    let oauth = auth::build_oauth_client(pool.clone(), callback_url)?;

    let http = reqwest::Client::builder()
        .user_agent("paperclips-gallery/0.1 (fluoddity field guide)")
        .build()?;
    let state: SharedState = Arc::new(AppState {
        pool,
        media_mode,
        oauth,
        threads: threads::ThreadFetcher::new(http),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/archive", get(archive))
        .route("/room/{author}/{rkey}", get(thread_room))
        .route("/specimen/{rkey}", get(specimen))
        .route("/colophon", get(colophon))
        .route("/admin", get(admin::dashboard))
        .route(
            "/admin/login",
            get(admin::login_page).post(admin::login_submit),
        )
        .route("/admin/oauth/callback", get(admin::oauth_callback))
        .route("/admin/logout", axum::routing::post(admin::logout))
        .route(
            "/admin/thread-rooms/add",
            axum::routing::post(admin::add_thread_room),
        )
        .route(
            "/admin/thread-rooms/remove",
            axum::routing::post(admin::remove_thread_room),
        )
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

async fn specimen(
    State(state): State<SharedState>,
    Path(rkey): Path<String>,
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
    Ok(views::specimen(&ctx, &hung_in, specimen).into_response())
}

async fn colophon(State(state): State<SharedState>) -> Result<maud::Markup, AppError> {
    Ok(views::colophon(&state.ctx().await?))
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
