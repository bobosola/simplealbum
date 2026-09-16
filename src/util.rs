use std::path::{Component, Path, PathBuf};

/// Validate a relative path supplied by a client.
///
/// Returns `None` for anything that could address a location outside the album
/// root: `..` segments, and absolute paths or Windows drive prefixes. The
/// absolute cases matter because `Path::join` *replaces* the base with an
/// absolute argument, so `root.join("/etc/passwd")` is `/etc/passwd`, not a
/// path inside the root. Callers join the returned value to the album root, so
/// accepting one here would silently escape the tree on the unguarded call
/// paths (`thumb::delete_thumb`, `api::set_cover`).
pub fn validate_path(input: &str) -> Option<String> {
    if input.is_empty() {
        return Some(String::new());
    }
    for comp in Path::new(input).components() {
        match comp {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Some(input.to_string())
}

pub fn resolve_album_path(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel = validate_path(rel)?;
    let joined = root.join(&rel);
    let canonical = std::fs::canonicalize(&joined).ok()?;
    let root_canonical = std::fs::canonicalize(root).ok()?;
    if canonical.starts_with(&root_canonical) {
        Some(canonical)
    } else {
        None
    }
}

pub fn is_ancestor(parent: &str, child: &str) -> bool {
    if parent.is_empty() {
        return true;
    }
    let parent = parent.trim_end_matches('/');
    let child = child.trim_end_matches('/');
    child.starts_with(parent) && (child.len() == parent.len() || child[parent.len()..].starts_with('/'))
}

pub fn is_media_file(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "webp" | "mp4" | "mov" | "avi" | "webm" | "mkv")
}

pub fn is_image_file(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "webp")
}

pub fn is_video_file(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    matches!(ext.as_str(), "mp4" | "mov" | "avi" | "webm" | "mkv")
}

pub fn thumb_name(name: &str) -> String {
    let p = Path::new(name);
    let stem = p.file_stem().unwrap_or_default().to_string_lossy();
    let ext = p.extension().unwrap_or_default().to_string_lossy().to_lowercase();
    if is_video_file(name) {
        format!("{}_thumb.jpg", stem)
    } else {
        format!("{}_thumb.{}", stem, ext)
    }
}

pub fn media_type(name: &str) -> &'static str {
    if is_video_file(name) { "video" } else { "image" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_path_accepts_relative_paths() {
        assert_eq!(validate_path(""), Some(String::new()));
        assert_eq!(
            validate_path("1970-79/1970/summer holiday"),
            Some("1970-79/1970/summer holiday".to_string())
        );
        // A doubledot *inside* a name is a normal character, not traversal.
        assert_eq!(validate_path("a..b/c"), Some("a..b/c".to_string()));
        assert_eq!(validate_path("."), Some(".".to_string()));
    }

    #[test]
    fn validate_path_rejects_traversal_and_absolute_paths() {
        assert_eq!(validate_path(".."), None);
        assert_eq!(validate_path("../secret"), None);
        assert_eq!(validate_path("1970/../../etc"), None);
        assert_eq!(validate_path("/etc/passwd"), None);
        assert_eq!(validate_path("/"), None);
        assert_eq!(validate_path("//server/share"), None);
    }

    #[cfg(windows)]
    #[test]
    fn validate_path_rejects_windows_drive_prefixes() {
        assert_eq!(validate_path("C:/Windows"), None);
        assert_eq!(validate_path("C:\\Windows"), None);
    }

    #[test]
    fn is_ancestor_matches_on_path_boundaries_only() {
        assert!(is_ancestor("", "1970-79/1970"));
        assert!(is_ancestor("1970-79", "1970-79/1970"));
        assert!(is_ancestor("1970-79", "1970-79"));
        // A prefix that is not a parent folder must not match.
        assert!(!is_ancestor("1970", "19700"));
        assert!(!is_ancestor("1970-79/1970", "1970-79"));
    }

    #[test]
    fn thumb_names_are_derived_from_the_stem() {
        assert_eq!(thumb_name("beach.JPG"), "beach_thumb.jpg");
        assert_eq!(thumb_name("logo.png"), "logo_thumb.png");
        assert_eq!(thumb_name("picture.webp"), "picture_thumb.webp");
        // Videos always get a JPEG frame, whatever the container.
        assert_eq!(thumb_name("clip.MOV"), "clip_thumb.jpg");
        assert_eq!(thumb_name("holiday.mkv"), "holiday_thumb.jpg");
    }

    #[test]
    fn media_types_are_classified_by_extension() {
        assert!(is_media_file("a.jpg"));
        assert!(is_media_file("a.MP4"));
        assert!(!is_media_file("notes.txt"));
        assert!(!is_media_file("noextension"));
        assert_eq!(media_type("a.jpg"), "image");
        assert_eq!(media_type("a.mp4"), "video");
    }
}
