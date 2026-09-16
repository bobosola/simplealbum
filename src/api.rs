use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{Html, Json},
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

#[derive(Deserialize)]
pub struct ShareQuery {
    #[serde(default)]
    pub path: String,
    /// Optional bare filename inside `path`. Absent means "share this folder".
    #[serde(default)]
    pub photo: String,
}

/// Escape text for use in HTML text or a double-quoted attribute.
///
/// Folder and file names come from the filesystem and are echoed back into the
/// generated page, so they must never be interpolated raw.
fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Percent-encode one value the way JavaScript's `encodeURIComponent` does.
/// Keeps share URLs byte-identical to what the frontend would have produced
/// itself, so a link generated here and one generated in the browser are the
/// same string. Note that `encodeURIComponent` leaves `!'()*` unescaped, so
/// those must be left alone here too.
fn encode_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'!'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(byte as char),
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

/// Percent-encode a `/`-separated relative path, leaving the separators alone.
fn encode_rel_path(rel: &str) -> String {
    rel.split('/').map(encode_component).collect::<Vec<_>>().join("/")
}

/// Scheme + host + port of a base URL, i.e. everything before the path.
fn origin_of(url: &str) -> &str {
    let after_scheme = url.find("//").map(|i| i + 2).unwrap_or(0);
    match url[after_scheme..].find('/') {
        Some(i) => &url[..after_scheme + i],
        None => url,
    }
}

/// Build the shareable page that link-preview crawlers read.
///
/// Preview crawlers (WhatsApp, Facebook, Slack, Telegram, iMessage...) read
/// Open Graph tags from an HTML `<head>` and never execute JavaScript. The SPA's
/// own URLs put the album path in the *fragment* (`#path=...`), which browsers
/// never send to the server, so a static `index.html` cannot describe what was
/// shared and every link previews identically.
///
/// This endpoint takes the same information as a *query string*, which does
/// reach the server, so each shared folder or photo can carry its own title and
/// image. It returns a tiny HTML page for the crawler and immediately forwards
/// a human visitor on to the real destination.
///
/// Query strings, not path segments, are used deliberately: photos and folders
/// may contain spaces, apostrophes and other characters that are awkward in a
/// path, and `util::validate_path` already guards the traversal cases.
pub async fn share_page(
    Query(query): Query<ShareQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Html<String>, StatusCode> {
    let cfg = &state.config;
    let root = &cfg.album.root;

    let rel = util::validate_path(&query.path).ok_or(StatusCode::BAD_REQUEST)?;

    // `photo`, when present, must be a bare filename: no separators, so it
    // cannot escape the folder it is joined to.
    let photo = query.photo.trim();
    if !photo.is_empty()
        && (photo.contains('/') || photo.contains('\\') || util::validate_path(photo).is_none())
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Site root with exactly one trailing slash, e.g. "https://host/photos/".
    let site = format!("{}/", cfg.server.public_url.trim_end_matches('/'));
    // Photos are served from the origin root (`/photoalbum/...`), independent of
    // any path prefix the site itself sits under.
    let origin = origin_of(&site);
    let media = |rel_path: &str| format!("{}/photoalbum/{}", origin, encode_rel_path(rel_path));

    let (title, description, image_url, redirect) = if photo.is_empty() {
        // ---- Folder ----
        let abs = util::resolve_album_path(root, &rel).ok_or(StatusCode::NOT_FOUND)?;
        if !abs.is_dir() {
            return Err(StatusCode::NOT_FOUND);
        }
        let name = rel.rsplit('/').next().filter(|s| !s.is_empty());
        let title = match name {
            Some(n) => n.to_string(),
            None => cfg.server.site_name.clone(),
        };
        let description = if rel.is_empty() {
            "Browse our photo and video collection by folder.".to_string()
        } else {
            format!("Photos and videos in {}", rel.replace('/', " / "))
        };
        // Reuses the same cover resolution as the grid: an admin-chosen cover
        // first, then the first thumbnail found in the folder or below it.
        let cover = state
            .db
            .get_cover(&rel)
            .filter(|full| root.join(full).exists())
            .and_then(|full| compute_cover_thumb(&rel, &full))
            .or_else(|| find_first_thumb_recursive(root, &rel));
        let image = cover
            .map(|c| media(&if rel.is_empty() { c.clone() } else { format!("{}/{}", rel, c) }));
        (
            title,
            description,
            image,
            format!("{}#path={}", site, encode_component(&rel)),
        )
    } else {
        // ---- Single photo or video ----
        // Resolve the media path itself: `rel` is only the folder it lives in.
        let photo_rel = if rel.is_empty() {
            photo.to_string()
        } else {
            format!("{}/{}", rel, photo)
        };
        let photo_abs =
            util::resolve_album_path(root, &photo_rel).ok_or(StatusCode::NOT_FOUND)?;
        if !photo_abs.is_file() || !util::is_media_file(photo) {
            return Err(StatusCode::NOT_FOUND);
        }
        let thumb_rel = if rel.is_empty() {
            format!("thumbs/{}", util::thumb_name(photo))
        } else {
            format!("{}/thumbs/{}", rel, util::thumb_name(photo))
        };
        // Prefer the thumbnail: originals run to several MB, well past the
        // 600KB that WhatsApp will accept for a preview image. Fall back to the
        // original when the worker has not produced a thumbnail yet.
        let image = if root.join(&thumb_rel).is_file() {
            Some(media(&thumb_rel))
        } else {
            Some(media(&photo_rel))
        };
        let description = if rel.is_empty() {
            cfg.server.site_name.clone()
        } else {
            format!("From {}", rel.replace('/', " / "))
        };
        (
            photo.to_string(),
            description,
            image,
            media(&photo_rel),
        )
    };

    let title_esc = escape_html(&title);
    let site_esc = escape_html(&cfg.server.site_name);
    let desc_esc = escape_html(&description);
    let redirect_esc = escape_html(&redirect);

    // Image tags are omitted entirely when no image is available. Pointing
    // og:image at a file that may not exist would be worse than saying nothing:
    // crawlers cache the resulting 404 and the preview silently loses its
    // picture. A folder whose thumbnails have not been generated yet therefore
    // yields a text-only card, which is honest and recovers by itself once the
    // worker catches up.
    let (image_tags, twitter_image_tag, twitter_card) = match image_url {
        Some(url) => {
            let url = escape_html(&url);
            (
                format!(
                    "<meta property=\"og:image\" content=\"{url}\">\n\
<meta property=\"og:image:alt\" content=\"{title_esc}\">\n"
                ),
                format!("<meta name=\"twitter:image\" content=\"{url}\">\n"),
                "summary_large_image",
            )
        }
        // No image means the large-image layout has nothing to show.
        None => (String::new(), String::new(), "summary"),
    };

    // `http-equiv="refresh"` rather than an inline script: it is honoured by
    // every browser, and unlike an inline script it cannot be blocked by a
    // Content-Security-Policy without 'unsafe-inline'.
    let page = format!(
        "<!DOCTYPE html>\n\
<html lang=\"en\">\n\
<head>\n\
<meta charset=\"UTF-8\">\n\
<title>{title_esc}</title>\n\
<meta name=\"description\" content=\"{desc_esc}\">\n\
<meta property=\"og:type\" content=\"website\">\n\
<meta property=\"og:site_name\" content=\"{site_esc}\">\n\
<meta property=\"og:title\" content=\"{title_esc}\">\n\
<meta property=\"og:description\" content=\"{desc_esc}\">\n\
{image_tags}<meta name=\"twitter:card\" content=\"{twitter_card}\">\n\
<meta name=\"twitter:title\" content=\"{title_esc}\">\n\
<meta name=\"twitter:description\" content=\"{desc_esc}\">\n\
{twitter_image_tag}<meta http-equiv=\"refresh\" content=\"0; url={redirect_esc}\">\n\
</head>\n\
<body>\n\
<p><a href=\"{redirect_esc}\">Continue to {title_esc}</a></p>\n\
</body>\n\
</html>\n"
    );

    Ok(Html(page))
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
            let (width, height, duration) = state.db.get_metadata(&photo_rel)
                .map(|(w, h, duration, _)| (w, h, duration))
                .unwrap_or((0, 0, None));
            let thumb = format!("thumbs/{}", util::thumb_name(&fname_str));
            let mtype = util::media_type(&fname_str);
            // Duration is only meaningful for videos. It is absent until the
            // worker has probed the file, and the frontend simply omits it in
            // that case (previously this was hardcoded to 0, which the
            // frontend treats as falsy, so it was never displayed at all).
            let duration = if mtype == "video" { duration } else { None };
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

    // Defence in depth against CSRF, as `DESIGN.md` documents. The custom
    // `X-Admin-Key` header already forces a preflight that a foreign origin
    // cannot satisfy, but a browser sends `Origin` on every POST, so a
    // mismatch is rejected outright. An absent header is allowed so that curl
    // and other non-browser callers keep working.
    if let Some(origin) = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        && !origin.is_empty()
        && !origin.eq_ignore_ascii_case(origin_of(&state.config.server.public_url))
    {
        warn!("set_cover rejected: origin '{}' does not match public_url", origin);
        return Err(StatusCode::FORBIDDEN);
    }

    let image_path = util::validate_path(&body.image_path).ok_or(StatusCode::BAD_REQUEST)?;
    let image_abs = util::resolve_album_path(&state.config.album.root, &image_path)
        .ok_or(StatusCode::BAD_REQUEST)?;
    // It must be a real media file, not a directory and not a stray non-media
    // file. A stored cover is consulted before the computed fallback, so a bad
    // value would leave that folder showing a placeholder until it is changed
    // by hand.
    if !image_abs.is_file() || !util::is_media_file(&image_path) {
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
            // `file_type()` reads the entry type from the directory itself, so
            // it needs no extra `stat` per entry and — unlike `metadata()` — it
            // does not follow symlinks. That matters here as much as in the
            // worker's walk: a symlink pointing back up the tree would make
            // this walk run forever while holding an async worker thread, and
            // one pointing outside the album would count files the album does
            // not own. Symlinked files are still counted via the metadata()
            // fallback below; symlinked directories are skipped.
            let Ok(file_type) = entry.file_type() else { continue };
            let is_symlink = file_type.is_symlink();
            let is_dir = if is_symlink {
                entry.metadata().map(|m| m.is_dir()).unwrap_or(false)
            } else {
                file_type.is_dir()
            };
            if is_dir && !is_symlink {
                if is_top_level {
                    albums += 1;
                }
                let sub_rel = if current_rel.is_empty() {
                    s.to_string()
                } else {
                    format!("{}/{}", current_rel, s)
                };
                stack.push((sub_rel, false));
            } else if !is_dir && util::is_media_file(&s) {
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
        // Stems of the media files actually present in this folder. A thumbnail
        // is only offered as a cover when its source still exists: after a
        // rename or delete the old `*_thumb.jpg` can survive in `thumbs/`, and
        // without this check it would be adopted as the folder's cover and
        // point at a photo that is no longer in the album. A thumbnail always
        // lives beside its source, so this never rejects a legitimate one.
        let sources: std::collections::HashSet<String> = std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok())
            .filter(|e| util::is_media_file(&e.file_name().to_string_lossy()))
            .filter_map(|e| {
                let name = e.file_name();
                let stem = std::path::Path::new(&name).file_stem()?;
                Some(stem.to_string_lossy().into_owned())
            })
            .collect();

        let mut entries: Vec<_> = std::fs::read_dir(&thumbs_dir)
            .ok()?
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|a| a.file_name());
        for entry in entries {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s.starts_with('.') {
                continue;
            }
            let Some((stem, _)) = s.rsplit_once("_thumb.") else { continue };
            if sources.contains(stem) {
                return Some(s.to_string());
            }
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
    children.sort_by_key(|a| a.file_name());

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
        gc.sort_by_key(|a| a.file_name());

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_html_neutralises_markup_and_quotes() {
        assert_eq!(
            escape_html("<img src=\"x\" onerror='y'>&"),
            "&lt;img src=&quot;x&quot; onerror=&#39;y&#39;&gt;&amp;"
        );
        assert_eq!(escape_html("Bob & Karen"), "Bob &amp; Karen");
    }

    #[test]
    fn encode_component_matches_encode_uri_component() {
        let uri = |s: &str| -> String {
            s.bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b"-_.~!*'()".contains(&b) {
                        (b as char).to_string()
                    } else {
                        format!("%{:02X}", b)
                    }
                })
                .collect()
        };
        for value in [
            "summer holiday",
            "a/b",
            "Bob & Karen's",
            "(1970) beach!",
            "caf\u{e9}",
            "100%_sure*",
        ] {
            assert_eq!(encode_component(value), uri(value), "mismatch for {value:?}");
        }
        // Separators are structural in a relative path and must survive.
        assert_eq!(encode_rel_path("1970-79/summer holiday/a b.jpg"), "1970-79/summer%20holiday/a%20b.jpg");
    }

    #[test]
    fn origin_of_strips_the_path_and_keeps_scheme_and_host() {
        assert_eq!(origin_of("https://www.example.org/photos/"), "https://www.example.org");
        assert_eq!(origin_of("http://localhost:8443/"), "http://localhost:8443");
        assert_eq!(origin_of("https://example.org"), "https://example.org");
    }

    #[test]
    fn breadcrumbs_accumulate_the_path() {
        let root_crumbs = build_breadcrumbs("");
        assert_eq!(root_crumbs.len(), 1);
        assert_eq!(root_crumbs[0].name, "Home");
        assert_eq!(root_crumbs[0].path, "");
        let crumbs = build_breadcrumbs("1970-79/1970/summer holiday");
        assert_eq!(crumbs.len(), 4);
        assert_eq!(crumbs[1].name, "1970-79");
        assert_eq!(crumbs[1].path, "1970-79");
        assert_eq!(crumbs[3].name, "summer holiday");
        assert_eq!(crumbs[3].path, "1970-79/1970/summer holiday");
    }

    #[test]
    fn cover_thumb_is_relative_to_the_covering_folder() {
        assert_eq!(
            compute_cover_thumb("1980-89", "1980-89/1981/beach.jpg"),
            Some("1981/thumbs/beach_thumb.jpg".to_string())
        );
        // A cover on the album root is the image's own folder, relative to root.
        assert_eq!(
            compute_cover_thumb("", "1980-89/1981/beach.jpg"),
            Some("1980-89/1981/thumbs/beach_thumb.jpg".to_string())
        );
        // Image directly inside the folder the cover is set on.
        assert_eq!(
            compute_cover_thumb("1980-89", "1980-89/beach.jpg"),
            Some("thumbs/beach_thumb.jpg".to_string())
        );
    }
}
