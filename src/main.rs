mod catalog;
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
use tower_http::services::ServeDir;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaMode {
    /// Serve mp4s from the local archive directory under /media.
    Local,
    /// Point at Bluesky's video CDN (HLS) — no local media hosting needed.
    Cdn,
}

pub struct AppState {
    pub catalog: Catalog,
    pub media_mode: MediaMode,
}

impl AppState {
    /// (video src, hls url for hls.js, poster url)
    pub fn video_sources(&self, s: &catalog::Specimen) -> (String, Option<String>, String) {
        match self.media_mode {
            MediaMode::Local => (
                format!("/media/{}", s.file),
                None,
                format!("/media/posters/{}.jpg", s.rkey),
            ),
            MediaMode::Cdn => {
                let did = &self.catalog.editorial.artist.did;
                let playlist =
                    format!("https://video.bsky.app/watch/{did}/{}/playlist.m3u8", s.cid);
                let poster = format!("https://video.bsky.app/watch/{did}/{}/thumbnail.jpg", s.cid);
                (playlist.clone(), Some(playlist), poster)
            }
        }
    }
}

type SharedState = Arc<AppState>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let catalog_path = std::env::var("PCG_CATALOG").unwrap_or_else(|_| "catalog.json".to_string());
    let media_dir = std::env::var("PCG_MEDIA_DIR")
        .unwrap_or_else(|_| "/home/coreyja.linux/paperclips-media/oops".to_string());
    let media_mode = match std::env::var("PCG_MEDIA_MODE").as_deref() {
        Ok("cdn") => MediaMode::Cdn,
        _ => MediaMode::Local,
    };
    let port: u16 = std::env::var("PCG_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(4601);

    let metadata_path =
        std::env::var("PCG_METADATA").unwrap_or_else(|_| format!("{media_dir}/metadata.jsonl"));

    let catalog = Catalog::load(&catalog_path, &metadata_path)?;
    tracing::info!(
        archive = catalog.archive.len(),
        rooms = catalog.editorial.rooms.len(),
        curated = catalog
            .editorial
            .rooms
            .iter()
            .map(|r| r.rkeys.len())
            .sum::<usize>(),
        families = catalog.editorial.families.len(),
        ?media_mode,
        "catalog loaded"
    );

    let state: SharedState = Arc::new(AppState {
        catalog,
        media_mode,
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/room/{slug}", get(room))
        .route("/archive", get(archive))
        .route("/specimen/{rkey}", get(specimen))
        .route("/colophon", get(colophon))
        .route("/static/style.css", get(stylesheet))
        .route("/static/gallery.js", get(gallery_js))
        .nest_service("/media", ServeDir::new(media_dir))
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

async fn index(State(state): State<SharedState>) -> maud::Markup {
    views::index(&state)
}

async fn room(State(state): State<SharedState>, Path(slug): Path<String>) -> Response {
    match state.catalog.room(&slug) {
        Some(room) => views::room(&state, room).into_response(),
        None => not_found(),
    }
}

async fn archive(State(state): State<SharedState>) -> maud::Markup {
    views::archive(&state)
}

async fn specimen(State(state): State<SharedState>, Path(rkey): Path<String>) -> Response {
    match state.catalog.archive.get(&rkey) {
        Some(specimen) => {
            let room = state.catalog.room_of(&rkey);
            views::specimen(&state, room, specimen).into_response()
        }
        None => not_found(),
    }
}

async fn colophon(State(state): State<SharedState>) -> maud::Markup {
    views::colophon(&state)
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../static/style.css"),
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
