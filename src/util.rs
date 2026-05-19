use std::path::{Path, PathBuf};

pub fn validate_path(input: &str) -> Option<String> {
    if input.is_empty() {
        return Some(String::new());
    }
    let p = Path::new(input);
    for comp in p.components() {
        if let std::path::Component::ParentDir = comp {
            return None;
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
