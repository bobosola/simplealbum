use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use image::{GenericImageView, imageops::FilterType};
use tracing::warn;

/// Monotonic per-process counter for unique temp filenames. Duplicate jobs
/// for the same source (inotify emits several events per in-flight write,
/// and more than one can pass the stability gate before the first rename
/// lands) must not share a temp file: they would rename it out from under
/// each other and one job would fail with ENOENT.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Unique hidden temp path alongside `dst` (e.g. `.7.beach_thumb.jpg.tmp`).
/// The dot prefix keeps it out of listings and of the startup scan, and it
/// lives inside `thumbs/`, so watcher events for it are filtered. An orphan
/// can only remain after a hard crash mid-write; it is harmless.
fn tmp_path(dst: &Path) -> PathBuf {
    let n = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let fname = dst.file_name().unwrap_or_default().to_string_lossy();
    dst.with_file_name(format!(".{}.{}.tmp", n, fname))
}

pub fn generate_image_thumb(src: &Path, dst: &Path) -> anyhow::Result<(u32, u32)> {
    let img = image::open(src)?;
    let (orig_w, orig_h) = img.dimensions();

    // Read EXIF orientation
    let orientation = read_exif_orientation(src);
    let img = apply_orientation(img, orientation);

    let max_dim = 400;
    let thumb = if orig_w > max_dim || orig_h > max_dim {
        img.resize(max_dim, max_dim, FilterType::Lanczos3)
    } else {
        img
    };

    std::fs::create_dir_all(dst.parent().unwrap())?;
    // Atomic write: temp file then rename so a crash never leaves a partial thumb.
    let tmp = tmp_path(dst);
    thumb.save_with_format(&tmp, image::ImageFormat::Jpeg)?;

    // Sanity check: a 400×300 RGB JPEG should not be under 1KB.
    // If it is, the decoder likely produced garbage pixels (e.g. solid grey).
    if let Ok(meta) = std::fs::metadata(&tmp) {
        if meta.len() < 1024 {
            let _ = std::fs::remove_file(&tmp);
            anyhow::bail!(
                "Generated thumbnail is suspiciously small ({} bytes) — probable decoder failure. Source: {}",
                meta.len(),
                src.display()
            );
        }
    }

    std::fs::rename(&tmp, dst)?;

    Ok((orig_w, orig_h))
}

pub fn generate_video_thumb(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst.parent().unwrap())?;
    // Atomic write: temp file then rename so a crash never leaves a partial thumb.
    let tmp = tmp_path(dst);

    let output = Command::new("ffmpeg")
        .args(&[
            "-ss", "00:00:01",
            "-i", src.to_str().unwrap(),
            "-vframes", "1",
            "-q:v", "2",
            "-f", "image2",
            "-y",
            tmp.to_str().unwrap(),
        ])
        .output()?;

    if !output.status.success() {
        let _ = std::fs::remove_file(&tmp);
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ffmpeg failed: {}", stderr);
    }

    // Resize the extracted frame to max 400px
    if let Ok(img) = image::open(&tmp) {
        let max_dim = 400;
        let (w, h) = img.dimensions();
        if w > max_dim || h > max_dim {
            let thumb = img.resize(max_dim, max_dim, FilterType::Lanczos3);
            thumb.save_with_format(&tmp, image::ImageFormat::Jpeg)?;
        }
    }

    std::fs::rename(&tmp, dst)?;
    Ok(())
}

fn read_exif_orientation(path: &Path) -> u32 {
    let Ok(file) = std::fs::File::open(path) else { return 1 };
    let Ok(reader) = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(&file)) else { return 1 };
    if let Some(field) = reader.get_field(exif::Tag::Orientation, exif::In::PRIMARY) {
        if let Some(v) = field.value.get_uint(0) {
            return v;
        }
    }
    1
}

fn apply_orientation(img: image::DynamicImage, orientation: u32) -> image::DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.fliph().rotate180(),
        5 => img.fliph().rotate90(),
        6 => img.rotate90(),
        7 => img.fliph().rotate270(),
        8 => img.rotate270(),
        _ => img,
    }
}

pub fn get_video_dimensions(path: &Path) -> Option<(u32, u32)> {
    let output = Command::new("ffprobe")
        .args(&[
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream=width,height",
            "-of", "csv=s=x:p=0",
            path.to_str()?,
        ])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.trim().split('x').collect();
    if parts.len() == 2 {
        let w = parts[0].parse().ok()?;
        let h = parts[1].parse().ok()?;
        return Some((w, h));
    }
    None
}

pub fn delete_thumb(root: &Path, rel: &str) {
    let thumbs_dir = root.join(rel).parent().map(|p| p.join("thumbs"));
    if let Some(thumbs_dir) = thumbs_dir {
        let name = Path::new(rel).file_name().unwrap_or_default().to_string_lossy();
        let thumb_name = super::util::thumb_name(&name);
        let thumb_path = thumbs_dir.join(&thumb_name);
        if thumb_path.exists() {
            if let Err(e) = std::fs::remove_file(&thumb_path) {
                warn!("Failed to delete thumb {}: {}", thumb_path.display(), e);
            }
        }
        // Clean up empty thumbs dir
        if let Ok(entries) = std::fs::read_dir(&thumbs_dir) {
            if entries.count() == 0 {
                let _ = std::fs::remove_dir(&thumbs_dir);
            }
        }
    }
}
