use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::{mpsc, Semaphore};
use image::GenericImageView;
use tracing::{debug, info, warn};

use crate::{config::Config, db::Db, thumb, util};

/// Two samples of a file separated by this interval must be identical
/// (size + mtime) for the file to be considered stable. 300 ms is long
/// enough to catch an in-flight write (inotify events for a growing file
/// arrive continuously while it is being written) yet short enough that a
/// finished upload is never visibly delayed.
const STABILITY_INTERVAL: Duration = Duration::from_millis(300);

/// Maximum number of stability samples before we give up on the job and
/// defer. A file that is still changing after this window (e.g. a very
/// large upload over a slow network) is left alone for now; inotify always
/// fires a final event when the write completes, which re-triggers the
/// job at a point where the file is stable. This keeps the async gate from
/// ever holding a task open for an unbounded time.
const STABILITY_MAX_SAMPLES: u32 = 3;

#[derive(Debug, Clone)]
pub enum ThumbJob {
    Create { rel_path: String },
    Delete { rel_path: String },
}

pub struct Worker {
    pub tx: mpsc::UnboundedSender<ThumbJob>,
}

impl Worker {
    pub fn spawn(config: Config, db: Arc<Db>) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<ThumbJob>();
        let root = config.album.root.clone();

        // Limit concurrent *generation* jobs to avoid exhausting RAM when
        // processing large collections (each image::open loads the full
        // decoded image into memory). The permit is acquired only *after*
        // the async stability pre-pass, so waiting on an in-flight upload
        // never occupies a worker slot and bursts of hundreds of files do
        // not reduce generation throughput.
        //
        // This count is the service's main memory lever: each job can hold
        // a full decoded frame (24 MP decodes to ~72 MB), so workers x that
        // peak is the upper bound on RSS. `[worker] threads = 0` (default)
        // selects the core count clamped to 2..8; operators can pin a lower
        // value to fit a smaller systemd MemoryMax.
        let auto = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 8);
        let concurrency = if config.worker.threads == 0 {
            auto
        } else {
            // Clamp to sane bounds: at least 1, and never so high that the
            // memory bound becomes unbounded (32 x ~72 MB would be ~2.3 GB).
            config.worker.threads.clamp(1, 32) as usize
        };
        tracing::info!(
            "Thumbnail worker: {} concurrent jobs configured",
            concurrency
        );
        let semaphore = Arc::new(Semaphore::new(concurrency));

        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                match job {
                    ThumbJob::Delete { rel_path } => {
                        // Deletes are cheap (an unlink plus a few indexed
                        // SQL deletes) and never race an in-flight write,
                        // so no stability pre-pass is needed.
                        let root = root.clone();
                        let db = db.clone();
                        let permit = semaphore.clone().acquire_owned().await.unwrap();
                        tokio::task::spawn_blocking(move || {
                            let _permit = permit;
                            process_delete(&root, &db, rel_path);
                        });
                    }
                    ThumbJob::Create { rel_path } => {
                        // Each Create job becomes its own async task. This is
                        // deliberate: the stability pre-pass contains sleeps,
                        // and running it in per-job tasks lets hundreds of
                        // in-flight uploads wait out their writes *in
                        // parallel* instead of stalling the consumer loop
                        // (a fixed delay inserted into the loop would serialise
                        // them: 200 files x 300 ms = 60 s). Only jobs that
                        // actually need work then acquire a permit, so worker
                        // throughput is identical to the no-wait design.
                        let root = root.clone();
                        let db = db.clone();
                        let semaphore = semaphore.clone();
                        tokio::spawn(async move {
                            if !await_stable(&root, &db, &rel_path).await {
                                return;
                            }
                            let permit = semaphore.acquire_owned().await.unwrap();
                            tokio::task::spawn_blocking(move || {
                                let _permit = permit; // hold until job finishes
                                process_create(&root, &db, &rel_path);
                            });
                        });
                    }
                }
            }
        });

        Worker { tx }
    }
}

/// Async pre-pass run for every Create event, *before* a worker permit is
/// taken. Returns `true` only when the file appears to be finished being
/// written AND actually needs work.
///
/// This is the race fix for uploads. Two mechanisms work together:
///
/// 1. **Stability sampling.** inotify fires `IN_CREATE` the instant a file
///    appears (before any bytes are written) and `Modify` on every write
///    chunk. A fixed "sleep N ms" delay is not a robust guard: slow
///    uploads, network stalls, or backlog queue time can exceed it. Instead
///    we sample the file's size+mtime, wait, and re-sample. An actively
///    written file will show a change, so we keep sampling until it goes
///    quiet. Both size *and* mtime are compared because some copy/upload
///    tools pre-allocate the full file size up front, which would defeat a
///    size-only check.
///
/// 2. **Staleness check.** A thumbnail is stale when the source's mtime is
///    newer than the thumbnail's mtime. If a thumbnail exists and is fresh,
///    the event is just watcher churn and we return immediately with no
///    sleep at all (inotify delivers many Modify events per in-flight
///    write; those must stay cheap). If the source is newer than the
///    thumbnail — including the case where the thumbnail was generated
///    from a *partially uploaded* file — it is regenerated. This is the
///    self-heal path: even if the stability check ever loses the race, the
///    final Modify event at upload completion re-triggers the job and
///    repairs the thumbnail within a second.
///
/// The `get_metadata` call is a single indexed point read (sub-millisecond)
/// and SQLite runs in WAL mode, so taking the connection mutex here in the
/// async layer is cheap and safe.
async fn await_stable(root: &Path, db: &Db, rel_path: &str) -> bool {
    let src = root.join(rel_path);
    let meta = match std::fs::metadata(&src) {
        Ok(m) => m,
        // File vanished between the watcher event and now (moved/deleted);
        // the Remove event handles cleanup.
        Err(_) => return false,
    };
    let fname = src.file_name().unwrap_or_default().to_string_lossy().into_owned();
    if !util::is_media_file(&fname) {
        return false;
    }

    let thumb_fresh = thumb_is_fresh(root, rel_path, file_mtime(&meta));
    let meta_missing = db.get_metadata(rel_path).is_none();
    if thumb_fresh && !meta_missing {
        return false; // nothing to do
    }

    let mut last = (meta.len(), file_mtime(&meta));
    for _ in 0..STABILITY_MAX_SAMPLES {
        tokio::time::sleep(STABILITY_INTERVAL).await;
        let cur = match std::fs::metadata(&src) {
            Ok(m) => (m.len(), file_mtime(&m)),
            Err(_) => return false, // vanished while we waited
        };
        if cur == last {
            return true; // stable: safe to generate
        }
        last = cur; // still changing: sample again
    }

    // Still changing after the last sample: defer rather than wait forever.
    // The final Modify event at end-of-write will re-trigger this job, and
    // the file will be stable then.
    debug!(
        "File still changing after {} samples, deferring: {}",
        STABILITY_MAX_SAMPLES, rel_path
    );
    false
}

/// A thumbnail is "fresh" when it exists and its mtime is at least as new as
/// the source's mtime.
///
/// This is the linchpin of self-healing. A healthy thumbnail is written
/// *after* the source last changed, so its mtime is >= the source's. If the
/// source changes afterwards — because the thumbnail was generated from a
/// partially uploaded file, or because the user replaced the photo — the
/// source becomes newer than the thumbnail, marking it stale and forcing
/// regeneration. (Renaming/moving is not affected: mtime is preserved, and
/// a file in a new folder has no thumbnail there anyway, so it is simply
/// generated fresh.)
///
/// `modified()` is used instead of `Metadata::mtime()` because the mtime
/// accessor is platform-specific (Unix-only) and this service compiles
/// for Linux, macOS, and Windows with no conditional code.
fn thumb_is_fresh(root: &Path, rel_path: &str, src_mtime: SystemTime) -> bool {
    let src = root.join(rel_path);
    let fname = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let thumb_path = match src.parent() {
        Some(p) => p.join("thumbs").join(util::thumb_name(&fname)),
        None => return false,
    };
    match std::fs::metadata(&thumb_path) {
        Ok(t) => file_mtime(&t) >= src_mtime,
        Err(_) => false, // no thumbnail
    }
}

/// Portable mtime accessor (`modified()` works on all supported platforms).
fn file_mtime(meta: &std::fs::Metadata) -> SystemTime {
    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)
}

/// CPU-bound part of a Create job, run on a blocking thread while holding a
/// worker permit.
fn process_create(root: &Path, db: &Db, rel_path: &str) {
    let src = root.join(rel_path);
    let meta = match std::fs::metadata(&src) {
        Ok(m) => m,
        Err(_) => return,
    };
    let fname = src.file_name().unwrap_or_default().to_string_lossy().into_owned();
    if !util::is_media_file(&fname) {
        return;
    }

    // Belt-and-braces re-check on the worker thread. The async gate saw the
    // file as stable when *it* ran, but that was a while ago: the file may
    // have been re-uploaded, or a concurrent duplicate job may have
    // finished generating first. Skip any decode work if nothing is out of
    // date. (If the file happens to be mid-re-upload right now, this could
    // still produce a partial thumbnail once — the self-heal via the final
    // Modify event will fix it, so this residual race is harmless.)
    if thumb_is_fresh(root, rel_path, file_mtime(&meta)) && db.get_metadata(rel_path).is_some() {
        return;
    }

    let parent = src.parent().unwrap();
    let thumbs_dir = parent.join("thumbs");
    let thumb_path = thumbs_dir.join(util::thumb_name(&fname));

    if thumb_is_fresh(root, rel_path, file_mtime(&meta)) {
        // Thumbnail is up to date but the dimensions cache row is missing.
        // Read the dimensions without regenerating the thumbnail.
        update_metadata(root, db, rel_path, &src);
        return;
    }

    info!("Generating thumbnail for {}", rel_path);

    if util::is_image_file(&fname) {
        match thumb::generate_image_thumb(&src, &thumb_path) {
            Ok((w, h)) => {
                let modified = get_mtime(&src);
                let _ = db.set_metadata(rel_path, w, h, modified);
            }
            Err(e) => {
                warn!("Failed to generate image thumb for {}: {}", rel_path, e);
            }
        }
    } else if util::is_video_file(&fname) {
        match thumb::generate_video_thumb(&src, &thumb_path) {
            Ok(()) => {
                if let Some((w, h)) = thumb::get_video_dimensions(&src) {
                    let modified = get_mtime(&src);
                    let _ = db.set_metadata(rel_path, w, h, modified);
                }
            }
            Err(e) => {
                warn!("Failed to generate video thumb for {}: {}", rel_path, e);
            }
        }
    }
}

/// Synchronous cleanup for a Remove event.
fn process_delete(root: &Path, db: &Db, rel_path: String) {
    thumb::delete_thumb(root, &rel_path);
    let _ = db.delete_cover(&rel_path);
    let _ = db.delete_metadata(&rel_path);

    // If a photo was deleted, clear any ancestor folder covers that referenced it.
    // Covers store the full relative image path, so we match against rel_path.
    let path = std::path::Path::new(&rel_path);
    if let Some(fname) = path.file_name().and_then(|n| n.to_str()) {
        if util::is_media_file(fname) {
            if let Some(parent) = path.parent().and_then(|p| p.to_str()) {
                let _ = db.delete_cover_if_matches(parent, &rel_path);
            }
            // Also check all ancestor folders up the tree
            let parts: Vec<&str> = rel_path.split('/').collect();
            for i in 1..parts.len().saturating_sub(1) {
                let ancestor = parts[..i].join("/");
                let _ = db.delete_cover_if_matches(&ancestor, &rel_path);
            }
        }
    }
}

fn update_metadata(_root: &Path, db: &Db, rel_path: &str, src: &Path) {
    let fname = src.file_name().unwrap_or_default().to_string_lossy();
    if util::is_image_file(&fname) {
        if let Ok(img) = image::open(src) {
            let (w, h) = img.dimensions();
            let modified = get_mtime(src);
            let _ = db.set_metadata(rel_path, w, h, modified);
        }
    } else if util::is_video_file(&fname) {
        if let Some((w, h)) = thumb::get_video_dimensions(src) {
            let modified = get_mtime(src);
            let _ = db.set_metadata(rel_path, w, h, modified);
        }
    }
}

fn get_mtime(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn scan_existing(root: &Path, db: &Db, tx: &mpsc::UnboundedSender<ThumbJob>) {
    let _ = walk_dir(root, PathBuf::new(), db, tx);
}

fn walk_dir(
    root: &Path,
    rel: PathBuf,
    db: &Db,
    tx: &mpsc::UnboundedSender<ThumbJob>,
) -> anyhow::Result<()> {
    // Iterative DFS to avoid unbounded recursion on deeply nested folder trees.
    let mut stack = vec![rel];

    while let Some(current_rel) = stack.pop() {
        let dir = root.join(&current_rel);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                warn!("Failed to read directory {}: {}", dir.display(), e);
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warn!("Failed to read entry in {}: {}", dir.display(), e);
                    continue;
                }
            };
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.') || name_str == "thumbs" {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(e) => {
                    warn!("Failed to read metadata for {}: {}", entry.path().display(), e);
                    continue;
                }
            };
            let sub_rel = if current_rel.as_os_str().is_empty() {
                PathBuf::from(&*name_str)
            } else {
                current_rel.join(&*name_str)
            };
            if meta.is_dir() {
                stack.push(sub_rel);
            } else if util::is_media_file(&name_str) {
                let rel_str = sub_rel.to_string_lossy().replace('\\', "/");
                // Queue regeneration when the thumbnail is missing or stale
                // (source newer than thumbnail), not just when missing.
                // Checking staleness here — not only existence — means the
                // startup scan also repairs any corrupt thumbnails left
                // behind by the old race (generated from partially
                // uploaded files): they have an older mtime than the now-
                // complete source, so they are regenerated once on the next
                // service restart. The async gate in await_stable re-checks
                // this cheaply at job time, so already-fresh files cost
                // nothing.
                let fresh = thumb_is_fresh(root, &rel_str, file_mtime(&meta));
                if db.get_metadata(&rel_str).is_none() || !fresh {
                    let _ = tx.send(ThumbJob::Create { rel_path: rel_str });
                }
            }
        }
    }
    Ok(())
}
