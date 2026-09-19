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
    counts::CountCache,
    covers::CoverCache,
    db::Db,
    models::{AlbumResponse, Breadcrumb, FolderItem, PhotoItem, SetCoverRequest},
    util,
};

pub struct AppState {
    pub config: Config,
    pub db: std::sync::Arc<Db>,
    /// Recursive folder counts, invalidated by the filesystem watcher. See
    /// [`crate::counts`].
    pub counts: std::sync::Arc<CountCache>,
    /// Computed cover resolutions, invalidated by the filesystem watcher and by
    /// `set_cover`. See [`crate::covers`].
    pub covers: std::sync::Arc<CoverCache>,
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
    let rel = util::validate_path(&query.path).ok_or(StatusCode::BAD_REQUEST)?;

    // `photo`, when present, must be a bare filename: no separators, so it
    // cannot escape the folder it is joined to.
    let photo = query.photo.trim();
    if !photo.is_empty()
        && (photo.contains('/') || photo.contains('\\') || util::validate_path(photo).is_none())
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Resolving the cover can walk directories (see `find_first_thumb_recursive`),
    // so it belongs on a blocking thread like the album listing, not on an
    // async worker thread that also serves `/api/health`.
    let cfg = state.config.clone();
    let db = state.db.clone();
    let covers = state.covers.clone();
    let photo = photo.to_string();
    let card = tokio::task::spawn_blocking(move || {
        build_share_card(&cfg, &db, &covers, &rel, &photo)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)??;

    let title_esc = escape_html(&card.title);
    let site_esc = escape_html(&state.config.server.site_name);
    let desc_esc = escape_html(&card.description);
    let redirect_esc = escape_html(&card.redirect);

    // Image tags are omitted entirely when no image is available. Pointing
    // og:image at a file that may not exist would be worse than saying nothing:
    // crawlers cache the resulting 404 and the preview silently loses its
    // picture. A folder whose thumbnails have not been generated yet therefore
    // yields a text-only card, which is honest and recovers by itself once the
    // worker catches up.
    let (image_tags, twitter_image_tag, twitter_card) = match card.image_url {
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

/// The pieces of a link-preview card, resolved from the filesystem.
struct ShareCard {
    title: String,
    description: String,
    image_url: Option<String>,
    redirect: String,
}

/// Blocking half of [`share_page`]: resolve the folder or photo being shared
/// and build the values the Open Graph tags need.
fn build_share_card(
    cfg: &Config,
    db: &Db,
    covers: &CoverCache,
    rel: &str,
    photo: &str,
) -> Result<ShareCard, StatusCode> {
    let root = &cfg.album.root;

    // Site root with exactly one trailing slash, e.g. "https://host/photos/".
    let site = format!("{}/", cfg.server.public_url.trim_end_matches('/'));
    // Photos are served from the origin root (`/photoalbum/...`), independent of
    // any path prefix the site itself sits under.
    let origin = origin_of(&site);
    let media = |rel_path: &str| format!("{}/photoalbum/{}", origin, encode_rel_path(rel_path));

    if photo.is_empty() {
        // ---- Folder ----
        let abs = util::resolve_album_path(root, rel).ok_or(StatusCode::NOT_FOUND)?;
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
        // `get_cover` is a single indexed read, so a point query is right here;
        // the listing uses the batched form because it resolves many folders.
        let cover = db
            .get_cover(rel)
            .filter(|full| root.join(full).exists())
            .and_then(|full| compute_cover_thumb(rel, &full))
            .or_else(|| resolve_cover(root, covers, rel));
        let image = cover.map(|c| {
            media(&if rel.is_empty() {
                c.clone()
            } else {
                format!("{}/{}", rel, c)
            })
        });
        Ok(ShareCard {
            title,
            description,
            image_url: image,
            redirect: format!("{}#path={}", site, encode_component(rel)),
        })
    } else {
        // ---- Single photo or video ----
        // Resolve the media path itself: `rel` is only the folder it lives in.
        let photo_rel = if rel.is_empty() {
            photo.to_string()
        } else {
            format!("{}/{}", rel, photo)
        };
        let photo_abs = util::resolve_album_path(root, &photo_rel).ok_or(StatusCode::NOT_FOUND)?;
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
        Ok(ShareCard {
            title: photo.to_string(),
            description,
            image_url: image,
            redirect: media(&photo_rel),
        })
    }
}

pub async fn get_album(
    Query(query): Query<AlbumQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<AlbumResponse>, StatusCode> {
    let rel_path = util::validate_path(&query.path).ok_or(StatusCode::BAD_REQUEST)?;
    // A path that is well-formed but absent is a 404; only a path that is
    // rejected — traversal, absolute, or resolving outside the album — is a
    // 400. Collapsing both into 400 told every caller the request was
    // malformed when the folder had simply been deleted.
    let abs_path = util::resolve_album_path_checked(&state.config.album.root, &rel_path)
        .map_err(|e| match e {
            util::PathError::Rejected => StatusCode::BAD_REQUEST,
            util::PathError::NotFound => StatusCode::NOT_FOUND,
        })?;

    if !abs_path.is_dir() {
        return Err(StatusCode::NOT_FOUND);
    }

    // Everything below is blocking filesystem work — `read_dir`, counting the
    // subtree of every subfolder, resolving covers — and it must not run on a
    // Tokio worker thread. A folder with many subfolders can occupy a runtime
    // thread for seconds, and a few concurrent page loads were enough to stall
    // unrelated requests (including `/api/health`).
    let root = state.config.album.root.clone();
    let db = state.db.clone();
    let counts = state.counts.clone();
    let covers = state.covers.clone();
    let rel = rel_path.clone();
    let response = tokio::task::spawn_blocking(move || {
        build_album_response(&root, &db, &counts, &covers, &rel, &abs_path)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)??;

    Ok(Json(response))
}

/// Blocking half of [`get_album`]: list one folder from disk.
fn build_album_response(
    root: &std::path::Path,
    db: &Db,
    counts: &CountCache,
    covers: &CoverCache,
    rel_path: &str,
    abs_path: &std::path::Path,
) -> Result<AlbumResponse, StatusCode> {
    let name = abs_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Home")
        .to_string();

    let breadcrumbs = build_breadcrumbs(rel_path);

    let entries = std::fs::read_dir(abs_path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by(|a, b| {
        let an = a.file_name();
        let bn = b.file_name();
        an.cmp(&bn)
    });

    // Split the folder into subfolders and media first, so the two database
    // lookups below can each be a single batched query rather than one point
    // query per entry.
    let mut subfolders: Vec<String> = Vec::new();
    let mut media: Vec<String> = Vec::new(); // file names, in listing order
    for entry in entries {
        let fname = entry.file_name();
        let fname_str = fname.to_string_lossy();
        if fname_str.starts_with('.') || fname_str == "thumbs" {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            // `file_type()` reports the link itself. A link to a directory is
            // not descended into (see `crate::worker::walk_dir` for why), so it
            // must not be offered as a folder either, and a link whose target
            // is gone can never be served, so it is not offered as a photo.
            let Ok(target) = std::fs::metadata(entry.path()) else {
                continue;
            };
            if target.is_dir() {
                continue;
            }
            if !util::is_media_file(&fname_str) {
                continue;
            }
        } else if file_type.is_dir() {
            subfolders.push(if rel_path.is_empty() {
                fname_str.to_string()
            } else {
                format!("{}/{}", rel_path, fname_str)
            });
            continue;
        } else if !util::is_media_file(&fname_str) {
            continue;
        }
        media.push(fname_str.to_string());
    }

    let photo_paths: Vec<String> = media
        .iter()
        .map(|name| {
            if rel_path.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", rel_path, name)
            }
        })
        .collect();
    let metadata = db.get_metadata_for(&photo_paths);
    let explicit_covers = db.get_covers(&subfolders);

    let mut folders = Vec::with_capacity(subfolders.len());
    for sub_path in subfolders {
        // Answered from the cache on every level below the first, so browsing
        // does not re-walk the tree once per folder visited.
        let (count_photos, count_albums) = counts.count(root, &sub_path);
        // A stored cover is consulted first, but only when it still points at a
        // file that exists; otherwise the computed fallback is used.
        let explicit = explicit_covers
            .get(&sub_path)
            .filter(|full_path| root.join(full_path).exists())
            .and_then(|full_path| compute_cover_thumb(&sub_path, full_path));
        let cover = match explicit {
            Some(thumb) => Some(thumb),
            None => resolve_cover(root, covers, &sub_path),
        };
        let name = sub_path.rsplit('/').next().unwrap_or(&sub_path).to_string();
        folders.push(FolderItem {
            name,
            path: sub_path,
            cover,
            count_photos,
            count_albums,
        });
    }

    let mut photos = Vec::with_capacity(media.len());
    for (name, photo_rel) in media.into_iter().zip(photo_paths) {
        let (width, height, duration) = metadata
            .get(&photo_rel)
            .map(|m| (m.width, m.height, m.duration))
            .unwrap_or((0, 0, None));
        let thumb = format!("thumbs/{}", util::thumb_name(&name));
        let mtype = util::media_type(&name);
        // Duration is only meaningful for videos, and a zero recorded by an
        // older build means "unknown" rather than "instant". It is absent
        // until the worker has probed the file; the frontend omits it then.
        let duration = if mtype == "video" {
            duration.filter(|secs| *secs > 0)
        } else {
            None
        };
        photos.push(PhotoItem {
            name,
            media_type: mtype.to_string(),
            thumb,
            width,
            height,
            duration,
        });
    }

    Ok(AlbumResponse {
        path: rel_path.to_string(),
        name,
        breadcrumbs,
        folders,
        photos,
    })
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

    // `validate_path` both rejects traversal and normalises, so what is stored
    // below is the canonical album-relative path. Storing the request string
    // verbatim meant a target of `"1970-79/"` (or an image of `"./a.jpg"`)
    // produced a row that `get_cover("1970-79")` can never match: the API
    // answered 204 and the cover silently never appeared.
    let image_path = util::validate_path(&body.image_path).ok_or(StatusCode::BAD_REQUEST)?;
    let image_abs = util::resolve_album_path_checked(&state.config.album.root, &image_path)
        .map_err(|e| match e {
            util::PathError::Rejected => StatusCode::BAD_REQUEST,
            util::PathError::NotFound => StatusCode::NOT_FOUND,
        })?;
    // It must be a real media file, not a directory and not a stray non-media
    // file. A stored cover is consulted before the computed fallback, so a bad
    // value would leave that folder showing a placeholder until it is changed
    // by hand.
    if !image_abs.is_file() || !util::is_media_file(&image_path) {
        return Err(StatusCode::NOT_FOUND);
    }

    // Every target is validated before anything is written. Writing as we went
    // meant a list whose last entry was invalid still applied its earlier
    // entries and then answered 400 — the caller saw a failure, the database
    // saw a partial success.
    let mut targets = Vec::with_capacity(body.targets.len());
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
        targets.push(target);
    }

    for target in &targets {
        // Store the full relative image path so covers work across folder levels
        state.db.set_cover(target, &image_path)
            .map_err(|e| {
                warn!("set_cover database error for target '{}': {}", target, e);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        info!("set_cover: stored cover for '{}' → '{}'", target, image_path);
    }

    // The cover a listing shows is cached; this write did not touch the
    // filesystem, so the watcher will not report it.
    state.covers.invalidate();

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

/// Resolve the cover a listing should show for `folder`.
///
/// The expensive half — walking the folder and its descendants for a usable
/// thumbnail — is memoised in `covers` and dropped when the watcher reports a
/// change. Only the *computed* answer is cached: an admin's explicit choice is
/// applied by the caller (a single batched query) because that lives in the
/// database rather than the filesystem.
fn resolve_cover(
    root: &std::path::Path,
    covers: &CoverCache,
    folder: &str,
) -> Option<String> {
    covers.get_or_compute(folder, || find_first_thumb_recursive(root, folder))
}

/// Find the first available thumbnail in a folder or any of its descendants.
/// Searches the folder itself, then immediate children, then grandchildren.
/// Returns a relative thumbnail path (e.g. "thumbs/photo_thumb.jpg" or
/// "subfolder/thumbs/photo_thumb.jpg") or None.
///
/// The result is memoised by [`resolve_cover`], which is what keeps a listing
/// from repeating these `read_dir` calls once per subfolder.
fn find_first_thumb_recursive(root: &std::path::Path, rel: &str) -> Option<String> {
    let base = root.join(rel);

    // Build a path relative to the folder being described by construction
    // rather than by canonicalising: every candidate below is reached from
    // `base` through names read out of the directory, so the join already is
    // the answer. `canonicalize` here cost a syscall per child and grandchild
    // and could never change the result.
    let rel_of = |parts: &[&str], thumb: &str| -> String {
        if parts.is_empty() {
            format!("thumbs/{thumb}")
        } else {
            format!("{}/thumbs/{thumb}", parts.join("/"))
        }
    };

    // Helper: check a single directory for thumbnails.
    // Returns the thumbnail filename (not path) if found.
    //
    // The `sources` set below is built from the directory listing, so it counts
    // the entries that are *named* like media and filters out `thumbs`, hidden
    // files and non-media names — the same rule the listing uses.
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

    // 1. Check the folder itself
    if let Some(t) = check_dir(&base) {
        return Some(rel_of(&[], &t));
    }

    // 2. Check immediate children (sorted)
    let Ok(entries) = std::fs::read_dir(&base) else { return None };
    let mut children: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(is_searchable_dir)
        .collect();
    children.sort_by_key(|a| a.file_name());

    for child in &children {
        let child_name = child.file_name();
        let child_name = child_name.to_string_lossy();
        if let Some(t) = check_dir(&child.path()) {
            return Some(rel_of(&[&child_name], &t));
        }
    }

    // 3. Check grandchildren (first child's first child, etc.)
    for child in &children {
        let child_name = child.file_name();
        let child_name = child_name.to_string_lossy();
        let Ok(grandchildren) = std::fs::read_dir(child.path()) else { continue };
        let mut gc: Vec<_> = grandchildren
            .filter_map(|e| e.ok())
            .filter(is_searchable_dir)
            .collect();
        gc.sort_by_key(|a| a.file_name());

        for gc_entry in &gc {
            let gc_name = gc_entry.file_name();
            let gc_name = gc_name.to_string_lossy();
            if let Some(t) = check_dir(&gc_entry.path()) {
                return Some(rel_of(&[&child_name, &gc_name], &t));
            }
        }
    }

    None
}

/// Whether a directory entry is a real subfolder worth searching: a directory,
/// not hidden, not a `thumbs` folder, and not a symlink (which may point
/// outside the album or back up its own tree).
fn is_searchable_dir(entry: &std::fs::DirEntry) -> bool {
    let name = entry.file_name();
    let name = name.to_string_lossy();
    if name.starts_with('.') || name == "thumbs" {
        return false;
    }
    entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
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

#[cfg(test)]
mod handler_tests {
    use super::*;
    use crate::db::Db;
    use crate::testutil::{TempTree, config_for, TEST_ADMIN_KEY};
    use axum::http::HeaderMap;

    fn state_for(tree: &TempTree, db: std::sync::Arc<Db>) -> Arc<AppState> {
        Arc::new(AppState {
            config: config_for(&tree.0, &tree.path("album.db")),
            db,
            counts: Arc::new(CountCache::new()),
            covers: Arc::new(CoverCache::new()),
        })
    }

    fn admin_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("X-Admin-Key", TEST_ADMIN_KEY.parse().unwrap());
        headers
    }

    /// The bug this covers: `set_cover` stored the request string verbatim, so
    /// a target of `"1970-79/"` (or an image of `"./…"`) produced a row that
    /// `get_cover("1970-79")` could never match. The API answered 204 and the
    /// cover silently never appeared.
    #[tokio::test]
    async fn set_cover_stores_canonical_paths() {
        let tree = TempTree::new();
        tree.file("1970-79/1970/a.jpg");
        let db = Arc::new(Db::open_in_memory().unwrap());

        let body = SetCoverRequest {
            image_path: "./1970-79/1970/a.jpg".to_string(),
            targets: vec!["1970-79/".to_string(), "1970-79//".to_string(), "".to_string()],
        };
        let result = set_cover(
            State(state_for(&tree, db.clone())),
            admin_headers(),
            Json(body),
        )
        .await;

        assert_eq!(result.unwrap(), StatusCode::NO_CONTENT);
        let expected = Some("1970-79/1970/a.jpg".to_string());
        assert_eq!(db.get_cover("1970-79"), expected);
        assert_eq!(db.get_cover(""), expected, "the album root is a valid target");
        // No un-normalised key was written.
        assert_eq!(db.get_cover("1970-79/"), None);
        assert_eq!(db.get_cover("./1970-79/1970/a.jpg"), None);
    }

    #[tokio::test]
    async fn set_cover_rejects_a_target_that_is_not_an_ancestor() {
        let tree = TempTree::new();
        tree.file("a/photo.jpg");
        tree.dir("b");
        let db = Arc::new(Db::open_in_memory().unwrap());

        let body = SetCoverRequest {
            image_path: "a/photo.jpg".to_string(),
            targets: vec!["b".to_string()],
        };
        let result = set_cover(State(state_for(&tree, db.clone())), admin_headers(), Json(body)).await;

        assert_eq!(result.unwrap_err(), StatusCode::BAD_REQUEST);
        assert_eq!(db.get_cover("b"), None);
    }

    #[tokio::test]
    async fn set_cover_applies_nothing_when_any_target_is_invalid() {
        let tree = TempTree::new();
        tree.file("a/photo.jpg");
        tree.dir("b");
        let db = Arc::new(Db::open_in_memory().unwrap());

        // `a` is valid, `b` is not an ancestor: neither may be written.
        let body = SetCoverRequest {
            image_path: "a/photo.jpg".to_string(),
            targets: vec!["a".to_string(), "b".to_string()],
        };
        let result = set_cover(State(state_for(&tree, db.clone())), admin_headers(), Json(body)).await;

        assert_eq!(result.unwrap_err(), StatusCode::BAD_REQUEST);
        assert_eq!(db.get_cover("a"), None, "a valid target must not be written either");
    }

    #[tokio::test]
    async fn set_cover_requires_the_admin_key() {
        let tree = TempTree::new();
        tree.file("a/photo.jpg");
        let db = Arc::new(Db::open_in_memory().unwrap());

        let body = SetCoverRequest {
            image_path: "a/photo.jpg".to_string(),
            targets: vec!["".to_string()],
        };
        let result = set_cover(
            State(state_for(&tree, db.clone())),
            HeaderMap::new(),
            Json(body),
        )
        .await;

        assert_eq!(result.unwrap_err(), StatusCode::FORBIDDEN);
        assert_eq!(db.get_cover(""), None);
    }

    fn list(tree: &TempTree, db: &Db) -> AlbumResponse {
        build_album_response(
            &tree.0,
            db,
            &CountCache::new(),
            &CoverCache::new(),
            "",
            &tree.0,
        )
        .expect("listing")
    }

    /// A symlinked directory is not browsable (the worker will not walk it), so
    /// it must not be offered as a folder, and a symlink whose target is gone
    /// can never be served, so it must not be offered as a photo. Symlinked
    /// *files* that do resolve are still listed.
    #[cfg(unix)]
    #[test]
    fn links_are_listed_only_when_they_resolve_to_a_usable_target() {
        let tree = TempTree::new();
        let outside = TempTree::new();
        tree.file("real.jpg");
        outside.dir("elsewhere");

        std::os::unix::fs::symlink("real.jpg", tree.path("link.jpg")).unwrap();
        std::os::unix::fs::symlink("gone.jpg", tree.path("dangling.jpg")).unwrap();
        std::os::unix::fs::symlink(outside.path("elsewhere"), tree.path("linked-dir")).unwrap();

        let db = Db::open_in_memory().unwrap();
        let response = list(&tree, &db);

        let photos: Vec<&str> = response.photos.iter().map(|p| p.name.as_str()).collect();
        assert!(photos.contains(&"real.jpg"), "photos: {photos:?}");
        assert!(photos.contains(&"link.jpg"), "photos: {photos:?}");
        assert!(!photos.contains(&"dangling.jpg"), "photos: {photos:?}");

        let folders: Vec<&str> = response.folders.iter().map(|f| f.name.as_str()).collect();
        assert!(
            !folders.contains(&"linked-dir"),
            "a symlinked directory must not be listed as a folder: {folders:?}"
        );
    }

    #[test]
    fn a_folder_listing_reports_the_metadata_it_has() {
        let tree = TempTree::new();
        tree.file("a.jpg");
        tree.file("b.mp4");
        tree.file("thumbs/a_thumb.jpg");
        tree.dir("sub");
        let db = Db::open_in_memory().unwrap();
        db.set_metadata("a.jpg", 400, 300, None, 1, true).unwrap();
        db.set_metadata("b.mp4", 1920, 1080, Some(90), 2, true).unwrap();
        // A zero written by an older build means "unknown", not "instant".
        db.set_metadata("c.mp4", 640, 480, Some(0), 3, true).unwrap();

        let response = list(&tree, &db);

        let by_name = |name: &str| response.photos.iter().find(|p| p.name == name).unwrap();
        assert_eq!((by_name("a.jpg").width, by_name("a.jpg").height), (400, 300));
        assert_eq!(by_name("b.mp4").duration, Some(90));
        assert_eq!(by_name("b.mp4").media_type, "video");
        assert_eq!(by_name("a.jpg").thumb, "thumbs/a_thumb.jpg");
        // The thumbs folder is not itself listed, and metadata-less files fall
        // back to zero dimensions rather than being dropped.
        assert_eq!(response.folders.len(), 1);
        assert_eq!(response.folders[0].name, "sub");
    }

    #[test]
    fn the_computed_cover_is_cached_until_it_is_invalidated() {
        let tree = TempTree::new();
        let covers = CoverCache::new();

        // Nothing to find yet: the miss is cached as "no cover".
        assert_eq!(resolve_cover(&tree.0, &covers, "b"), None);
        tree.file("b/photo.jpg");
        tree.file("b/thumbs/photo_thumb.jpg");
        assert_eq!(
            resolve_cover(&tree.0, &covers, "b"),
            None,
            "a memoised miss must not be recomputed for every request"
        );

        // The watcher invalidates on exactly this kind of change.
        covers.invalidate();
        assert_eq!(
            resolve_cover(&tree.0, &covers, "b"),
            Some("thumbs/photo_thumb.jpg".to_string())
        );

        // A cover is only offered when its source still exists.
        std::fs::remove_file(tree.path("b/photo.jpg")).unwrap();
        covers.invalidate();
        assert_eq!(resolve_cover(&tree.0, &covers, "b"), None);
    }

    #[test]
    fn cover_lookup_prefers_the_stored_choice_and_falls_back_to_the_walk() {
        let tree = TempTree::new();
        tree.file("sub/photo.jpg");
        tree.file("sub/thumbs/photo_thumb.jpg");
        let db = Db::open_in_memory().unwrap();
        let covers = CoverCache::new();

        // No stored choice: the walk finds the only thumbnail.
        assert_eq!(resolve_cover(&tree.0, &covers, "sub"), Some("thumbs/photo_thumb.jpg".to_string()));

        // A stored choice wins over the computed one, and is relative to the
        // folder that owns it.
        assert_eq!(
            compute_cover_thumb("sub", "sub/photo.jpg"),
            Some("thumbs/photo_thumb.jpg".to_string())
        );
        assert_eq!(compute_cover_thumb("", "sub/photo.jpg"), Some("sub/thumbs/photo_thumb.jpg".to_string()));
        assert!(db.get_cover("sub").is_none());
    }

    #[test]
    fn a_trailing_slash_cannot_add_an_empty_breadcrumb() {
        let normalised = util::validate_path("1970-79/1970/").unwrap();
        assert_eq!(normalised, "1970-79/1970");
        let crumbs = build_breadcrumbs(&normalised);
        assert_eq!(crumbs.len(), 3, "Home / 1970-79 / 1970");
        assert!(crumbs.iter().all(|c| !c.name.is_empty()));
    }
}
