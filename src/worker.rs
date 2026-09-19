use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::{mpsc, Semaphore};
use tracing::{debug, info, warn};

use crate::{config::Config, db::Db, db::PhotoMeta, thumb, util};

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

/// Grace window applied when a source file's mtime is in the future. See
/// [`thumb_is_fresh`] for why such a timestamp cannot be trusted.
const UNTRUSTED_MTIME_GRACE: Duration = Duration::from_secs(5);

/// A file whose mtime is older than this cannot be an in-flight upload, so the
/// stability sampling below is skipped for it.
///
/// On the initial scan of an existing library every file would otherwise pay
/// the full 300 ms sample window before anything is generated, which on 8,000
/// photos is over half an hour of pure sleeping. The gate stays in place for
/// files that *could* still be arriving, and the `thumb_is_fresh` self-heal
/// still repairs a thumbnail that was built from a partial file: a writer that
/// copies in place (rather than renaming a completed temp file, as `rsync` and
/// every upload tool this project documents does) leaves the source mtime newer
/// than the thumbnail, so the final write event regenerates it.
const STABILITY_SKIP_AGE: Duration = Duration::from_secs(5);

/// Capacity of the job queue.
///
/// The queue was unbounded, which meant the initial scan of a large tree
/// enqueued one job per media file — tens of thousands of `String` paths —
/// before the worker had generated anything, and the only memory bound in the
/// system was the pre-pass semaphore. A bounded queue makes the *producer*
/// wait instead, which is the back-pressure the pre-pass comment describes but
/// could not deliver while the channel itself accepted everything.
const JOB_QUEUE_CAPACITY: usize = 1024;

/// Upper bound on Create jobs whose stability pre-pass is running at once.
///
/// The job queue is unbounded and the consumer loop spawns one task per job, so
/// without this the first scan of a large tree (one job per file) would create
/// thousands of concurrent tasks, each taking stat samples and hammering the
/// single SQLite connection mutex — all before the smaller generation
/// semaphore below is reached. Taking a permit *before* spawning bounds the
/// task count; because the pre-pass only sleeps, a few hundred files still wait
/// out their uploads in parallel, so throughput of finished files is unchanged.
/// When the cap is hit the consumer stops draining the queue, which is the
/// back-pressure that keeps the burst bounded.
const PREPASS_CONCURRENCY: usize = 256;

#[derive(Debug, Clone)]
pub enum ThumbJob {
    Create { rel_path: String },
    Delete { rel_path: String },
}

pub struct Worker {
    pub tx: mpsc::Sender<ThumbJob>,
}

/// Queue one job, waiting if the queue is full.
///
/// Every caller runs on a blocking thread or on `notify`'s own event thread,
/// never on an async worker, so parking here is safe — and is the point: the
/// bounded channel pushes back on the scan rather than growing without limit.
/// A closed queue means the worker is gone, which only happens during shutdown;
/// the next startup scan re-queues whatever was dropped.
pub fn enqueue(tx: &mpsc::Sender<ThumbJob>, job: ThumbJob) {
    if tx.blocking_send(job).is_err() {
        debug!("Thumbnail worker has shut down; dropping a queued job");
    }
}

impl Worker {
    pub fn spawn(config: Config, db: Arc<Db>) -> Self {
        let (tx, mut rx) = mpsc::channel::<ThumbJob>(JOB_QUEUE_CAPACITY);
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
        // peak is the upper bound on RSS. `[worker] threads = 0` (auto)
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
        let prepass = Arc::new(Semaphore::new(PREPASS_CONCURRENCY));

        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                match job {
                    ThumbJob::Delete { rel_path } => {
                        // Deletes are cheap (an unlink plus a few indexed SQL
                        // deletes) and never race an in-flight write, so no
                        // stability pre-pass is needed. They also take no
                        // worker permit: the permit bounds the *memory* of
                        // full-frame decodes, and a delete holds none. Waiting
                        // for one here would suspend this receive loop, so a
                        // burst of deletes arriving while all workers were busy
                        // decoding would stall every queued job behind it.
                        let root = root.clone();
                        let db = db.clone();
                        tokio::task::spawn_blocking(move || {
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
                        //
                        // `prepass` bounds how many of those tasks exist at
                        // once; acquiring it here suspends the receive loop
                        // rather than spawning an unbounded number of waiters.
                        let root = root.clone();
                        let db = db.clone();
                        let semaphore = semaphore.clone();
                        let pre_pass_permit = prepass
                            .clone()
                            .acquire_owned()
                            .await
                            .expect("pre-pass semaphore is never closed");
                        tokio::spawn(async move {
                            let _pre_pass_permit = pre_pass_permit; // held across the wait
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
    if thumb_fresh && !metadata_incomplete(db.get_metadata(rel_path), &fname) {
        return false; // nothing to do
    }

    // An empty file is not a small file, it is a file that has not been written
    // yet. Two samples of `(0, mtime)` look identical, so without this check a
    // freshly created placeholder would pass the stability gate and be decoded
    // (logging a decode failure every time). Defer instead: the write that
    // fills it fires another event, and the file is judged then.
    if meta.len() == 0 {
        debug!("Empty file, deferring: {}", rel_path);
        return false;
    }

    // A file whose mtime is already well in the past is not an upload in
    // flight, so the sample window below would be pure latency. This is the
    // difference between the first scan of an existing library finishing in
    // minutes and it paying 300 ms of sleep per file. A source copied in place
    // (rather than renamed into place) still self-heals: its mtime ends up
    // newer than the thumbnail generated from the partial file.
    if let Ok(age) = SystemTime::now().duration_since(file_mtime(&meta))
        && age > STABILITY_SKIP_AGE
    {
        return true;
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

    // A source whose mtime is in the future — a camera with a wrong clock, or a
    // copy that preserved such a timestamp — can never look older than a
    // thumbnail written from the current clock, so it would be judged stale on
    // every scan for the rest of time. Such a timestamp carries no ordering
    // information, so fall back to the current clock with a grace window that
    // absorbs the lag between writing a thumbnail and comparing against it.
    // Files with a plausible mtime are compared exactly as before, preserving
    // the self-healing behaviour for in-flight uploads.
    let now = SystemTime::now();
    let reference = if src_mtime > now {
        now.checked_sub(UNTRUSTED_MTIME_GRACE).unwrap_or(SystemTime::UNIX_EPOCH)
    } else {
        src_mtime
    };

    match std::fs::metadata(&thumb_path) {
        Ok(t) => file_mtime(&t) >= reference,
        Err(_) => false, // no thumbnail
    }
}

/// Whether a metadata row still needs (re)writing.
///
/// A missing row always does. A video row with no duration does too: durations
/// were not recorded by earlier builds, and the API documents a duration for
/// videos, so such rows are repaired on the next scan rather than being written
/// off as complete. Nothing else counts as incomplete — regenerating thumbnails
/// to refresh a stale value would be far more expensive than the value is
/// worth.
fn metadata_incomplete(meta: Option<PhotoMeta>, fname: &str) -> bool {
    match meta {
        None => true,
        // Whether a video has a duration is decided by `probed`, not by the
        // duration being `None`. Containers exist that never expose a container
        // duration (a streamed or linear-muxed Matroska/WebM write, which is
        // what `ffmpeg -f matroska -` and some cameras produce), so treating
        // `None` as "needs work" meant probing such a file forever: on every
        // startup, on every scan, and again on every unrelated event for that
        // folder. `probed` records that a probe ran and its answer was stored,
        // so the row settles after one attempt — while rows written before the
        // column existed still get their one repair pass.
        Some(meta) => {
            let _ = meta.duration;
            util::is_video_file(fname) && !meta.probed
        }
    }
}

/// Portable mtime accessor (`modified()` works on all supported platforms).
fn file_mtime(meta: &std::fs::Metadata) -> SystemTime {
    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)
}

/// A probed duration, in whole seconds, never zero.
///
/// The frontend treats `0` as "unknown" (it is falsy), so a clip shorter than
/// half a second would silently lose its duration badge.
fn whole_seconds(duration_secs: f64) -> u64 {
    (duration_secs.round() as u64).max(1)
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
    if thumb_is_fresh(root, rel_path, file_mtime(&meta))
        && !metadata_incomplete(db.get_metadata(rel_path), &fname)
    {
        return;
    }

    let parent = src.parent().unwrap();
    let thumbs_dir = parent.join("thumbs");
    let thumb_path = thumbs_dir.join(util::thumb_name(&fname));

    if thumb_is_fresh(root, rel_path, file_mtime(&meta)) {
        // Thumbnail is up to date but the metadata row is missing or partial
        // (e.g. a video whose duration predates it being recorded). Fill it in
        // without regenerating the thumbnail.
        update_metadata(root, db, rel_path, &src);
        return;
    }

    info!("Generating thumbnail for {}", rel_path);

    if util::is_image_file(&fname) {
        match thumb::generate_image_thumb(&src, &thumb_path) {
            Ok((w, h)) => {
                let modified = get_mtime(&src);
                let _ = db.set_metadata(rel_path, w, h, None, modified, true);
            }
            Err(e) => {
                warn!("Failed to generate image thumb for {}: {}", rel_path, e);
            }
        }
    } else if util::is_video_file(&fname) {
        // One probe gives both the poster-frame seek offset and the dimensions
        // and duration to store, so the worker and the API agree on one reading.
        let info = thumb::probe_video(&src);
        match thumb::generate_video_thumb(&src, &thumb_path, info.and_then(|i| i.duration_secs)) {
            Ok(()) => {
                // `info` is `None` when `ffprobe` could not answer at all
                // (missing binary, unreadable container). The row is then left
                // unwritten, so the file stays "unprobed" and is retried once
                // the tool is available rather than being written off.
                if let Some(info) = info {
                    let _ = db.set_metadata(
                        rel_path,
                        info.width,
                        info.height,
                        info.duration_secs.map(whole_seconds),
                        get_mtime(&src),
                        true,
                    );
                }
            }
            Err(e) => {
                warn!("Failed to generate video thumb for {}: {}", rel_path, e);
            }
        }
    }
}

/// Synchronous cleanup for a Remove event.
///
/// The event covers either a single media file or a whole folder. A folder
/// removal is the awkward case: the files that were inside it are gone, so
/// their rows have to be found by path prefix rather than visited individually.
fn process_delete(root: &Path, db: &Db, rel_path: String) {
    let path = std::path::Path::new(&rel_path);
    let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();

    if util::is_media_file(fname) {
        thumb::delete_thumb(root, &rel_path);

        // If a photo was deleted, clear any ancestor folder covers that referenced it.
        // Covers store the full relative image path, so we match against rel_path.
        if let Some(parent) = path.parent().and_then(|p| p.to_str()) {
            let _ = db.delete_cover_if_matches(parent, &rel_path);
        }
        // Also check all ancestor folders up the tree. The album root is the
        // empty path, which `parent()` only yields for a file sitting directly
        // in the root, so it has to be checked separately: a root cover chosen
        // from a subfolder's photo would otherwise outlive the photo.
        let _ = db.delete_cover_if_matches("", &rel_path);
        let parts: Vec<&str> = rel_path.split('/').collect();
        for i in 1..parts.len().saturating_sub(1) {
            let ancestor = parts[..i].join("/");
            let _ = db.delete_cover_if_matches(&ancestor, &rel_path);
        }
    } else {
        // Not a media file, so this is a removed folder: purge everything the
        // database recorded beneath it. Skipping this left a row (and a cover
        // choice) behind for every photo that was in the folder — invisible,
        // since listings are built from the filesystem, but never reclaimed.
        let _ = db.delete_metadata_under(&rel_path);
        let _ = db.delete_covers_under(&rel_path);
    }

    let _ = db.delete_cover(&rel_path);
    let _ = db.delete_metadata(&rel_path);
}

fn update_metadata(_root: &Path, db: &Db, rel_path: &str, src: &Path) {
    let fname = src.file_name().unwrap_or_default().to_string_lossy();
    let modified = get_mtime(src);
    if util::is_image_file(&fname) {
        // Header read plus EXIF orientation, not a full decode: this path runs
        // for photos whose thumbnail is already current.
        if let Some((w, h)) = thumb::oriented_dimensions(src) {
            let _ = db.set_metadata(rel_path, w, h, None, modified, true);
        }
    } else if util::is_video_file(&fname)
        && let Some(info) = thumb::probe_video(src)
    {
        let _ = db.set_metadata(
            rel_path,
            info.width,
            info.height,
            info.duration_secs.map(whole_seconds),
            modified,
            true,
        );
    } else if util::is_video_file(&fname) {
        warn!(
            "Could not probe {}: ffprobe produced nothing, leaving the row unprobed",
            rel_path
        );
    }
}

/// The same instant [`get_mtime`] records, taken from metadata already in hand.
fn mtime_secs(meta: &std::fs::Metadata) -> i64 {
    file_mtime(meta).duration_since(std::time::SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

fn get_mtime(path: &Path) -> i64 {
    std::fs::metadata(path).map(|m| mtime_secs(&m)).unwrap_or(0)
}

pub fn scan_existing(root: &Path, db: &Db, tx: &mpsc::Sender<ThumbJob>) {
    let _ = walk_dir(root, PathBuf::new(), db, tx);
}

/// Queue thumbnail work for a folder that has just appeared in the tree.
///
/// When a folder is moved or copied into the album the watcher reports the
/// folder itself as one event and says nothing about its contents, so the
/// subtree has to be enumerated for its media files to be picked up at all.
/// Without this they stayed thumbnail-less until the service was restarted.
pub fn scan_subtree(root: &Path, rel: &str, db: &Db, tx: &mpsc::Sender<ThumbJob>) {
    // Never descend into a thumbnails folder; doing so would generate a second
    // generation of thumbnails inside it.
    if rel.is_empty() || rel.split('/').any(|part| part == "thumbs") {
        return;
    }
    info!("Scanning newly appeared folder: {}", rel);
    if let Err(e) = walk_dir(root, PathBuf::from(rel), db, tx) {
        warn!("Failed to scan {}: {}", rel, e);
    }
}

fn walk_dir(
    root: &Path,
    rel: PathBuf,
    db: &Db,
    tx: &mpsc::Sender<ThumbJob>,
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
            // `file_type()` reports what the directory entry *is*, which for a
            // symlink is the link itself rather than its target. That is what
            // makes a self-referential link (`photos/loop -> photos`) visible
            // and skippable: `metadata()` follows the link, reports
            // `is_dir()` for it, and the walk then pushes it and loops forever.
            // Symlinked *files* are still processed, because the metadata()
            // call below resolves them.
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(e) => {
                    warn!("Failed to read file type for {}: {}", entry.path().display(), e);
                    continue;
                }
            };
            let is_symlink = file_type.is_symlink();
            let is_dir = if is_symlink {
                entry.metadata().map(|m| m.is_dir()).unwrap_or(false)
            } else {
                file_type.is_dir()
            };
            let sub_rel = if current_rel.as_os_str().is_empty() {
                PathBuf::from(&*name_str)
            } else {
                current_rel.join(&*name_str)
            };
            if is_dir {
                // A directory reached through a symlink is never descended
                // into, for the loop reason above and because it may point
                // outside the album root entirely.
                if !is_symlink {
                    stack.push(sub_rel);
                }
            } else if util::is_media_file(&name_str) {
                // A symlink whose target is gone can never be thumbnailed or
                // served, but it still carries a media extension. Without this
                // check it is re-queued on every scan for the life of the
                // service, since `thumb_is_fresh` can never become true for it.
                // Only symlinks need the extra `stat`.
                if is_symlink && std::fs::metadata(entry.path()).is_err() {
                    continue;
                }
                let meta = match entry.metadata() {
                    Ok(m) => m,
                    Err(e) => {
                        warn!("Failed to read metadata for {}: {}", entry.path().display(), e);
                        continue;
                    }
                };
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
                if !fresh || metadata_incomplete(db.get_metadata(&rel_str), &name_str) {
                    enqueue(tx, ThumbJob::Create { rel_path: rel_str });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::PhotoMeta;

    fn meta(duration: Option<u64>, probed: bool) -> Option<PhotoMeta> {
        Some(PhotoMeta { width: 1920, height: 1080, duration, modified: 1, probed })
    }

    #[test]
    fn a_video_is_only_reprobed_until_a_probe_has_answered() {
        // No row at all: there is everything to learn, so it needs work.
        assert!(metadata_incomplete(None, "a.mp4"));

        // A legacy row (written before the column existed) is re-probed once.
        assert!(metadata_incomplete(meta(None, false), "a.mp4"));

        // A probed row is finished with, whether or not the container exposed a
        // duration. This is the case that used to be re-probed forever.
        assert!(!metadata_incomplete(meta(None, true), "a.mp4"));
        assert!(!metadata_incomplete(meta(Some(90), true), "a.mp4"));

        // Images have no probe step; an existing row is complete.
        assert!(!metadata_incomplete(meta(None, false), "a.jpg"));
    }

    #[test]
    fn durations_round_but_never_to_zero() {
        assert_eq!(whole_seconds(0.4), 1, "the frontend treats 0 as unknown");
        assert_eq!(whole_seconds(0.0), 1);
        assert_eq!(whole_seconds(0.6), 1);
        assert_eq!(whole_seconds(90.4), 90);
    }

    /// `enqueue` parks when the queue is full, so the assumption that it is safe
    /// to call from the blocking contexts this project uses has to hold: a panic
    /// here would abort the scan or the watcher thread. Nothing else asserts it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn enqueue_parks_from_a_blocking_thread_instead_of_panicking() {
        let (tx, mut rx) = mpsc::channel::<ThumbJob>(1);
        // Fill the queue so the send below must wait for space.
        tx.try_send(ThumbJob::Delete { rel_path: "a.jpg".into() }).unwrap();

        let sender = tx.clone();
        let sender_task = tokio::task::spawn_blocking(move || {
            enqueue(&sender, ThumbJob::Delete { rel_path: "b.jpg".into() });
            "sent"
        });

        // Draining releases the parked sender.
        assert!(rx.recv().await.is_some());
        assert!(rx.recv().await.is_some());
        assert_eq!(sender_task.await.unwrap(), "sent");
    }
}
