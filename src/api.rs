use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{info, warn};

use crate::{
    config::Config,
    db::Db,
    models::{AlbumResponse, Breadcrumb, FolderItem, PhotoItem, SetCoverRequest},
    util,
};

pub struct AppState {
    pub config: Config,
    pub db: std::sync::Arc<Db>,
}

#[derive(Deserialize)]
pub struct AlbumQuery {
    #[serde(default)]
    pub path: String,
}

pub async fn get_album(
    Query(query): Query<AlbumQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<AlbumResponse>, StatusCode> {
    let rel_path = util::validate_path(&query.path).ok_or(StatusCode::BAD_REQUEST)?;
    let abs_path = util::resolve_album_path(&state.config.album.root, &rel_path)
        .ok_or(StatusCode::BAD_REQUEST)?;

    if !abs_path.is_dir() {
        return Err(StatusCode::NOT_FOUND);
    }

    let name = abs_path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Home")
        .to_string();

    let breadcrumbs = build_breadcrumbs(&rel_path);

    let mut folders = vec![];
    let mut photos = vec![];

    let entries = std::fs::read_dir(&abs_path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by(|a, b| {
        let an = a.file_name();
        let bn = b.file_name();
        an.cmp(&bn)
    });

    for entry in entries {
        let fname = entry.file_name();
        let fname_str = fname.to_string_lossy();
        if fname_str.starts_with('.') || fname_str == "thumbs" {
            continue;
        }
        let meta = entry.metadata();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);

        if is_dir {
            let sub_path = if rel_path.is_empty() {
                fname_str.to_string()
            } else {
                format!("{}/{}", rel_path, fname_str)
            };
            let (count_photos, count_albums) = count_contents(&state.config.album.root, &sub_path);
            let cover = state.db.get_cover(&sub_path)
                .filter(|full_path| {
                    // Verify the cover image still exists (wasn't deleted)
                    let full = state.config.album.root.join(full_path);
                    full.exists()
                })
                .and_then(|full_path| compute_cover_thumb(&sub_path, &full_path))
                .or_else(|| find_first_thumb_recursive(&state.config.album.root, &sub_path));
            folders.push(FolderItem {
                name: fname_str.to_string(),
                path: sub_path,
                cover,
                count_photos,
                count_albums,
            });
        } else if util::is_media_file(&fname_str) {
            let photo_rel = if rel_path.is_empty() {
                fname_str.to_string()
            } else {
                format!("{}/{}", rel_path, fname_str)
            };
            let (width, height) = state.db.get_metadata(&photo_rel)
                .map(|(w, h, _)| (w, h))
                .unwrap_or((0, 0));
            let thumb = format!("thumbs/{}", util::thumb_name(&fname_str));
            let mtype = util::media_type(&fname_str);
            let duration = if mtype == "video" { Some(0) } else { None };
            photos.push(PhotoItem {
                name: fname_str.to_string(),
                media_type: mtype.to_string(),
                thumb,
                width,
                height,
                duration,
            });
        }
    }

    Ok(Json(AlbumResponse {
        path: rel_path.clone(),
        name,
        breadcrumbs,
        folders,
        photos,
    }))
}

pub async fn set_cover(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<SetCoverRequest>,
) -> Result<StatusCode, StatusCode> {
    let admin_key = headers.get("X-Admin-Key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if admin_key != state.config.admin.key || state.config.admin.key.is_empty() {
        return Err(StatusCode::FORBIDDEN);
    }

    let image_path = util::validate_path(&body.image_path).ok_or(StatusCode::BAD_REQUEST)?;
    let image_abs = util::resolve_album_path(&state.config.album.root, &image_path)
        .ok_or(StatusCode::BAD_REQUEST)?;
    if !image_abs.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    for target in &body.targets {
        let target = util::validate_path(target).ok_or(StatusCode::BAD_REQUEST)?;
        tracing::debug!("set_cover: target={}, image_path={}", target, image_path);
        if !util::is_ancestor(&target, &image_path) {
            warn!(
                "set_cover rejected: target '{}' is not an ancestor of image '{}'",
                target, image_path
            );
            return Err(StatusCode::BAD_REQUEST);
        }
        let target_abs = state.config.album.root.join(&target);
        if !target_abs.is_dir() {
            warn!(
                "set_cover rejected: target '{}' does not exist or is not a directory",
                target
            );
            return Err(StatusCode::NOT_FOUND);
        }
        // Store the full relative image path so covers work across folder levels
        state.db.set_cover(&target, &image_path)
            .map_err(|e| {
                warn!("set_cover database error for target '{}': {}", target, e);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        info!("set_cover: stored cover for '{}' → '{}'", target, image_path);
    }

    Ok(StatusCode::NO_CONTENT)
}

pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

fn build_breadcrumbs(rel_path: &str) -> Vec<Breadcrumb> {
    let mut crumbs = vec![Breadcrumb { name: "Home".to_string(), path: "".to_string() }];
    if rel_path.is_empty() {
        return crumbs;
    }
    let parts: Vec<&str> = rel_path.split('/').collect();
    let mut accum = String::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            accum.push('/');
        }
        accum.push_str(part);
        crumbs.push(Breadcrumb {
            name: part.to_string(),
            path: accum.clone(),
        });
    }
    crumbs
}

fn count_contents(root: &std::path::Path, rel: &str) -> (usize, usize) {
    // Iterative DFS to avoid unbounded recursion.
    // Each stack item is (relative_path, is_top_level).
    // is_top_level is true only for direct children of the queried folder.
    // We count albums only at the top level; photos are counted at all levels.
    let mut stack = vec![(rel.to_string(), true)];
    let mut photos = 0;
    let mut albums = 0;

    while let Some((current_rel, is_top_level)) = stack.pop() {
        let path = root.join(&current_rel);
        let Ok(dir_entries) = std::fs::read_dir(&path) else { continue };
        for entry in dir_entries.filter_map(|e| e.ok()) {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s.starts_with('.') || s == "thumbs" {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                if is_top_level {
                    albums += 1;
                }
                let sub_rel = if current_rel.is_empty() {
                    s.to_string()
                } else {
                    format!("{}/{}", current_rel, s)
                };
                stack.push((sub_rel, false));
            } else if util::is_media_file(&s) {
                photos += 1;
            }
        }
    }

    (photos, albums)
}

/// Find the first available thumbnail in a folder or any of its descendants.
/// Searches the folder itself, then immediate children, then grandchildren.
/// Returns a relative thumbnail path (e.g. "thumbs/photo_thumb.jpg" or
/// "subfolder/thumbs/photo_thumb.jpg") or None.
fn find_first_thumb_recursive(root: &std::path::Path, rel: &str) -> Option<String> {
    let base = root.join(rel);
    let base_canonical = std::fs::canonicalize(&base).unwrap_or(base.clone());

    // Helper: check a single directory for thumbnails.
    // Returns the thumbnail filename (not path) if found.
    let check_dir = |dir: &std::path::Path| -> Option<String> {
        let thumbs_dir = dir.join("thumbs");
        if !thumbs_dir.is_dir() {
            return None;
        }
        let mut entries: Vec<_> = std::fs::read_dir(&thumbs_dir)
            .ok()?
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        for entry in entries {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s.starts_with('.') {
                continue;
            }
            return Some(s.to_string());
        }
        None
    };

    // Helper: build a relative path string from an absolute path under base.
    let to_rel = |abs: &std::path::Path| -> Option<String> {
        let c = std::fs::canonicalize(abs).unwrap_or(abs.to_path_buf());
        c.strip_prefix(&base_canonical)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
    };

    // 1. Check the folder itself
    if let Some(t) = check_dir(&base) {
        return Some(format!("thumbs/{}", t));
    }

    // 2. Check immediate children (sorted)
    let Ok(entries) = std::fs::read_dir(&base) else { return None };
    let mut children: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let s = name.to_string_lossy();
            e.metadata().map(|m| m.is_dir()).unwrap_or(false)
                && !s.starts_with('.')
                && s != "thumbs"
        })
        .collect();
    children.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

    for child in &children {
        let child_path = child.path();
        if let Some(t) = check_dir(&child_path) {
            let joined = child_path.join("thumbs").join(&t);
            return to_rel(&joined);
        }
    }

    // 3. Check grandchildren (first child's first child, etc.)
    for child in &children {
        let child_path = child.path();
        let Ok(grandchildren) = std::fs::read_dir(&child_path) else { continue };
        let mut gc: Vec<_> = grandchildren
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let s = name.to_string_lossy();
                e.metadata().map(|m| m.is_dir()).unwrap_or(false)
                    && !s.starts_with('.')
                    && s != "thumbs"
            })
            .collect();
        gc.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

        for gc_entry in &gc {
            let gc_path = gc_entry.path();
            if let Some(t) = check_dir(&gc_path) {
                let joined = gc_path.join("thumbs").join(&t);
                return to_rel(&joined);
            }
        }
    }

    None
}

/// Convert a full image path (from album root) into the thumbnail path
/// relative to the given folder. For example:
///   folder_path="1980-89", full_image_path="1980-89/1981/beach.jpg"
///   → "1981/thumbs/beach_thumb.jpg"
fn compute_cover_thumb(folder_path: &str, full_image_path: &str) -> Option<String> {
    // Strip folder prefix from full image path
    let rel = if folder_path.is_empty() {
        full_image_path.to_string()
    } else if let Some(stripped) = full_image_path.strip_prefix(&format!("{}/", folder_path)) {
        stripped.to_string()
    } else {
        // Image is outside this folder (shouldn't happen due to ancestor check)
        full_image_path.to_string()
    };

    let path = std::path::Path::new(&rel);
    let parent = path.parent();
    let filename = path.file_name()?.to_string_lossy();
    let thumb = util::thumb_name(&filename);

    match parent {
        Some(p) if !p.as_os_str().is_empty() => {
            Some(format!("{}/thumbs/{}", p.to_string_lossy().replace('\\', "/"), thumb))
        }
        _ => Some(format!("thumbs/{}", thumb)),
    }
}
