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
use anyhow::Context;
use axum::{
    routing::{get, post},
    Router,
};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use crate::{
    api::{AppState, get_album, health, set_cover, share_page},
    config::Config,
    db::Db,
    worker::{scan_existing, Worker},
};

/// Warn about missing media tools. `ffprobe` is checked separately from
/// `ffmpeg`: it is a distinct binary that some split packages and minimal
/// container images omit, and without it video metadata probing silently
/// returns nothing (so videos get no duration and no dimensions).
fn check_media_tools() {
    if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
        warn!("FFmpeg not found on PATH. Video thumbnail generation will be unavailable.");
        warn!("Install FFmpeg: https://ffmpeg.org/download.html");
    } else {
        info!("FFmpeg detected.");
    }
    if std::process::Command::new("ffprobe").arg("-version").output().is_err() {
        warn!("ffprobe not found on PATH. Video dimensions and durations will be unavailable.");
        warn!("It is normally installed with FFmpeg. See https://ffmpeg.org/download.html");
    } else {
        info!("ffprobe detected.");
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `SIMPLE_ALBUM_LOG` is optional and defaults to `info`, so the startup log
    // (which contains the admin URL) is visible without extra configuration.
    // `EnvFilter::from_env` would instead leave the service silent when unset.
    let filter = match tracing_subscriber::EnvFilter::try_from_env("SIMPLE_ALBUM_LOG") {
        Ok(filter) => filter,
        Err(err) => {
            if std::env::var_os("SIMPLE_ALBUM_LOG").is_some() {
                eprintln!("Ignoring invalid SIMPLE_ALBUM_LOG ({err}); using `info`.");
            }
            tracing_subscriber::EnvFilter::new("info")
        }
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let (cfg, cfg_path) = Config::load()?;
    info!("Config loaded from {}", cfg_path.display());
    info!("Album root: {}", cfg.album.root.display());
    info!("API binding: {}", cfg.server.bind);
    info!("Admin key: {}", cfg.admin.key);
    // `public_url` is the deployment's externally visible base URL, so one line
    // covers both live and local runs. Previously this was hardcoded, which told
    // every deployment the same wrong domain.
    info!("Admin URL: {}#admin={}", cfg.server.public_url, cfg.admin.key);

    check_media_tools();

    let db = Arc::new(Db::open(&cfg.state.db_path)?);
    let worker = Worker::spawn(cfg.clone(), db.clone());

    // The watcher is started *before* the initial scan, not after. The scan can
    // take a while on a large tree, and anything added while it runs would
    // otherwise be missed entirely until the next restart.
    let _watcher = watcher::start(&cfg.album.root, db.clone(), worker.tx.clone())?;

    // The scan then runs on a blocking thread rather than inline: it walks the
    // whole photo tree, and there is no reason for the API to be unreachable
    // while it does. Thumbnail generation is already a background worker, so
    // the service is usable as soon as this function returns.
    {
        let root = cfg.album.root.clone();
        let db = db.clone();
        let tx = worker.tx.clone();
        tokio::task::spawn_blocking(move || {
            info!("Starting initial scan...");
            scan_existing(&root, &db, &tx);
            info!("Initial scan complete. Thumbnail backlog is being worked through.");
        });
    }

    let state = Arc::new(AppState { config: cfg.clone(), db });

    let app = Router::new()
        .route("/api/album", get(get_album))
        .route("/api/cover", post(set_cover))
        .route("/api/health", get(health))
        .route("/api/share", get(share_page))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = cfg.server.bind.parse().with_context(|| {
        format!("`server.bind` `{}` is not a valid socket address", cfg.server.bind)
    })?;
    info!("API server listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
