# Comprehensive Code & Documentation Audit: Simple Photo Album

**Repository:** `simplealbum`  
**Auditor:** Gemini (via Pi Coding Harness)  
**Date:** March 2026  
**Scope:** Backend (Rust 2024 / Axum), Frontend (HTML5 / Vanilla JS / CSS), Database (SQLite / rusqlite), Scripts (`sync_photos.sh`), and Documentation (`README.md`, `DESIGN.md`, `DEPLOY.md`, `DEPLOY_MAC_DEV.md`).

---

## 1. Executive Summary

**Simple Photo Album** is an exceptionally well-conceived, self-hosted photo and video gallery. Built around the **KISS (Keep It Simple, Stupid)** philosophy, it sidesteps the bloat, fragility, and heavy database synchronization layers typical of modern media servers (e.g., Immich, Photoprism, Nextcloud). 

The system treats the **filesystem as the single source of truth**, pairs an edge reverse proxy (Caddy/Nginx) for static/media delivery with a lightweight Rust backend for indexing, thumbnailing, and metadata, and serves a zero-framework, dependency-free vanilla frontend.

### Overall Assessment
- **Architecture:** ⭐⭐⭐⭐⭐ (5/5) — Elegant, clean separation of concerns; fast startup; minimal resource footprint.
- **Backend Code Quality:** ⭐⭐⭐⭐☆ (4.5/5) — Idiomatic Rust 2024, excellent error handling, disciplined path sanitization, and defensive subprocess management. Minor issues exist around blocking I/O on async threads and task queuing during large startup scans.
- **Security Posture:** ⭐⭐⭐⭐⭐ (4.8/5) — Exemplary directory traversal protection and XSS mitigation. Strong separation of privileges. Minor considerations around plain-text admin key logging and non-constant-time key comparison.
- **Frontend & UX:** ⭐⭐⭐⭐☆ (4.5/5) — Lightweight, accessible, responsive, with smart client-side preloading and social link preview workarounds (`/api/share`). Minor maintenance friction around manual timestamp cache-busting.
- **Documentation:** ⭐⭐⭐⭐⭐ (5/5) — Outstanding. Clear, thoughtful, honest about limitations, and packed with production deployment recipes across Linux, macOS, and Windows.

---

## 2. Architecture & System Design

### 2.1 The Edge vs. API Split
```
┌─────────────┐     ┌─────────────────────────────────────────────────────────┐
│   Browser   │────▶│ Caddy / Reverse Proxy (Port 443 / TLS)                  │
│  (Client)   │     │ • Serves static assets: / → static/                     │
└─────────────┘     │ • Direct media streaming: /photoalbum/* → album/ root   │
                    │ • Proxies dynamic JSON & Share: /api/* → localhost:8080 │
                    └─────────────────────────────────────────────────────────┘
                                                 │
                                                 ▼
                                    ┌────────────────────────┐
                                    │   Rust Album Service   │
                                    │    (axum + tokio)      │
                                    │ • Filesystem Watcher   │
                                    │ • Thumbnail Worker     │
                                    │ • Metadata & Covers DB │
                                    └────────────────────────┘
```
**Why this is brilliant:**
1. **Zero Media Proxy Overhead:** High-bandwidth image delivery and video range streaming (HTTP 206) bypass the application runtime entirely. The web server handles byte ranges, gzip/zstd compression, and TLS acceleration directly from the kernel (`sendfile`).
2. **Stateless API:** The Rust backend holds no directory tree in memory. A folder request triggers a fast local `readdir`, queries SQLite for cached dimensions and cover overrides, and immediately outputs JSON.
3. **True Zero-Lock Invalidation:** When a user uploads or removes a file via SFTP or `rsync`, the filesystem watcher updates the database and regenerates thumbnails automatically without requiring manual re-indexing or database rebuilding.

### 2.2 Key Architectural Trade-offs
- **Live Traversal vs. Precomputed Tree:** Reading the filesystem on every request guarantees that the gallery is 100% consistent with disk. However, subfolder badge counting (`count_contents`) and thumbnail cover fallback (`find_first_thumb_recursive`) perform recursive disk I/O at request time (see [Section 5.2](#52-synchronous-filesystem-traversal-inside-axum-async-handlers)).
- **Embedded SQLite in WAL Mode:** Using SQLite solely for folder cover choices and dimension/duration caching strikes the right balance between flat-file simplicity and relational lookup speed.

---

## 3. Backend Code Review (`src/`)

### 3.1 `src/main.rs` & Subprocess Management
- **Startup Flow:** The startup sequence starts the filesystem watcher *before* initiating the background initial scan. This is a critical detail: files modified during the scan are not missed.
- **Diagnostic Tool Checks:** `check_media_tools()` independently verifies `ffmpeg` and `ffprobe`. Many distributions package `ffprobe` separately from `ffmpeg`; diagnosing this at boot prevents silent thumbnail/duration failures.
- **Clean Signal Handling:** Relies on Axum’s built-in graceful lifecycle.

### 3.2 `src/config.rs`
- **Fail-Fast Philosophy:** No hidden fallbacks, no default guessing, and no auto-creation of config files. If `SIMPLE_ALBUM_CONFIG` is unset or invalid, the binary refuses to start with an actionable error message.
- **Strict Validation:** Enforces non-empty strings, validates directory existence for `album.root`, checks `server.public_url` for protocol (`http://` or `https://`) and trailing slash, and ensures `admin.key` is non-empty.

### 3.3 `src/db.rs`
- **Schema & Pragmas:** Automatically enables `PRAGMA journal_mode = WAL;` and `PRAGMA busy_timeout = 5000;`. The busy timeout is essential for avoiding immediate `SQLITE_BUSY` errors when an operator or backup tool inspects the database concurrently.
- **Atomic Operations:** `set_cover` and `set_metadata` use modern `INSERT INTO ... ON CONFLICT DO UPDATE` (upsert) queries.
- **Prefix Matching Safety:** In `delete_metadata_under` and `delete_covers_under`:
  ```sql
  DELETE FROM photo_metadata
  WHERE substr(photo_path, 1, length(?1) + 1) = ?1 || '/'
  ```
  The code uses `substr` instead of `LIKE '?1/%'`, preventing folder names with `%` or `_` from accidentally wildcard-matching unrelated paths. The `+ 1` accounts for the trailing `/`.
- **Concurrency Consideration:** `Db` wraps a single `rusqlite::Connection` in a `std::sync::Mutex<Connection>`. While WAL mode allows multiple concurrent readers, using a single Mutex serializes every single read and write. For high-concurrency environments, a connection pool (such as `r2d2_sqlite`) or separate read connections would unlock SQLite's concurrent WAL read capabilities. For personal album traffic, however, this mutex is rarely contended.

### 3.4 `src/util.rs`
- **Path Sanitization:** `validate_path()` parses path components using `std::path::Component`. It explicitly rejects `ParentDir` (`..`), `RootDir` (`/`), and `Prefix` (`C:` on Windows).
- **Symlink Jail Resolution:** `resolve_album_path()` canonicalizes the target path and asserts `canonical.starts_with(&root_canonical)`. This effectively defeats symlink-based jailbreak attempts.
- **Ancestor Checks:** `is_ancestor()` correctly enforces boundary checks (e.g., verifying `child.len() == parent.len() || child[parent.len()..].starts_with('/')`), preventing `1970` from being falsely considered an ancestor of `19700`.

### 3.5 `src/thumb.rs`
- **Subprocess Safety (`run_with_timeout`):**
  Spawns background threads to drain stdout and stderr concurrently:
  ```rust
  fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>>
  ```
  This prevents deadlocks where a child process (like FFmpeg) writes more data than the OS pipe buffer and blocks indefinitely while the parent waits.
- **Atomic File Writing:** Thumbnails are written to unique hidden temporary files (`.12.photo_thumb.jpg.tmp`) and atomically renamed into place via `std::fs::rename`. This guarantees that crashes or power cuts never leave corrupt, truncated thumbnail files on disk.
- **Empty Image / Decoder Corruption Detection:** For JPEG thumbnails, `meta.len() < 1024` flags suspicious solid-grey or corrupt outputs and cleans them up.
- **EXIF Orientation:** Respects EXIF tags 1 through 8 and computes rotated dimensions accurately for portrait smartphone photos.

### 3.6 `src/worker.rs` & `src/watcher.rs`
- **Upload Race Protection (Stability Gate):**
  The `await_stable()` mechanism solves a classic media server problem: inotify fires the millisecond a file is created, before any bytes are written. By checking size and mtime over consecutive 300ms intervals (up to 3 samples) and falling back to the trailing `Modify`/`Close(Write)` event, the worker avoids decoding partial files.
- **Mtime Drift Resilience:** If an image has an mtime in the future (e.g. camera clock misconfigured), `thumb_is_fresh` uses `now - UNTRUSTED_MTIME_GRACE` so the worker doesn't re-generate the thumbnail on every single pass.
- **Iterative Directory Walk:** `walk_dir` uses an explicit `Vec<PathBuf>` stack instead of recursive function calls, eliminating stack overflow risks on deeply nested directories.

---

## 4. Security & Privacy Audit

| Security Domain | Status | Analysis |
|---|---|---|
| **Path Traversal** | **Secure** | Robust multi-layer defense (`Component` inspection + canonical prefix check). |
| **Cross-Site Scripting (XSS)** | **Secure** | All dynamic DOM injections in JS use `escapeHtml()` or `document.createTextNode()`. Backend `/api/share` escapes all tags and attributes. |
| **CSRF / Cross-Origin Attacks** | **Secure** | `set_cover` requires custom `X-Admin-Key` header (triggers CORS preflight) and validates `Origin` against `public_url`. |
| **Path Traversal** | **Secure** | Robust multi-layer defense (`Component` inspection + canonical prefix check). |
| **Cross-Site Scripting (XSS)** | **Secure** | All dynamic DOM injections in JS use `escapeHtml()` or `document.createTextNode()`. Backend `/api/share` escapes all tags and attributes. |
| **CSRF / Cross-Origin Attacks** | **Secure** | `set_cover` requires custom `X-Admin-Key` header (triggers CORS preflight) and validates `Origin` against `public_url`. |
| **Threat Boundary Separation** | **Secure** | Uploading is entirely out-of-band via SSH/SFTP; no mutating file APIs exposed on the server. |
| **Credential Storage** | **Moderate** | Admin key logged in plaintext to stdout at boot; stored in browser `localStorage`. |
| **Side-Channel Timing** | **Minor** | `admin_key != state.config.admin.key` is not constant-time. |

### 4.1 Client-Side Helper Script Robustness (`sync_photos.sh`)
`sync_photos.sh` is an optional client-side convenience script intended to assist the album owner in uploading local folders over SSH/rsync. Because it executes solely on the operator's local machine with their own photos and credentials, it is **outside the server's security perimeter and poses no remote security risk**.

However, in the interactive drag-and-drop fallback (line 41):
```bash
echo -n "Enter or drag-and-drop file(s) or folder(s): "
read -r USER_INPUT
...
# Parse drag-and-dropped paths (handles quotes and escaped spaces)
eval "SOURCES=($USER_INPUT)"
```
Using `eval` to parse shell words causes practical **brittleness and UX friction** when handling filenames containing spaces, single quotes, or apostrophes (e.g. `Bob's 50th.jpg`), as noted in `README.md`. 

**Improvement for Usability:**
Rather than `eval`, users can continue to pass filenames as standard quoted CLI arguments (e.g. `./sync_photos.sh "path/to/dir"`), or the interactive prompt can read paths cleanly using `read -r` line-by-line, avoiding shell evaluation surprises on awkward photo names.

### 4.2 Admin Key Handling & Operational Exposure
1. **Startup Log:**
   ```rust
   info!("Admin URL: {}#admin={}", cfg.server.public_url, cfg.admin.key);
   ```
   *Pros:* Extremely convenient for headless setup on personal servers.  
   *Cons:* Systemd `journalctl`, launchd log files, and container logging aggregators store the admin secret in cleartext. If logs are shared or exposed via an unprivileged daemon, the admin key is compromised.  
   *Mitigation:* Retain the convenience, but add an option to suppress secret logging (e.g., `SIMPLE_ALBUM_REDACT_SECRETS=1`), or log `Admin URL: <configured in album.toml>`.
2. **Timing Attack:**
   In `api.rs`:
   ```rust
   if admin_key != state.config.admin.key || state.config.admin.key.is_empty()
   ```
   Standard string comparison short-circuits on the first byte mismatch. While exploitation across a network reverse proxy is difficult, best practice for secret token verification is constant-time comparison (e.g., using `subtle::ConstantTimeEq` or SHA-256 hash comparison).

---

## 5. Performance & Scalability Analysis

### 5.1 The Startup Task Storm in `worker.rs` (Bottleneck)
In `worker.rs`, the channel is unbounded:
```rust
let (tx, mut rx) = mpsc::unbounded_channel::<ThumbJob>();
```
When `scan_existing()` walks a library with 10,000–50,000 photos on first launch:
```rust
ThumbJob::Create { rel_path } => {
    let root = root.clone();
    let db = db.clone();
    let semaphore = semaphore.clone();
    tokio::spawn(async move {
        if !await_stable(&root, &db, &rel_path).await {
            return;
        }
        let permit = semaphore.acquire_owned().await.unwrap();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            process_create(&root, &db, &rel_path);
        });
    });
}
```
**The Issue:**
The consumer loop immediately drains all 50,000 messages and spawns **50,000 independent Tokio tasks**. 
1. Each task executes `await_stable()`, making filesystem `stat` calls and acquiring the single SQLite database mutex via `db.get_metadata()`.
2. While image decoding is properly throttled by `semaphore` (clamped to 2–8 concurrent threads), the task creation, filesystem checks, and database lock acquisitions are completely unconstrained.
3. This can lead to high memory consumption (task structures in Tokio) and severe SQLite lock contention during large initial imports.

**Recommendation:**
Use a bounded channel (e.g., `mpsc::channel(256)`) or throttle task intake. The sender in `scan_existing` will naturally yield or block when the backlog is full, keeping peak memory and mutex contention bounded.

### 5.2 Synchronous Filesystem Traversal Inside Axum Async Handlers
In `api.rs`, `get_album` is an `async fn`:
```rust
pub async fn get_album(
    Query(query): Query<AlbumQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<AlbumResponse>, StatusCode>
```
Within this async function:
1. `std::fs::read_dir(&abs_path)` runs synchronously.
2. For every child folder, `count_contents()` walks the entire directory subtree synchronously using an iterative DFS to calculate photo and album counts.
3. `find_first_thumb_recursive()` checks the folder, immediate children, and grandchildren synchronously if no explicit cover is set.

**The Issue:**
In Tokio, executing synchronous blocking filesystem operations directly inside an async task stalls the Tokio worker thread. If the photo tree contains 10,000 items and multiple users load the gallery root at the same time, all available Tokio worker threads can become blocked waiting for synchronous disk I/O, degrading API responsiveness.

**Recommendation:**
Wrap the filesystem read logic in `tokio::task::spawn_blocking`:
```rust
let album = tokio::task::spawn_blocking(move || {
    read_album_from_disk(...)
}).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)??;
```

---

## 6. Media Processing & Metadata Insights

### 6.1 Video Orientation Bug (Discrepancy Between Probe and Output)
In `thumb.rs`, `probe_video()` runs:
```bash
ffprobe -v error -select_streams v:0 -show_entries stream=width,height:format=duration -of json
```
For videos shot on modern smartphones (portrait orientation), many MP4/MOV files store raw coded frames horizontally (e.g., 1920×1080) with a container rotation tag (`rotate=90` or displaymatrix side data).
1. `ffprobe` returns `width: 1920, height: 1080`.
2. Later, when `ffmpeg` extracts the poster frame, it **automatically applies the rotation** (producing a 225×400 portrait JPEG).
3. The database and API report `1920x1080` (landscape), but the thumbnail shown in the UI is portrait!

**Recommendation:**
Query stream side data in `ffprobe` or check the `rotate` tag in stream tags (`-show_entries stream_tags=rotate:stream_side_data:stream=width,height...`). If rotated by 90° or 270°, swap width and height.

### 6.2 Video Frame Extraction Seek Edge Case
In `thumb.rs`:
```rust
let seek = match duration_secs {
    Some(duration) => (duration * 0.1).clamp(0.0, (duration - 0.1).max(0.0)),
    None => 1.0,
};
```
If `ffprobe` fails to extract the duration (e.g., live stream dump, truncated container, or certain MKV/AVI files) and the clip is shorter than 1.0 second, seeking to `1.0` lands past EOF. In this scenario, `ffmpeg` exits with an error and no thumbnail is produced.
- **Fix:** When `duration_secs` is `None`, default to `0.0` or `0.1` rather than `1.0`.

### 6.3 Lexicographical vs. Natural Filename Sorting
In `api.rs`:
```rust
entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
```
Standard byte-wise lexicographical sorting orders strings as:
`photo1.jpg`, `photo10.jpg`, `photo11.jpg`, `photo2.jpg`, `photo3.jpg`...
Most users expect **natural/alphanumeric sorting** (`photo1.jpg`, `photo2.jpg`, ..., `photo10.jpg`). While users can zero-pad names (`photo01.jpg`), adopting natural sorting or providing it as an option would significantly enhance UX.

---

## 7. Frontend Code Review (`static/`)

### 7.1 Architecture & Highlights
- **Framework-Free Simplicity:** Plain ES2020+ JavaScript with zero build pipeline, zero bundlers, and zero `node_modules`.
- **Browser History Integration:** Outstanding routing. Viewer state is pushed to `history.pushState({path, view: index})`. Pressing the browser's Back button closes the modal viewer without navigating away from the album folder.
- **Touch & Gesture Navigation:** Features clean, passive horizontal swipe detection for mobile screens.
- **Adjacent Image Preloading:** Automatically creates invisible, async-decoded `<img>` elements for adjacent images (`index - 1` and `index + 1`), ensuring instant transitions when browsing high-resolution photos.
- **Social Sharing Bridge (`/api/share`):** A clever solution to the SPA preview limitation. Since crawlers do not execute JS and ignore URL fragments (`#path=...`), the share sheet points to `/api/share?path=...`. The backend serves a minimal HTML card with OpenGraph metadata and immediately issues a `<meta http-equiv="refresh">` redirect to the SPA.

### 7.2 Frontend Observations & Areas for Improvement
1. **Hardcoded Personal Metadata in `index.html`:**
   `static/index.html` contains production-specific data from the author:
   ```html
   <title>Photo Album</title>
   ...
   <meta property="og:site_name" content="Bob &amp; Karen's Photo Album">
   <meta property="og:image" content="https://www.osola.org.uk/photos/og-image.png">
   ...
   <h1>Bob &amp; Karen's Photo Album</h1>
   <p><a href="/index.html">return </a>to main site</p>
   ```
   While `DEPLOY.md` clearly lists the 8 places to customize, new adopters who clone the repo and forget to edit `index.html` will inadvertently deploy with someone else's names and links. Providing clean placeholder variables or generic defaults would make adoption smoother.
2. **Asset Cache-Busting Maintenance:**
   Files are named with timestamps like `app-2026-09-16-2243.js` and `style-2026-09-16-2243.css`. Whenever a frontend line is changed, the developer must:
   - Rename the JS/CSS files on disk.
   - Edit the `<link>` and `<script>` tags in `index.html`.
   - Update `Caddyfile` or proxy configurations if matching patterns changed.
   *Alternative:* A simple pre-commit hook, Makefile, or build script that computes a SHA-256 hash or automated timestamp would eliminate human error.

---

## 8. Documentation Review

The documentation in this repository is of exceptionally high quality.
- **`README.md`**: Well-written, engaging, and sets clear expectations (KISS principle, supported/unsupported formats, non-features, and transparent performance benchmarks).
- **`DESIGN.md`**: Detailed technical specification covering architecture, data flows, SQLite schemas, and threat modeling.
- **`DEPLOY.md`**: Comprehensive production guide with ready-to-use service unit files for **systemd** (Linux), **launchd** (macOS), and **NSSM** (Windows), along with hardening flags (`ProtectSystem=strict`, `MemoryMax=512M`).
- **`DEPLOY_MAC_DEV.md`**: Clean, accessible guide for local testing with Caddy.

### Doc Drift & Minor Notes
- In `prompts.txt`, earlier specifications mentioned "ES16 vanilla Javascript". `DESIGN.md` accurately modernizes this to "ES2020+".
- Asset names in documentation examples frequently change to illustrate versioning, which is properly explained in `DEPLOY.md`.

---

## 9. Testing & Quality Assurance

### 9.1 Existing Coverage (17 Passing Tests)
- `api.rs`: HTML escaping, URI encoding, origin parsing, breadcrumbs generation, and cover thumbnail path calculations.
- `thumb.rs`: Extension-to-encoder mapping, EXIF rotation parsing, temp file isolation, and timeout execution for child processes.
- `util.rs`: Media classification, thumbnail stem derivation, ancestor path logic, and path traversal rejection.
- `watcher.rs`: Root path stripping and `thumbs/` directory exclusion.

### 9.2 Key Test Gaps
1. **`src/db.rs` has 0 unit tests:** The SQL prefix deletions (`delete_metadata_under`, `delete_covers_under`) and `delete_cover_if_matches` are vital for data integrity and should be tested against an in-memory SQLite database (`Connection::open_in_memory()`).
2. **`src/worker.rs` has 0 unit tests:** `metadata_incomplete()`, `thumb_is_fresh()`, and `await_stable()` logic are untested.
3. **No End-to-End API Integration Tests:** There are no tests spinning up Axum with `tower::ServiceExt::oneshot` to verify HTTP responses (`/api/album`, `/api/health`, `/api/cover`, `/api/share`).

---

## 10. Prioritized Recommendations & Action Items

### Priority 1: Security & Stability (Immediate)
1. **Bound Worker Task Concurrency:** Replace `unbounded_channel` with a bounded channel (`mpsc::channel(128)`), or throttle `tokio::spawn` calls during `scan_existing` to avoid task flooding and SQLite lock contention.
2. **Offload Blocking Directory Walks from Async API Handlers:** Wrap `count_contents()` and `find_first_thumb_recursive()` in `tokio::task::spawn_blocking` inside `get_album()`.

### Priority 2: Correctness & Usability (Short-Term)
3. **Fix Video Rotation in Metadata:** Update `probe_video()` to inspect rotation tags so portrait videos do not report flipped dimensions.
4. **Safe Fallback for Unknown Video Duration:** Change the fallback seek time in `generate_video_thumb()` from `1.0s` to `0.0s` to handle clips shorter than 1 second.
5. **Constant-Time Admin Key Comparison:** Use `subtle::ConstantTimeEq` or hash comparison for `X-Admin-Key` verification.
6. **Robust Interactive Prompt in `sync_photos.sh`:** Replace `eval` with line-by-line reading to eliminate word-splitting issues on photos with apostrophes or spaces.
7. **Provide Neutral Default Branding in `static/index.html`:** Replace hardcoded personal URLs and names with clean placeholders.

### Priority 3: Polish & Maintenance (Long-Term)
8. **Adopt Natural Sorting:** Implement alphanumeric/natural sort for folder and media listings.
9. **Add In-Memory Tests for `db.rs`:** Verify database prefix deletion queries and schema migration idempotency.
10. **Automate Cache-Busting Asset Renaming:** Provide a minimal build/release script to hash static assets and update `index.html` automatically.

---

## 11. Conclusion

`simplealbum` is a refreshing, high-performance, and pragmatic approach to self-hosted media management. It proves that a modern photo album doesn't need heavy microservices, multi-gigabyte memory footprints, or complex database synchronization. The architecture is sound, the Rust implementation is robust and defensive, and the documentation is top-tier. Addressing the few concurrency, media probing, and script parsing items identified above will make it virtually bulletproof.
