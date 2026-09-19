use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use image::metadata::Orientation;
use image::{GenericImageView, ImageReader, imageops::FilterType};
use tracing::warn;

/// Longest edge of a generated thumbnail, in pixels.
const THUMB_MAX_DIM: u32 = 400;

/// How long `ffprobe` may take to read a container's metadata.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// How long `ffmpeg` may take to extract one poster frame.
///
/// Both limits exist because the worker is a blocking context holding one of
/// only 2-8 generation permits, and there is no way to cancel a synchronous
/// `Command::output()` from outside it. A malformed container or an
/// unresponsive network mount would otherwise hold a permit for the lifetime of
/// the process, so a handful of bad files could stop thumbnail generation
/// permanently — with nothing for systemd's `Restart=always` to react to,
/// because the process has not died.
const EXTRACT_TIMEOUT: Duration = Duration::from_secs(30);

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

/// Run a child process to completion, killing it if it outlives `timeout`.
///
/// Same contract as [`Command::output`] — stdin closed, both output pipes
/// captured — with a deadline, so one pathological input cannot pin a worker
/// permit forever. Both pipes are drained on their own threads: a child that
/// writes more than a pipe buffer while we are only polling `try_wait()` would
/// otherwise block on its next write and never exit, which would defeat the
/// timeout entirely.
fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> anyhow::Result<Output> {
    use std::io::Read;

    /// Drain a pipe on its own thread so the child can never block on a full
    /// pipe buffer while we are only polling `try_wait()`.
    fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    }

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout_drain = drain(child.stdout.take());
    let stderr_drain = drain(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            // Kill, then reap: the pipes close, so the draining threads finish
            // and the joins below cannot block.
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    let stdout = stdout_drain.join().unwrap_or_default();
    let stderr = stderr_drain.join().unwrap_or_default();

    match status {
        Some(status) => Ok(Output { status, stdout, stderr }),
        None => anyhow::bail!("process exceeded the {timeout:?} timeout and was killed"),
    }
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
///
/// The dimensions returned are the *display* dimensions. Phone cameras store a
/// landscape frame plus a rotation matrix ("rotate 90°"), and ffmpeg applies
/// that rotation when it extracts the poster frame — so reporting the coded
/// width/height here would label every portrait video as landscape while the
/// thumbnail next to it is correct. The rotation lives in the stream's side
/// data (`rotation`, from the Display Matrix) with the older `rotate` tag as a
/// fallback, so both are requested.
pub fn probe_video(src: &Path) -> Option<VideoInfo> {
    let mut cmd = Command::new("ffprobe");
    cmd.args([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=width,height:stream_side_data=rotation:stream_tags=rotate:format=duration",
        "-of",
        "json",
    ])
    .arg(src);

    let output = run_with_timeout(&mut cmd, PROBE_TIMEOUT).ok()?;

    if !output.status.success() {
        return None;
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let stream = json.get("streams")?.get(0)?;
    let (width, height) = display_dimensions(stream)?;

    let duration_secs = json
        .get("format")
        .and_then(|format| format.get("duration"))
        .and_then(|duration| duration.as_str())
        .and_then(|duration| duration.parse::<f64>().ok())
        .filter(|d| d.is_finite() && *d > 0.0);

    Some(VideoInfo {
        width,
        height,
        duration_secs,
    })
}

/// The dimensions a viewer sees for one ffprobe stream object: the coded size
/// with width and height swapped when the display rotation is a quarter turn.
fn display_dimensions(stream: &serde_json::Value) -> Option<(u32, u32)> {
    let width = stream.get("width")?.as_u64()? as u32;
    let height = stream.get("height")?.as_u64()? as u32;
    if is_quarter_turn(stream) {
        Some((height, width))
    } else {
        Some((width, height))
    }
}

/// Whether a stream carries a 90°/270° display rotation.
///
/// Both spellings are read: modern containers carry a Display Matrix in the
/// stream's side data (`rotation`), which is what current ffprobe versions
/// expose, while older files use a `rotate` tag. A rotation of 180° leaves the
/// aspect ratio alone, so only quarter turns count.
fn is_quarter_turn(stream: &serde_json::Value) -> bool {
    fn degrees(value: &serde_json::Value) -> Option<f64> {
        value
            .as_f64()
            .or_else(|| value.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
    }

    let side_data = stream
        .get("side_data_list")
        .and_then(|list| list.as_array())
        .and_then(|list| list.iter().find_map(|entry| entry.get("rotation").and_then(degrees)));

    let tag = stream
        .get("tags")
        .and_then(|tags| tags.get("rotate"))
        .and_then(degrees);

    match side_data.or(tag) {
        // `rem_euclid` maps the -90 that some writers use to 270.
        Some(rotation) => {
            let normalized = rotation.rem_euclid(360.0);
            (normalized - 90.0).abs() < 1.0 || (normalized - 270.0).abs() < 1.0
        }
        None => false,
    }
}

/// Extract a poster frame for a video.
///
/// `duration_secs` comes from [`probe_video`]; the frame is taken 10% into the
/// clip, because the opening second is frequently a title card, a fade from
/// black, or nothing at all. `-ss` before `-i` is a fast seek, so it lands on
/// the nearest preceding keyframe rather than decoding from the start. When the
/// duration is unknown the frame is taken from the very start: a fixed one
/// second is wrong for a clip shorter than that, where the seek lands past the
/// last frame and ffmpeg fails, leaving the file thumbless until it next
/// changes.
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
        // Unknown duration: the first frame is the only offset guaranteed to be
        // inside the clip, however short it is.
        None => 0.0,
    };
    let seek_arg = format!("{seek:.3}");

    // Paths are passed as `OsStr` arguments rather than `to_str().unwrap()`: a
    // filename that is not valid UTF-8 used to panic the worker thread.
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-ss", seek_arg.as_str(), "-i"])
        .arg(src)
        .args(["-vframes", "1", "-q:v", "2", "-f", "image2", "-y"])
        .arg(&tmp);

    let output = run_with_timeout(&mut cmd, EXTRACT_TIMEOUT)?;

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
    fn quarter_turn_rotations_swap_dimensions() {
        let rotated = serde_json::json!({
            "width": 320,
            "height": 240,
            "side_data_list": [{"rotation": 90}]
        });
        assert_eq!(display_dimensions(&rotated), Some((240, 320)));

        // Negative and 270° spellings are the same quarter turn.
        for rotation in [-90.0, 270.0, -270.0] {
            let stream = serde_json::json!({
                "width": 320,
                "height": 240,
                "side_data_list": [{"rotation": rotation}]
            });
            assert_eq!(display_dimensions(&stream), Some((240, 320)), "rotation {rotation}");
        }

        // The older `rotate` tag is honoured when there is no side data.
        let tagged = serde_json::json!({
            "width": 320,
            "height": 240,
            "tags": {"rotate": "90"}
        });
        assert_eq!(display_dimensions(&tagged), Some((240, 320)));
    }

    #[test]
    fn non_quarter_turn_rotations_keep_dimensions() {
        for rotation in [0, 180, -180] {
            let stream = serde_json::json!({
                "width": 320,
                "height": 240,
                "side_data_list": [{"rotation": rotation}]
            });
            assert_eq!(display_dimensions(&stream), Some((320, 240)), "rotation {rotation}");
        }
        // No rotation information at all is the common case.
        let plain = serde_json::json!({"width": 1920, "height": 1080});
        assert_eq!(display_dimensions(&plain), Some((1920, 1080)));
    }

    #[test]
    fn missing_dimensions_are_reported_as_none() {
        let stream = serde_json::json!({"width": 320});
        assert_eq!(display_dimensions(&stream), None);
    }

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

    // Unix-only because the helpers used as stand-in children (`true`, `sleep`)
    // are not guaranteed to exist on Windows.
    #[cfg(unix)]
    #[test]
    fn run_with_timeout_returns_output_for_a_fast_child() {
        let mut cmd = Command::new("echo");
        cmd.arg("hello");
        let out = run_with_timeout(&mut cmd, Duration::from_secs(10)).unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");
    }

    #[cfg(unix)]
    #[test]
    fn run_with_timeout_kills_a_child_that_overruns() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let started = Instant::now();
        let err = run_with_timeout(&mut cmd, Duration::from_millis(300)).unwrap_err();
        // It must give up promptly rather than waiting out the child.
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(err.to_string().contains("timeout"), "unexpected error: {err}");
    }
}
