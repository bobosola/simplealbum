use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::counts::CountCache;
use crate::covers::CoverCache;
use crate::db::Db;
use crate::worker::{self, ThumbJob};

/// Start the recursive filesystem watcher.
///
/// Must be called from within a Tokio runtime context: a folder arriving as a
/// single event needs its contents enumerated, which happens on a blocking task
/// off this callback thread.
pub fn start(
    album_root: &Path,
    db: Arc<Db>,
    tx: mpsc::Sender<ThumbJob>,
    counts: Arc<CountCache>,
    covers: Arc<CoverCache>,
) -> anyhow::Result<RecommendedWatcher> {
    let root = album_root.to_path_buf();
    let handle = tokio::runtime::Handle::current();

    // The watcher backend reports paths as the filesystem resolves them, which
    // is not always how they were configured: on macOS a root given as `/tmp/x`
    // is reported as `/private/tmp/x`, because `/tmp` is a symlink. Without the
    // canonical form as a fallback, `strip_prefix` fails and the full absolute
    // path is treated as album-relative — which silently generates thumbnails
    // whose metadata is keyed by a path no API lookup will ever match.
    let canonical_root = std::fs::canonicalize(album_root).unwrap_or_else(|_| root.clone());

    // Folders whose contents are already being enumerated, so a burst of events
    // for the same folder (one per file written inside it on some platforms) does
    // not start overlapping walks of the same subtree.
    let scanning: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

    let mut watcher = RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            let event = match res {
                Ok(event) => event,
                Err(e) => {
                    warn!("Watch error: {}", e);
                    return;
                }
            };

            for path in event.paths {
                // Events outside the album root are ignored rather than guessed
                // at, so a path that cannot be made relative never reaches the
                // worker.
                let Some(rel) = strip_root(&root, &canonical_root, &path) else {
                    continue;
                };
                let rel_str = rel.replace('\\', "/");
                if is_thumbs(&rel_str) {
                    // A generated or deleted thumbnail can turn a cover-less
                    // folder into a resolvable one (or back), and it never
                    // changes a photo or album count. Only the cover cache is
                    // dropped here — invalidating the count cache on every
                    // thumbnail would force a full re-walk of the tree after
                    // each one the worker writes.
                    covers.invalidate();
                    continue;
                }
                match event.kind {
                    notify::EventKind::Create(_) | notify::EventKind::Modify(_) => {
                        // Any add, rename or content change can alter counts and
                        // covers, so drop the cached ones. Cheap: a couple of
                        // atomic bumps.
                        counts.invalidate();
                        covers.invalidate();
                        if path.is_file() {
                            worker::enqueue(&tx, ThumbJob::Create { rel_path: rel_str });
                        } else if path.is_dir() {
                            // A folder moved or copied into the album is reported
                            // as one event for the folder; its files are not
                            // reported individually, so walk it.
                            scan_folder(&handle, &root, &db, &tx, &scanning, rel_str);
                        } else {
                            // The path does not exist any more. That is how a
                            // rename or move reports its *source*
                            // (`IN_MOVED_FROM` arrives as a Modify naming the
                            // old path), while the destination arrives as its
                            // own event and is regenerated there.
                            //
                            // For a *file* this reclaims the stale thumbnail
                            // and metadata row. For a **folder** it purges
                            // every row recorded beneath the old path, which is
                            // the only chance we get: a moved folder's contents
                            // are re-scanned at the destination, but nothing
                            // else would ever reclaim the old prefix, so those
                            // rows were orphaned forever. The worker decides
                            // which case it is (it can check the name), so a
                            // vanished non-media path is harmless here.
                            worker::enqueue(&tx, ThumbJob::Delete { rel_path: rel_str });
                        }
                    }
                    // `IN_CLOSE_WRITE` (and its equivalents) is the "this file
                    // is finished being written" signal, which is exactly when
                    // a deferred upload becomes safe to process. Treating it as
                    // a Create gives the stability gate a guaranteed final
                    // retry even if the platform coalesces the trailing
                    // Modify. The gate itself makes repeats cheap: it checks
                    // freshness first and returns without decoding anything.
                    notify::EventKind::Access(notify::event::AccessKind::Close(
                        notify::event::AccessMode::Write,
                    )) => {
                        if path.is_file() {
                            covers.invalidate();
                            worker::enqueue(&tx, ThumbJob::Create { rel_path: rel_str });
                        }
                    }
                    notify::EventKind::Remove(_) => {
                        // May be a single file or a whole folder; the worker
                        // distinguishes the two cases itself.
                        counts.invalidate();
                        covers.invalidate();
                        worker::enqueue(&tx, ThumbJob::Delete { rel_path: rel_str });
                    }
                    _ => {}
                }
            }
        },
        notify::Config::default(),
    )?;

    watcher.watch(album_root, RecursiveMode::Recursive)?;
    info!("Filesystem watcher started on {}", album_root.display());
    Ok(watcher)
}

/// True for the thumbnails folder itself or anything inside it. Events for
/// generated thumbnails must never feed back into the worker.
fn is_thumbs(rel: &str) -> bool {
    rel == "thumbs" || rel.starts_with("thumbs/") || rel.contains("/thumbs/")
}


/// Enumerate a folder that has just appeared, without restarting the service.
fn scan_folder(
    handle: &tokio::runtime::Handle,
    root: &Path,
    db: &Arc<Db>,
    tx: &mpsc::Sender<ThumbJob>,
    scanning: &Arc<Mutex<HashSet<String>>>,
    rel: String,
) {
    {
        let mut in_flight = scanning.lock().unwrap_or_else(|e| e.into_inner());
        if !in_flight.insert(rel.clone()) {
            return; // already being walked
        }
    }

    let root = root.to_path_buf();
    let db = db.clone();
    let tx = tx.clone();
    let scanning = scanning.clone();

    handle.spawn(async move {
        let walk_rel = rel.clone();
        // Filesystem walking is blocking work and must not occupy a runtime
        // worker thread.
        let _ = tokio::task::spawn_blocking(move || {
            worker::scan_subtree(&root, &walk_rel, &db, &tx);
        })
        .await;
        scanning
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&rel);
    });
}

/// Strip the album root from an event path, returning `None` when the path is
/// not inside the album at all (or is the root itself).
fn strip_root(root: &Path, canonical_root: &Path, path: &Path) -> Option<String> {
    let rel = path
        .strip_prefix(root)
        .or_else(|_| path.strip_prefix(canonical_root))
        .ok()?;
    let rel = rel.to_string_lossy();
    if rel.is_empty() {
        None
    } else {
        Some(rel.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_root_handles_configured_and_canonical_forms() {
        let root = Path::new("/tmp/album");
        let canonical = Path::new("/private/tmp/album");
        // Configured form.
        assert_eq!(
            strip_root(root, canonical, Path::new("/tmp/album/1970/a.jpg")),
            Some("1970/a.jpg".to_string())
        );
        // Resolved form, as reported by macOS FSEvents for a symlinked root.
        assert_eq!(
            strip_root(root, canonical, Path::new("/private/tmp/album/1970/a.jpg")),
            Some("1970/a.jpg".to_string())
        );
        // The root itself carries no relative path.
        assert_eq!(strip_root(root, canonical, Path::new("/tmp/album")), None);
        // Unrelated paths are rejected instead of being treated as relative.
        assert_eq!(strip_root(root, canonical, Path::new("/etc/passwd")), None);
        assert_eq!(strip_root(root, canonical, Path::new("/private/tmp/other/a.jpg")), None);
    }

    #[test]
    fn thumbs_paths_are_filtered_at_any_depth() {
        assert!(is_thumbs("thumbs"));
        assert!(is_thumbs("thumbs/a_thumb.jpg"));
        assert!(is_thumbs("1970-79/1970/thumbs/a_thumb.jpg"));
        assert!(!is_thumbs("1970-79/1970/a.jpg"));
        assert!(!is_thumbs("1970-79/thumbsup/a.jpg"));
    }
}
