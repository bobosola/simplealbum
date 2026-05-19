use std::path::Path;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::{db::Db, worker::ThumbJob};

pub fn start(
    album_root: &std::path::Path,
    _db: std::sync::Arc<Db>,
    tx: mpsc::UnboundedSender<ThumbJob>,
) -> anyhow::Result<RecommendedWatcher> {
    let root = album_root.to_path_buf();

    let mut watcher = RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            match res {
                Ok(event) => {
                    for path in event.paths {
                        let rel = strip_root(&root, &path);
                        if rel.is_empty() {
                            continue;
                        }
                        let rel_str = rel.replace('\\', "/");
                        if rel_str.starts_with("thumbs/") || rel_str.contains("/thumbs/") {
                            continue;
                        }
                        match event.kind {
                            notify::EventKind::Create(_) | notify::EventKind::Modify(_) => {
                                if path.is_file() {
                                    let _ = tx.send(ThumbJob::Create { rel_path: rel_str });
                                }
                            }
                            notify::EventKind::Remove(_) => {
                                let _ = tx.send(ThumbJob::Delete { rel_path: rel_str });
                            }
                            _ => {}
                        }
                    }
                }
                Err(e) => {
                    warn!("Watch error: {}", e);
                }
            }
        },
        notify::Config::default(),
    )?;

    watcher.watch(album_root, RecursiveMode::Recursive)?;
    info!("Filesystem watcher started on {}", album_root.display());
    Ok(watcher)
}

fn strip_root(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}
