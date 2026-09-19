mod api;
mod config;
mod counts;
mod covers;
mod db;
mod models;
mod thumb;
mod util;
mod watcher;
mod worker;

#[cfg(test)]
mod testutil;

use std::net::SocketAddr;
use std::path::Path;
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
    counts::CountCache,
    covers::CoverCache,
    db::Db,
    worker::{scan_existing, Worker},
};

/// Warn about missing media tools, but only when they are actually needed.
///
/// `ffprobe` is checked separately from `ffmpeg`: it is a distinct binary that
/// some split packages and minimal container images omit, and without it video
/// metadata probing silently returns nothing (so videos get no duration and no
/// dimensions).
///
/// Neither tool is used for images, and they are the only external programs the
/// service runs — so an album with no videos needs nothing installed. Warning
/// about FFmpeg on every start of an image-only installation is noise that
/// reads as breakage, hence the check for video files first.
fn check_media_tools(root: &Path) {
    let tool = |name: &str| {
        std::process::Command::new(name)
            .arg("-version")
            .output()
            .is_ok()
    };
    let (ffmpeg, ffprobe) = (tool("ffmpeg"), tool("ffprobe"));
    if ffmpeg && ffprobe {
        info!("FFmpeg and ffprobe detected.");
        return;
    }

    if !album_has_videos(root) {
        info!("No videos found in the album, so FFmpeg is not required.");
        return;
    }

    if !ffmpeg {
        warn!("FFmpeg not found on PATH: video thumbnails will be unavailable.");
    }
    if !ffprobe {
        warn!("ffprobe not found on PATH: video dimensions and durations will be unavailable.");
    }
    warn!("Your album contains videos. Install FFmpeg (which normally includes ffprobe): https://ffmpeg.org/download.html");
}

/// Whether the album contains a video, stopping at the first one found.
///
/// Used only to decide whether a missing FFmpeg is worth warning about. The walk
/// is the same shape as the worker's — dotfiles and `thumbs` are skipped, and a
/// symlinked directory is never descended into — so it cannot find media the
/// worker would refuse to process. A video-bearing album therefore costs one
/// directory read, and only an image-only album is walked in full.
fn album_has_videos(root: &Path) -> bool {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || name == "thumbs" {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let is_symlink = file_type.is_symlink();
            let is_dir = if is_symlink {
                entry.metadata().map(|m| m.is_dir()).unwrap_or(false)
            } else {
                file_type.is_dir()
            };
            if is_dir {
                if !is_symlink {
                    stack.push(entry.path());
                }
            } else if util::is_video_file(&name) {
                return true;
            }
        }
    }
    false
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
    // The admin URL is printed deliberately: it is how an operator learns the key
    // after installation, and it belongs in the log they already read for startup
    // errors. The key is not logged separately — that would only duplicate the
    // secret on a second line. The consequence is that these logs contain the key
    // (documented in README), so they should not be shipped to third-party log
    // aggregation or attached to bug reports.
    //
    // `public_url` is the deployment's externally visible base URL, so one line
    // covers both live and local runs. Previously this was hardcoded, which told
    // every deployment the same wrong domain.
    info!("Admin URL: {}#admin={}", cfg.server.public_url, cfg.admin.key);

    check_media_tools(&cfg.album.root);

    let db = Arc::new(Db::open(&cfg.state.db_path)?);

    // Repair cover rows whose paths were stored un-normalised by an earlier
    // build, which no lookup could ever match. One read of a tiny table on a
    // clean database.
    match db.normalize_stored_paths() {
        Ok(0) => {}
        Ok(n) => info!(
            "Normalised {n} stored folder cover path(s) that no lookup could have matched"
        ),
        Err(e) => warn!("Could not normalise stored cover paths: {e}"),
    }

    let worker = Worker::spawn(cfg.clone(), db.clone());

    // Recursive folder counts are cached here and invalidated by the watcher on
    // every change, so a browse does not re-walk the tree at each level.
    let counts = Arc::new(CountCache::new());
    // Computed covers are cached for the same reason: resolving one walks up to
    // three directory levels, once per subfolder, on every request.
    let covers = Arc::new(CoverCache::new());

    // The watcher is started *before* the initial scan, not after. The scan can
    // take a while on a large tree, and anything added while it runs would
    // otherwise be missed entirely until the next restart.
    let _watcher = watcher::start(
        &cfg.album.root,
        db.clone(),
        worker.tx.clone(),
        counts.clone(),
        covers.clone(),
    )?;

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

    let state = Arc::new(AppState {
        config: cfg.clone(),
        db,
        counts,
        covers,
    });

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
    // Without this, SIGTERM (which is how systemd stops the service) killed the
    // process mid-request and mid-write. Pending thumbnail jobs are still lost,
    // but they are re-queued by the next startup scan.
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    let queued = worker.tx.max_capacity() - worker.tx.capacity();
    if queued > 0 {
        info!(
            "Stopped with {} queued thumbnail job(s); the next startup scan re-queues them",
            queued
        );
    }

    Ok(())
}

/// Resolve when the process is asked to stop, so in-flight requests can finish.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(e) => {
                warn!("Could not install a SIGTERM handler: {e}");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("Shutdown requested (interrupt); finishing in-flight requests"),
        _ = terminate => info!("Shutdown requested (SIGTERM); finishing in-flight requests"),
    }
}
