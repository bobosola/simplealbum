use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use image::metadata::Orientation;
use image::{GenericImageView, ImageReader, imageops::FilterType};
use tracing::warn;

/// Longest edge of a generated thumbnail, in pixels.
const THUMB_MAX_DIM: u32 = 400;

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

/// The source file's EXIF orientation, or `NoTransforms` when there is none.
///
/// The `image` crate deliberately does not apply EXIF orientation while
/// decoding (it is metadata, not pixels), so it has to be read separately and
/// applied explicitly. PNG and WebP carry no EXIF orientation, and for those
/// the container read simply fails and the default is returned.
fn exif_orientation(path: &Path) -> Orientation {
    let Ok(file) = std::fs::File::open(path) else {
        return Orientation::NoTransforms;
    };
    let Ok(reader) = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(&file))
    else {
        return Orientation::NoTransforms;
    };
    reader
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|field| field.value.get_uint(0))
        .and_then(|value| Orientation::from_exif(value as u8))
        .unwrap_or(Orientation::NoTransforms)
}

/// Encoder matching a thumbnail's file extension.
///
/// Thumbnails keep the source extension (see `util::thumb_name`), so the bytes
/// written must match it. Encoding everything as JPEG would leave a PNG or WebP
/// thumbnail containing JPEG bytes, which Caddy then serves with the *source*
/// MIME type — and it would also throw away the transparency a PNG source may
/// legitimately have.
fn encoder_for(ext: &str) -> image::ImageFormat {
    match ext {
        "png" => image::ImageFormat::Png,
        "webp" => image::ImageFormat::WebP,
        _ => image::ImageFormat::Jpeg,
    }
}

/// Display dimensions of an image: its dimensions *after* EXIF orientation is
/// applied.
///
/// A portrait photo from a phone is usually stored landscape with an EXIF flag
/// saying "rotate 90°", so the raw pixel dimensions are the transpose of what
/// the viewer sees. Only the header is read here (no pixels are decoded), which
/// makes this cheap enough to use as a metadata repair path as well.
pub fn oriented_dimensions(src: &Path) -> Option<(u32, u32)> {
    let (width, height) = ImageReader::open(src).ok()?.into_dimensions().ok()?;
    Some(match exif_orientation(src) {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH => (height, width),
        _ => (width, height),
    })
}

pub fn generate_image_thumb(src: &Path, dst: &Path) -> anyhow::Result<(u32, u32)> {
    let mut img = image::open(src)?;
    img.apply_orientation(exif_orientation(src));

    // Read the dimensions *after* orienting: these are the dimensions the user
    // sees, and they are what gets stored and shown in the UI. Reading them
    // before the rotation reported landscape dimensions for portrait photos.
    let (width, height) = img.dimensions();

    let thumb = if width > THUMB_MAX_DIM || height > THUMB_MAX_DIM {
        img.resize(THUMB_MAX_DIM, THUMB_MAX_DIM, FilterType::Lanczos3)
    } else {
        img
    };

    let ext = dst
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let format = encoder_for(&ext);

    std::fs::create_dir_all(dst.parent().unwrap())?;
    // Atomic write: temp file then rename so a crash never leaves a partial thumb.
    let tmp = tmp_path(dst);
    thumb.save_with_format(&tmp, format)?;

    // Sanity check for JPEG output only: a 400x300 JPEG should not be under 1KB,
    // so anything smaller means the decoder produced garbage pixels (e.g. solid
    // grey). Lossless formats compress flat or tiny images below that threshold
    // legitimately, so applying the check to them would cause false failures.
    if format == image::ImageFormat::Jpeg
        && let Ok(meta) = std::fs::metadata(&tmp)
        && meta.len() < 1024
    {
        let _ = std::fs::remove_file(&tmp);
        anyhow::bail!(
            "Generated thumbnail is suspiciously small ({} bytes) — probable decoder failure. Source: {}",
            meta.len(),
            src.display()
        );
    }

    std::fs::rename(&tmp, dst)?;

    Ok((width, height))
}

/// What `ffprobe` reports about a video: the display dimensions and, when the
/// container exposes one, the duration.
#[derive(Debug, Clone, Copy)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub duration_secs: Option<f64>,
}

/// Read a video's dimensions and duration with a single `ffprobe` call.
///
/// Duration comes from the *format* section (the container) rather than the
/// video stream: both are usually present, but the container value is the one
/// that covers the whole clip, which is what a "10% in" seek needs. A missing
/// or unparseable duration is not an error — dimensions alone are still useful.
pub fn probe_video(src: &Path) -> Option<VideoInfo> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height:format=duration",
            "-of",
            "json",
        ])
        .arg(src)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let stream = json.get("streams")?.get(0)?;

    let duration_secs = json
        .get("format")
        .and_then(|format| format.get("duration"))
        .and_then(|duration| duration.as_str())
        .and_then(|duration| duration.parse::<f64>().ok())
        .filter(|d| d.is_finite() && *d > 0.0);

    Some(VideoInfo {
        width: stream.get("width")?.as_u64()? as u32,
        height: stream.get("height")?.as_u64()? as u32,
        duration_secs,
    })
}

/// Extract a poster frame for a video.
///
/// `duration_secs` comes from [`probe_video`]; the frame is taken 10% into the
/// clip, because the opening second is frequently a title card, a fade from
/// black, or nothing at all. `-ss` before `-i` is a fast seek, so it lands on
/// the nearest preceding keyframe rather than decoding from the start. When the
/// duration is unknown, one second is the fallback.
pub fn generate_video_thumb(
    src: &Path,
    dst: &Path,
    duration_secs: Option<f64>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst.parent().unwrap())?;
    // Atomic write: temp file then rename so a crash never leaves a partial thumb.
    let tmp = tmp_path(dst);

    let seek = match duration_secs {
        // Keep the seek inside the clip: a 0.4s video must not be seeked to 0.4s
        // and land past the last frame.
        Some(duration) => (duration * 0.1).clamp(0.0, (duration - 0.1).max(0.0)),
        None => 1.0,
    };
    let seek_arg = format!("{seek:.3}");

    // Paths are passed as `OsStr` arguments rather than `to_str().unwrap()`: a
    // filename that is not valid UTF-8 used to panic the worker thread.
    let output = Command::new("ffmpeg")
        .args(["-ss", seek_arg.as_str(), "-i"])
        .arg(src)
        .args(["-vframes", "1", "-q:v", "2", "-f", "image2", "-y"])
        .arg(&tmp)
        .output()?;

    if !output.status.success() {
        let _ = std::fs::remove_file(&tmp);
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ffmpeg failed: {}", stderr);
    }

    // Resize the extracted frame to max 400px
    if let Ok(img) = image::open(&tmp) {
        let (w, h) = img.dimensions();
        if w > THUMB_MAX_DIM || h > THUMB_MAX_DIM {
            let thumb = img.resize(THUMB_MAX_DIM, THUMB_MAX_DIM, FilterType::Lanczos3);
            thumb.save_with_format(&tmp, image::ImageFormat::Jpeg)?;
        }
    }

    std::fs::rename(&tmp, dst)?;
    Ok(())
}

pub fn delete_thumb(root: &Path, rel: &str) {
    let thumbs_dir = root.join(rel).parent().map(|p| p.join("thumbs"));
    if let Some(thumbs_dir) = thumbs_dir {
        let name = Path::new(rel).file_name().unwrap_or_default().to_string_lossy();
        let thumb_name = super::util::thumb_name(&name);
        let thumb_path = thumbs_dir.join(&thumb_name);
        if thumb_path.exists()
            && let Err(e) = std::fs::remove_file(&thumb_path)
        {
            warn!("Failed to delete thumb {}: {}", thumb_path.display(), e);
        }
        // Clean up empty thumbs dir
        if let Ok(entries) = std::fs::read_dir(&thumbs_dir)
            && entries.count() == 0
        {
            let _ = std::fs::remove_dir(&thumbs_dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_matches_the_thumbnail_extension() {
        assert_eq!(encoder_for("png"), image::ImageFormat::Png);
        assert_eq!(encoder_for("webp"), image::ImageFormat::WebP);
        assert_eq!(encoder_for("jpg"), image::ImageFormat::Jpeg);
        assert_eq!(encoder_for("jpeg"), image::ImageFormat::Jpeg);
        assert_eq!(encoder_for(""), image::ImageFormat::Jpeg);
    }

    #[test]
    fn orientation_rotations_are_recognised() {
        // Values 5-8 are the ones that transpose the image.
        for exif in [5u8, 6, 7, 8] {
            let orientation = Orientation::from_exif(exif).unwrap();
            assert!(matches!(
                orientation,
                Orientation::Rotate90
                    | Orientation::Rotate270
                    | Orientation::Rotate90FlipH
                    | Orientation::Rotate270FlipH
            ));
        }
        // 1-4 never change the aspect ratio.
        for exif in [1u8, 2, 3, 4] {
            let orientation = Orientation::from_exif(exif).unwrap();
            assert!(matches!(
                orientation,
                Orientation::NoTransforms
                    | Orientation::FlipHorizontal
                    | Orientation::Rotate180
                    | Orientation::FlipVertical
            ));
        }
    }

    #[test]
    fn tmp_paths_are_unique_and_hidden_next_to_the_target() {
        let dst = Path::new("/album/1970/thumbs/beach_thumb.jpg");
        let a = tmp_path(dst);
        let b = tmp_path(dst);
        assert_ne!(a, b);
        assert_eq!(a.parent(), dst.parent());
        assert!(a.file_name().unwrap().to_string_lossy().starts_with('.'));
    }
}
