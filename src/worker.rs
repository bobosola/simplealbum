use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, Semaphore};
use image::GenericImageView;
use tracing::{info, warn};

use crate::{config::Config, db::Db, thumb, util};

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

        // Limit concurrent thumbnail jobs to avoid exhausting RAM when
        // processing large collections (each image::open loads the full
        // decoded image into memory).
        let concurrency = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 8);
        let semaphore = Arc::new(Semaphore::new(concurrency));

        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                let permit = semaphore.clone().acquire_owned().await.unwrap();
                let root = root.clone();
                let db = db.clone();
                tokio::task::spawn_blocking(move || {
                    let _permit = permit; // hold until job finishes
                    process_job(&root, &db, job);
                });
            }
        });

        Worker { tx }
    }
}

fn process_job(root: &Path, db: &Db, job: ThumbJob) {
    match job {
        ThumbJob::Create { rel_path } => {
            let src = root.join(&rel_path);
            if !src.exists() {
                return;
            }

            let fname = src.file_name().unwrap_or_default().to_string_lossy();
            if !util::is_media_file(&fname) {
                return;
            }

            let parent = src.parent().unwrap();
            let thumbs_dir = parent.join("thumbs");
            let thumb_name = util::thumb_name(&fname);
            let thumb_path = thumbs_dir.join(&thumb_name);

            if thumb_path.exists() {
                // Still update metadata if missing
                if db.get_metadata(&rel_path).is_none() {
                    update_metadata(root, db, &rel_path, &src);
                }
                return;
            }

            info!("Generating thumbnail for {}", rel_path);

            if util::is_image_file(&fname) {
                match thumb::generate_image_thumb(&src, &thumb_path) {
                    Ok((w, h)) => {
                        let modified = get_mtime(&src);
                        let _ = db.set_metadata(&rel_path, w, h, modified);
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
                            let _ = db.set_metadata(&rel_path, w, h, modified);
                        }
                    }
                    Err(e) => {
                        warn!("Failed to generate video thumb for {}: {}", rel_path, e);
                    }
                }
            }
        }
        ThumbJob::Delete { rel_path } => {
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
                if db.get_metadata(&rel_str).is_none() || !has_thumb(root, &sub_rel) {
                    let _ = tx.send(ThumbJob::Create { rel_path: rel_str });
                }
            }
        }
    }
    Ok(())
}

fn has_thumb(root: &Path, rel: &Path) -> bool {
    let src = root.join(rel);
    let parent = src.parent().unwrap();
    let thumbs_dir = parent.join("thumbs");
    let fname = src.file_name().unwrap_or_default().to_string_lossy();
    let thumb_name = util::thumb_name(&fname);
    let path = thumbs_dir.join(thumb_name);
    path.exists() && path.metadata().map(|m| m.len() > 0).unwrap_or(false)
}
