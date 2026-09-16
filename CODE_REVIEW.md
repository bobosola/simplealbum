# Simple Photo Album — Code Quality & Architecture Review

# Simple Photo Album — Code Quality & Architecture Review

> **Status:** this is a review of the code as it stood *before* the fix commit that
> follows it in the history. Most findings were acted on; four were revised after
> closer reading of the source, and are left here uncorrected in place so the
> review reads as written:
>
> - **§4.1** mis-diagnosed the viewer history: pressing Back while the viewer is
>   open does *not* jump to photo 0, it closes the viewer (the entry before the
>   viewer state carries no `view` field). The real issue was §4.3's stale entry.
> - **§4.3** wrongly claimed the up-arrow left `viewer-open` on `<body>` and left
>   video playing. It called `hideViewer()`, which does both correctly; the only
>   fault was the unconsumed history entry.
> - **§6.4** blamed `README.md`, which documents the recursive count correctly at
>   lines 101-102. The understated claim is in `DESIGN.md` line 529.
> - **§3.1** over-stated the counting cost: the per-subfolder subtrees are
>   disjoint, so one `GET /api/album` performs one traversal of the requested
>   folder's subtree, not a cost multiplied by depth. It is blocking I/O on the
>   async runtime that is the sharper issue, not redundant work.
>
> **§2.5** also over-stated the admin-key logging: printing the admin URL at
> startup is deliberate and documented (`DESIGN.md` line 367).

## 1. Executive Summary

This report evaluates the code quality, architecture, performance, security, and documentation of the **Simple Photo Album** project.

### Key Strengths
- **Design & Philosophy:** Excellent adherence to the KISS ("Keep It Simple, Stupid") philosophy. Offloading TLS, static files, and media serving directly to Caddy while keeping the Rust service focused purely on metadata, thumbnail generation, and directory queries is a very sound architectural choice.
- **Zero-Dependency Frontend:** Plain HTML/CSS and vanilla ES2020+ JavaScript without heavy npm build pipelines or frameworks makes the project lightweight, portable, and easy to deploy.
- **Thoughtful Operational Details:**
  - Using `.tmp` files with atomic renames prevents half-written thumbnails from crashes.
  - Link preview crawling via query strings on `/api/share` nicely resolves the classic SPA OpenGraph fragment limitation.
  - Comprehensive documentation with step-by-step guides for multiple platforms.

### Key Areas for Improvement
- **Data & MIME Inconsistencies:** JPEG bytes saved under `.png`/`.webp` extensions; un-rotated dimensions stored for portrait EXIF photos; dummy video durations.
- **Performance at Scale:** Subfolder badge counting recurses through entire subtrees on every request, contradicting documentation and introducing latency on large collections.
- **Security & Input Handling:** Path validation allows root and prefix components; shell command injection vulnerability in `sync_photos.sh`; plaintext admin secret logged on startup.
- **Testing & Tooling:** Zero automated tests; 11 compiler/clippy warnings; manual cache-busting renaming workflow.
- **Documentation Drift:** Several discrepancies between `DESIGN.md` / `README.md` and the actual implementation.

---

## 2. High-Priority Issues (Bugs & Security)

### 2.1 Thumbnail Extension vs. Image Encoding Mismatch
- **Location:** `src/util.rs` (`thumb_name`) & `src/thumb.rs` (`generate_image_thumb`)
- **Problem:** In `util.rs`, `thumb_name` retains the original file extension (e.g. `image.png` $\to$ `image_thumb.png`, `image.webp` $\to$ `image_thumb.webp`). In `thumb.rs`, however, thumbnails are always encoded as JPEG:
  ```rust
  thumb.save_with_format(&tmp, image::ImageFormat::Jpeg)?;
  ```
  This creates JPEG images named with `.png` or `.webp` extensions. When Caddy serves these assets, it infers the `Content-Type` from the file extension (`image/png` or `image/webp`). Strict image decoders, social media preview bots, or browsers can fail or log format mismatch errors.
- **Recommendation:**
  - **Option A (Simpler & Recommended):** Always name thumbnails with `.jpg` in `thumb_name()` (`{stem}_thumb.jpg`), which unifies video and photo thumbnails and saves disk space.
  - **Option B:** Preserve source format by selecting `ImageFormat::Jpeg`, `ImageFormat::Png`, or `ImageFormat::WebP` based on the file extension during encoding.

### 2.2 Inverted Dimensions for EXIF-Rotated Images
- **Location:** `src/thumb.rs` (`generate_image_thumb`) & `src/worker.rs` (`update_metadata`)
- **Problem:** `generate_image_thumb` captures the dimensions before applying the EXIF orientation:
  ```rust
  let (orig_w, orig_h) = img.dimensions();
  let orientation = read_exif_orientation(src);
  let img = apply_orientation(img, orientation);
  // ...
  Ok((orig_w, orig_h))
  ```
  If an image has EXIF orientation 5, 6, 7, or 8 (90° or 270° rotation, common on smartphones), `apply_orientation` rotates the image and swaps width and height. But `generate_image_thumb` returns the unrotated `(orig_w, orig_h)`, and `update_metadata` ignores EXIF entirely. These inverted dimensions are stored in SQLite and sent to the frontend, causing portrait photos to report landscape pixel dimensions in the UI.
- **Recommendation:** Query `img.dimensions()` *after* `apply_orientation` has rotated the image.

### 2.3 Command Injection Vulnerability in `sync_photos.sh`
- **Location:** `sync_photos.sh` line 38
- **Problem:** User input is evaluated directly in the shell:
  ```bash
  eval "SOURCES=($USER_INPUT)"
  ```
  If a dragged path or pasted filename contains shell metacharacters (e.g., backticks, `$()`, `;`), `eval` executes them as arbitrary shell commands.
- **Recommendation:** Avoid `eval`. Use bash array assignment (`read -r -a SOURCES`) or sanitize/quote paths safely.

### 2.4 Path Validation Permits Root and Drive Prefixes
- **Location:** `src/util.rs` (`validate_path`)
- **Problem:** `validate_path` only checks for `Component::ParentDir` (`..`). It does not reject `Component::RootDir` (`/`) or Windows `Component::Prefix` (`C:`):
  ```rust
  pub fn validate_path(input: &str) -> Option<String> {
      if input.is_empty() { return Some(String::new()); }
      let p = Path::new(input);
      for comp in p.components() {
          if let std::path::Component::ParentDir = comp { return None; }
      }
      Some(input.to_string())
  }
  ```
  In Rust, `Path::new("/var/album").join("/etc/passwd")` discards `/var/album` and evaluates to `/etc/passwd`. While `resolve_album_path` includes a canonicalization boundary check, callers such as `thumb::delete_thumb` and `api::set_cover` call `root.join(&rel)` directly without canonicalization guards.
- **Recommendation:** Reject paths containing `Component::RootDir` or `Component::Prefix`, and strip leading slashes.

### 2.5 Plaintext Admin Key Logging & Non-Constant-Time Check
- **Location:** `src/main.rs` lines 58–63 & `src/api.rs` (`set_cover`)
- **Problem:**
  - `main.rs` outputs the admin key and admin URL in plaintext to standard logs at startup:
    ```rust
    info!("Admin key: {}", cfg.admin.key);
    info!("Admin URL: {}#admin={}", cfg.server.public_url, cfg.admin.key);
    ```
    On production systems, journald or syslog files can be read by other processes or persisted in shared log aggregators.
  - `api.rs` uses standard string equality (`admin_key != state.config.admin.key`), which is vulnerable to timing attacks.
- **Recommendation:**
  - Redact or mask the admin key in logs (e.g., `****${key[key.len()-4..]}`), or only print the full URL under an explicit debug flag.
  - Use constant-time comparison (`subtle::ConstantTimeEq`) when validating `X-Admin-Key`.

---

## 3. Medium-Priority Issues (Architecture & Performance)

### 3.1 Unbounded Subtree Recursion on Every `GET /api/album`
- **Location:** `src/api.rs` (`get_album` $\to$ `count_contents`)
- **Problem:** The documentation states that `GET /api/album` only reads the single requested directory and does not recurse. In reality, for every subfolder in the requested directory, `count_contents` performs a recursive DFS over the entire directory tree below it to compute the photo count badge.
  On a collection with 10–20 subfolders at the root and tens of thousands of media files, visiting `/` forces thousands of synchronous `stat` and `read_dir` operations on disk per request.
- **Recommendation:**
  - Cache folder badge counts in SQLite or in memory with a short TTL; or
  - Count only direct children (`is_top_level`) instead of recursing into the entire subtree; or
  - Maintain cumulative counts in SQLite updated during worker scans.

### 3.2 Watcher Ignores Directory Creation and Leaves Orphaned DB Rows
- **Location:** `src/watcher.rs` & `src/worker.rs` (`process_delete`)
- **Problem:**
  - In `watcher.rs`, `if path.is_file()` guards `Create` and `Modify` events. If a user moves an entire folder into the album (`mv new_folder /var/album/`), the watcher receives a directory event (`is_file()` is false) and ignores it. Thumbnails for files inside the moved folder are not generated until the service restarts.
  - In `process_delete`, deleting a folder only removes rows where `photo_path == rel_path`. Metadata rows for all images inside that folder remain orphaned in the database.
- **Recommendation:**
  - When the watcher detects a new directory, trigger a scan of that directory.
  - When a directory is deleted, run `DELETE FROM photo_metadata WHERE photo_path LIKE ?1 || '/%'`.

### 3.3 Single Database Mutex and Missing Busy Timeout
- **Location:** `src/db.rs`
- **Problem:**
  - All database interactions share a single `std::sync::Mutex<Connection>`. Even though SQLite is in WAL mode, all concurrent API reads are serialized behind background thumbnail inserts.
  - SQLite defaults to a 0 ms busy timeout. If an external tool (e.g. `sqlite3 album.db`) inspects the database while the service is writing, queries immediately return `SQLITE_BUSY`.
- **Recommendation:**
  - Set `PRAGMA busy_timeout = 5000;` on connection open.
  - Consider using an `r2d2` connection pool or separate read/write handles so API reads never wait on worker writes.

### 3.4 Future `mtime` Staleness Loop
- **Location:** `src/worker.rs` (`thumb_is_fresh`)
- **Problem:** `thumb_is_fresh` checks `thumb_mtime >= src_mtime`. If a photo's `mtime` is set in the future (e.g. camera clock desynchronization or clock drift), `thumb_mtime >= src_mtime` is always false. On every service restart, `scan_existing` will treat the file as stale and repeatedly regenerate its thumbnail.
- **Recommendation:** Clamp `src_mtime` to `SystemTime::now()` when checking freshness.

---

## 4. Frontend & User Experience Improvements

### 4.1 Viewer History Desynchronization
- **Location:** `static/app-*.js` (`viewerNext`, `viewerPrev`, `popstate`)
- **Problem:** `openViewer` pushes `{path, view: index}` to the browser history, but clicking Next or Prev navigates without updating the history state. If a user opens photo 0, clicks Next 10 times to photo 10, and hits browser Back, the viewer jumps back to photo 0 instead of closing or stepping back.
- **Recommendation:** Call `history.replaceState({path: currentPath, view: currentViewerIndex}, '')` when advancing photos in the viewer.

### 4.2 HTML Entity Typo in Header
- **Location:** `static/index.html` line 53
- **Problem:** `<h1>Bob &amp Karen's Photo Album</h1>` is missing the closing semicolon on `&amp;`.
- **Recommendation:** Fix to `<h1>Bob &amp; Karen's Photo Album</h1>`.

### 4.3 Incomplete and Redundant `viewerUp()` Function
- **Location:** `static/app-*.js` line 281 & line 624
- **Problem:** `viewerUp()` only hides `#viewer`. It fails to remove `viewer-open` from `document.body` (leaving background scroll locked) and fails to stop playing video (`stopViewerVideo()`). At line 624, the button is bound to `hideViewer()` instead, but neither cleans up the viewer's history state.
- **Recommendation:** Consolidate all viewer exit paths into a single clean function that stops video, removes body classes, and synchronizes browser history.

### 4.4 Double Slashes in URLs at Album Root
- **Location:** `static/app-*.js` (`renderGrid`, `photoMediaUrl`, `renderViewerItem`)
- **Problem:** When viewing the root directory (`currentPath === ""`), templates like `${PHOTO_BASE}/${encodePath(currentPath)}/${photo.thumb}` produce `/photoalbum//thumbs/...`.
- **Recommendation:** Use a helper function (e.g., `joinPaths(PHOTO_BASE, currentPath, photo.thumb)`) that avoids redundant slashes.

### 4.5 Aggressive Original File Preloading
- **Location:** `static/app-*.js` (`preloadAdjacentImages`)
- **Problem:** The viewer preloads adjacent full-resolution images by inserting hidden `1x1` `<img>` elements into the DOM. For modern 24–48 MP camera images, this triggers 20–50 MB background downloads on each photo view, saturating mobile bandwidth.
- **Recommendation:** Use standard `new Image().src = ...` without DOM pollution, and consider preloading the next thumbnail first or debouncing full-resolution preloading.

### 4.6 Hardcoded Dummy Video Duration
- **Location:** `src/api.rs` (`get_album`) & `static/app-*.js`
- **Problem:** `api.rs` hardcodes `duration: Some(0)` for all videos. Because `0` is falsy in JavaScript, `photo.duration ? ...` in `app.js` never renders.
- **Recommendation:** Either extract duration via `ffprobe` and store it in SQLite alongside width/height, or remove the duration field until implemented.

---

## 5. Code Health, Testing & Tooling

### 5.1 Add Automated Tests
- **Status:** Currently **0 tests** exist in the repository (`cargo test` passes 0 tests).
- **Recommendation:** Add unit tests for core helper functions:
  - `util::validate_path` (parent dir traversal, root paths, Windows separators, special characters).
  - `util::is_ancestor` and `api::build_breadcrumbs`.
  - `util::thumb_name`.
  - `api::escape_html` and `api::encode_component`.

### 5.2 Resolve 11 Compiler Clippy Warnings
- **Clippy Findings:**
  - Replace `entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()))` with `entries.sort_by_key(|a| a.file_name())` in `api.rs`.
  - Collapse nested `if` statements in `thumb.rs` and `worker.rs`.
  - Remove redundant borrowed slice syntax `&[...]` in `Command::args` in `thumb.rs`.

### 5.3 Remove Redundant SQLite Table Indexes
- **Location:** `src/db.rs` (`init`)
- **Problem:**
  ```sql
  CREATE TABLE IF NOT EXISTS photo_metadata (
      photo_path TEXT PRIMARY KEY,
      ...
  );
  CREATE INDEX IF NOT EXISTS idx_photo_meta_path ON photo_metadata(photo_path);
  ```
  In SQLite, declaring `photo_path TEXT PRIMARY KEY` automatically generates an underlying unique B-tree index. Creating `idx_photo_meta_path` creates a duplicate index that consumes extra disk space and slows down write operations. The same applies to `idx_covers_path` on `folder_covers`.
- **Recommendation:** Drop the redundant `CREATE INDEX` statements.

### 5.4 Automate Asset Versioning
- **Location:** `static/` asset workflow
- **Problem:** Cache busting relies entirely on manually renaming files (e.g. `style-2026-09-16-1253.css` and `app-2026-09-16-1515.js`) and editing `index.html`. This is error-prone and tedious.
- **Recommendation:** Switch to query-string asset hashing (e.g. `style.css?v=<hash>`) or provide a short release script to automate timestamp renaming.

---

## 6. Documentation Discrepancies

1. **Video Thumbnail Generation:** `DESIGN.md` Section 3 states video thumbnails are extracted at the 10% mark. In `thumb.rs`, it is hardcoded to `-ss 00:00:01` (1 second).
2. **Configuration Search Path:** `DESIGN.md` Section 2 states default config paths adapt per platform via the `dirs` crate. The `dirs` crate is not in `Cargo.toml`; `config.rs` strictly requires the `SIMPLE_ALBUM_CONFIG` environment variable.
3. **Asset Filenames:** `DEPLOY.md` refers to `app-2026-09-16-1253.js`, while the repository contains `app-2026-09-16-1515.js`.
4. **Filesystem Recursion Claim:** `README.md` asserts that `get_album()` never recurses and performs exactly one `readdir` per page load. Update the documentation to reflect recursive badge counting, or implement count caching.
