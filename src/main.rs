mod api;
mod config;
mod db;
mod models;
mod thumb;
mod util;
mod watcher;
mod worker;

use std::net::SocketAddr;
use std::sync::Arc;
use axum::{
    routing::{get, post},
    Router,
};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use crate::{
    api::{AppState, get_album, set_cover, health},
    config::load_or_create,
    db::Db,
    worker::{scan_existing, Worker},
};

fn check_ffmpeg() {
    if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
        warn!("FFmpeg not found on PATH. Video thumbnail generation will be unavailable.");
        warn!("Install FFmpeg: https://ffmpeg.org/download.html");
    } else {
        info!("FFmpeg detected.");
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_env("SIMPLE_ALBUM_LOG"))
        .init();

    let (cfg, cfg_path) = load_or_create()?;
    info!("Config loaded from {}", cfg_path.display());
    info!("Album root: {}", cfg.album.root.display());
    info!("API binding: {}", cfg.server.bind);
    if !cfg.admin.key.is_empty() {
        info!("Admin key: {}", cfg.admin.key);
        info!("Admin URL (production): https://your-domain.com/#admin={}", cfg.admin.key);
        info!("Admin URL (local dev):  https://localhost:8443/#admin={}", cfg.admin.key);
    }

    check_ffmpeg();

    let db = Arc::new(Db::open(&cfg.state.db_path)?);
    let worker = Worker::spawn(cfg.clone(), db.clone());

    // Initial scan
    info!("Starting initial scan...");
    scan_existing(&cfg.album.root, &db, &worker.tx);
    info!("Initial scan queued. Starting watcher and API...");

    // Start filesystem watcher
    let _watcher = watcher::start(&cfg.album.root, db.clone(), worker.tx.clone())?;

    let state = Arc::new(AppState { config: cfg.clone(), db });

    let app = Router::new()
        .route("/api/album", get(get_album))
        .route("/api/cover", post(set_cover))
        .route("/api/health", get(health))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = cfg.server.bind.parse()?;
    info!("API server listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
