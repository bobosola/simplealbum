# Photo Album Service — Design Document

## 1. Overview

A self-hosted photo album web application. The user manages photos by adding folders and images directly to the filesystem. A Rust service watches for changes, generates thumbnails, and serves a JSON API. Caddy acts as the edge server — serving static frontend assets, proxying API calls, and directly serving photos and thumbnails.

The frontend is plain HTML, CSS, and vanilla JavaScript (ES2020+). No frameworks, no build step.

**Cross-platform by design.** The Rust service compiles and runs on Linux, macOS, and Windows without source changes. The filesystem watcher (`notify` crate) automatically selects the correct backend for each OS: inotify on Linux, FSEvents on macOS, ReadDirectoryChangesW on Windows. See `DEPLOY.md` for per-platform installation instructions.

---

## 2. Architecture

```
┌─────────────┐     ┌──────────────────────────────────────┐
│   Browser   │────▶│  Caddy (port 443)                       │
└─────────────┘     │  • Static files: / → static/            │
                    │  • Photos:     /photoalbum/* → album/   │
                    │  • API:       /api/* → localhost:8080   │
                    └─────────────────────────────────────---─┘
                                         │
                                         ▼
                              ┌────────────────────┐
                              │ Rust Album Service │
                              │ (localhost:8080)   │
                              │  • File watcher    │
                              │  • Thumbnail gen   │
                              │  • HTTP API        │
                              │  • SQLite state    │
                              └────────────────────┘
```

### Why this split?

- **Caddy** handles TLS, static file serving, and reverse proxying.
- **Rust** handles the dynamic work: watching the filesystem, resizing images, and maintaining metadata. Keeping this as a separate service avoids embedding a web framework into Caddy or writing Caddy modules.
- **Cross-platform**: The entire Rust backend compiles on Linux (Debian), macOS, and Windows with zero conditional compilation. The `notify` crate abstracts the OS-specific watcher APIs. Default config paths adapt per platform via the `dirs` crate.

---

## 3. Folder & Naming Conventions

### Photo Tree (managed by user)

```
/album/                     ← root path passed to Rust service
├── 1960-69/
│   ├── 1960/
│   │   ├── photo1.jpg
│   │   ├── photo2.jpg
│   │   └── summer holiday/
│   │       ├── photo3.jpg
│   │       └── thumbs/
│   │           └── photo3_thumb.jpg
│   │   └── thumbs/
│   │       ├── photo1_thumb.jpg
│   │       └── photo2_thumb.jpg
│   ├── 1961/
│   └── thumbs/
├── 1970-79/
│   ├── 1970/
│   └── 1971/
└── thumbs/                  ← optional: decade-level cover thumbs
```

**Suggested convention** (not enforced by the app):
- Top level: decade spans named `YYYY-YY` (e.g. `1970-79`).
- Second level: single years named `YYYY` (e.g. `1971`).
- Third level and deeper: event/album folders with free names (e.g. `Christmas`, `summer holiday`).

Any valid folder names are accepted; the app simply reads directories recursively and sorts contents lexicographically by filename.
- **Images**: JPEG, PNG, WEBP.
- **Videos**: MP4 (H.264), MOV, AVI, WebM, MKV. Playback uses the browser's native HTML5 `<video>` player (supported by all browsers from 2020 onwards). For maximum compatibility, MP4 with H.264 is recommended.
- **Thumbnails**: generated automatically in a `thumbs/` subfolder within any folder that contains media.
  - Image thumbnails: `<image_stem>_thumb.<ext>` (same extension as source).
  - Video thumbnails: `<video_stem>_thumb.jpg` (JPEG extracted at the 10% mark via FFmpeg).
- **Ordering**: Folders and photos within each folder are displayed in **lexicographic filename order** (A-Z, 0-9). This gives the user full control over ordering by renaming files. No EXIF date parsing or mtime-based sorting is used.

### State Storage (managed by Rust service)

```
/var/lib/album/album.db     ← SQLite database (outside photo tree)
```

Stores:
- `folder_covers` table: `folder_path` → `cover_image_name` (chosen by user, persisted across restarts).
- Optional caching metadata (last scan time, file count, etc.).

---

## 4. Configuration

The Rust service is configured via a TOML file. No command-line arguments are required.

### Config File Locations (searched in order)

1. Path from environment variable: `SIMPLE_ALBUM_CONFIG=/path/to/album.toml`
2. `/etc/album/album.toml` (system-wide, for systemd deployments)
3. `~/.config/album/album.toml` (user-local, for manual runs)

### Example `album.toml`

```toml
[server]
# API bind address and port. Change this if 8080 is in use.
bind = "127.0.0.1:8080"

[album]
# Absolute path to the root of your photo tree.
root = "/var/album"

[state]
# Where the SQLite database lives.
db_path = "/var/lib/album/album.db"

[worker]
# Concurrent thumbnail generation jobs. 0 = auto (CPU core count, clamped
# to 2-8). Each job can hold one full decoded frame in RAM, so this is the
# service's main memory lever — lower it on small servers.
threads = 0

[admin]
# Pre-shared key for cover selection and other write operations.
# If left empty or omitted, the service generates one on first startup
# and writes it back into this file.
key = ""
```

### First-Run Admin Key Generation

On first startup, if `[admin] key` is empty or missing:

1. The service generates a cryptographically random 32-byte string, base64url-encoded (e.g. `xT9vK2mNqLpR5sW8yZ3aB7cE1fG4hJ6k`).
2. Writes the key back into `album.toml` under `[admin] key`.
3. Logs it prominently:
   ```
   [INFO] First run detected. Admin key generated and saved to /etc/album/album.toml
   [INFO] Admin URL: https://album.example.com/#admin=xT9vK2mNqLpR5sW8yZ3aB7cE1fG4hJ6k
   ```
4. Thereafter, an empty string is never accepted as a valid key.

The admin bookmarks this URL. On page load, the frontend JavaScript reads `window.location.hash`, stores the key in `localStorage` as `album_admin_key`, and immediately strips it from the URL via `history.replaceState()`. The key is sent as the `X-Admin-Key` header on every write request.

**Why the fragment (`#`) instead of query (`?`):** The URL fragment is never sent to the server, never appears in access logs, and is stripped from `Referer` headers when navigating to external sites. A query string would leak the key into Caddy logs and third-party analytics.

### Port Configuration

If port 8080 is unavailable, change one line in `album.toml`:

```toml
[server]
bind = "127.0.0.1:8081"
```

Then update the Caddy reverse proxy accordingly.

---

## 5. Thumbnail Strategy

- **Location**: A `thumbs/` subfolder inside every directory that contains one or more images.
- **Naming**: `<original_name>_thumb.<ext>`. Example: `beach.jpg` → `beach_thumb.jpg`.
- **Size**: Fixed maximum dimension, e.g. 400px width or height, maintaining aspect ratio.
- **Image thumbnail format**: JPEG at quality 85. If source is PNG with transparency, thumbnail is PNG.
- **Video thumbnail format**: JPEG at quality 85, 400px max dimension, extracted at the 10% timestamp (or first available keyframe). A small play-icon overlay is rendered by the CSS/frontend; it is not baked into the JPEG.
- **EXIF orientation**: The thumbnail worker reads the EXIF `Orientation` tag (via `kamadak-exif`) and rotates the output accordingly. This prevents portrait photos from appearing sideways. The full-size viewing image is served as-is (browsers handle EXIF orientation natively in `<img>` tags since 2019+).
- **Lifecycle**:
  - On startup, the service does a quick consistency scan to queue missing or stale thumbnails, then starts the API immediately. Thumbnail generation happens in a **background worker pool** (auto-sized to the CPU core count, clamped to 2–8, and configurable via `[worker] threads`) so the service is usable within seconds even with a large backlog.
  - At runtime, `notify` (inotify on Linux) watches the album root with **recursive mode**. A single watch covers the entire tree, avoiding `max_user_watches` exhaustion. On `Create`/`Modify` events, jobs enter an **async pre-pass** (see [In-Flight Upload Protection](#in-flight-upload-protection)) before reaching the worker pool. On `Remove` events, thumbnails are deleted synchronously.
  - If a `thumbs/` folder becomes empty, it may be removed.
  - **Video thumbnail generation** uses FFmpeg (system dependency). The worker shells out to `ffmpeg -ss <10pct> -i <input> -vframes 1 -q:v 2 <thumb.jpg>`. Per-job timeouts are not currently implemented (see [Resource Protection & Limits](#resource-protection--limits)); the worker cap and `TasksMax` contain the worst case.
  - **Error handling during generation**: If a file cannot be decoded (corrupt image, unsupported format, FFmpeg failure), the worker logs a warning, skips the file, and moves on. The file does not appear in API listings until it can be processed successfully.

**Rationale**: The user explicitly wants thumbnails co-located with photos. This integrates seamlessly with Caddy's static file server — no special routing rules are needed.

### In-Flight Upload Protection

Photographs are normally added by copying files into the tree (rsync, scp, rclone, a sync client). Filesystem watchers fire `IN_CREATE` the instant a file *appears* — before a single byte is written — and `Modify` on every write chunk. Without protection, the worker would race the upload and decode a **truncated** image: decoders are lenient about partial data, producing a thumbnail that is mostly solid grey with a strip of real pixels. Worse, the old behaviour cached the bad thumbnail forever (an existing thumbnail was never regenerated), so the file only recovered after being moved to a new folder.

Three mechanisms cooperate to make this class of bug impossible in the steady state:

1. **Stability sampling (async pre-pass).** Before a job may touch a worker, an async task samples the file's *size and mtime*, waits ~300 ms, and re-samples; a file that is actively being written shows a change, so sampling repeats until the file goes quiet (up to 3 samples, then the job defers). Both size and mtime are compared because some copy/upload tools pre-allocate the full file size up front, which a size-only check would miss. The pre-pass runs as lightweight per-job async tasks, so hundreds of in-flight uploads wait out their writes **in parallel** without occupying worker slots; the final `Modify` event fired when a write completes re-triggers any deferred job. A fixed delay is deliberately *not* used: slow uploads, network stalls, or backlog queue time can exceed any fixed value.
2. **mtime-based staleness (self-healing).** A thumbnail is considered stale when the source's mtime is newer than the thumbnail's mtime, and stale thumbnails are regenerated. If the stability check ever loses the race (e.g. an upload pauses mid-file long enough to look stable), the final write event marks the bad thumbnail stale and repairs it within a second. The startup scan applies the same check, so corrupt thumbnails left behind by an older version of the service are all repaired on the next restart.
3. **Atomic, collision-free writes.** Thumbnails are written to a unique hidden temp file (`.7.photo_thumb.jpg.tmp`) and renamed into place, so a crash or a duplicate concurrent job can never leave a partially written thumbnail or rename the temp file out from under its sibling job.

The residual risk is bounded and self-correcting: at worst one bad thumbnail exists briefly, and the next event (or next restart) fixes it. Watcher-event *churn* (the many `Modify` events per in-flight write) is cheap by design — the staleness check runs first and returns immediately when nothing needs doing, so no sleeping and no decoding happens for events that are no-ops.

### Resource Protection & Limits

The service is designed to accept **unlimited** upload volumes (bounded only by disk space) without failing itself or degrading other services such as the web server. Uploads that back up only add queue latency; nothing in the design scales resource use with queue depth.

- **Bounded memory via the worker count.** Each generation job can hold one full decoded frame in RAM (a 24 MP photo decodes to ~72 MB), so total memory use is bounded by `[worker] threads`, never by the number of queued files or in-flight uploads. Measured peak RSS: ~200 MB at 2 workers (24 MP photos), ~460 MB at 8 workers (12 MP), ~790 MB at 8 workers (24 MP); idle is ~10 MB. Queued jobs are small strings; the async pre-pass tasks are coroutines, not OS threads. This count is the service's single memory lever: small servers pin a low value and keep a modest `MemoryMax`; larger machines leave it at `0` (auto).
- **Bounded CPU via `CPUQuota`.** Under systemd the unit caps total CPU (e.g. `CPUQuota=150%` on a 2-core box: bursts get nearly the whole machine, but other services are guaranteed the rest). Thumbnail bursts therefore finish more slowly under quota rather than starving Caddy or system processes; after a burst the service idles at ~0% CPU.
- **Bounded tasks via `TasksMax`.** The service's OS thread count plus any FFmpeg child processes stays well under the limit (measured peak ~33 with 8 workers on video-heavy input; ~20 at 2 workers); `TasksMax=50` leaves comfortable headroom.
- **`MemoryMax` sizing per server class.** Default unit: `MemoryMax=1G` (covers 8 workers on 24 MP photos). Small servers (≤4 cores, auto workers ≤ 4): `[worker] threads = 2` (or leave auto) with `MemoryMax=512M`. The two knobs trade off against each other — operators raise one or lower the other.
- **Failure safety.** Thumbnails are never left half-written (atomic rename); an OOM kill is recovered by `Restart=always`, and the startup scan re-queues any thumbnails lost to the kill. The service can never wedge in a state that corrupts the photo tree.
- **Videos** add FFmpeg child processes (memory included in the limits above); their count is bounded by the worker semaphore. Note: per-job timeouts are not yet implemented in code — FFmpeg extraction of a pathological input could hold a worker slot for a long time; this is a known gap to close (the worker cap and `TasksMax` contain the blast radius).

Sizing guide: see [Deployment](#9-deployment) and `DEPLOY.md` for the complete unit files and troubleshooting table.

### Performance Considerations (10,000+ images)

With a large collection, four measures are critical:

1. **Background thumbnail worker**: The service must never block startup or API requests on thumbnail generation. New and missing thumbnails are queued and processed asynchronously.
2. **Recursive inotify watch**: A single recursive watch on the album root avoids Linux `fs.inotify.max_user_watches` limits (default ~8,192).
3. **Image dimension cache**: Opening every image to read its width/height on every API call is prohibitively expensive. Dimensions are cached in SQLite (see [State Management](#state-management)).
4. **Bounded resource use**: Memory is bounded by the worker count and CPU by the systemd quota, independent of collection size or upload volume — see [Resource Protection & Limits](#resource-protection--limits).

---

## 6. Rust Service — API Specification

Base URL: `http://<bind_address>` (default `127.0.0.1:8080`; configurable in `album.toml`). Not exposed externally — Caddy reverse-proxies `/api/*`.

### Endpoints

#### `GET /api/album`

Query params:
- `path` (optional): relative path within the album root. Defaults to empty (top level).

Response `200 OK`:
```json
{
  "path": "1980-89/1981",
  "name": "1981",
  "breadcrumbs": [
    { "name": "Home", "path": "" },
    { "name": "1980-89", "path": "1980-89" },
    { "name": "1981", "path": "1980-89/1981" }
  ],
  "folders": [
    {
      "name": "summer holiday",
      "path": "1980-89/1981/summer holiday",
      "cover": "beach_thumb.jpg",
      "count_photos": 12,
      "count_albums": 0
    }
  ],
  "photos": [
    {
      "name": "wedding.jpg",
      "type": "image",
      "thumb": "thumbs/wedding_thumb.jpg",
      "width": 2048,
      "height": 1536
    },
    {
      "name": "party.mp4",
      "type": "video",
      "thumb": "thumbs/party_thumb.jpg",
      "width": 1920,
      "height": 1080,
      "duration": 124
    }
  ]
}
```

- `folders`: subdirectories that themselves contain photos or other folders, sorted by folder name.
- `photos`: direct media files in this folder (images and videos), sorted by filename.
  - `type`: `"image"` or `"video"`.
  - `duration`: present only for videos, integer seconds.
- `cover`: the chosen thumbnail for the folder. Falls back recursively: first photo in the folder itself, then the first photo in the first child folder, then the first photo in the first grandchild folder. If no thumbnail exists anywhere in the subtree, the frontend displays a static muted placeholder.

#### `POST /api/cover`

Headers:
- `X-Admin-Key: <admin_key>` — required. Must match the key in `album.toml`. Requests without a valid key receive `403 Forbidden`. Requests from foreign origins receive `403` (no CORS preflight allowed).

Body:
```json
{
  "image_path": "1980-89/1981/wedding.jpg",
  "targets": ["1980-89/1981", "1980-89", ""]
}
```

- `image_path`: relative path to the image within the album root.
- `targets`: array of folder paths to set this image as the cover for. An empty string `""` represents the album root. Each target must be a valid parent folder of the image (the backend validates this).

Response `204 No Content`.

**Error responses:**
- `400 Bad Request` — malformed JSON, invalid path characters, path escapes the album root, or a target is not a valid ancestor of the image.
- `403 Forbidden` — missing or incorrect admin key.
- `404 Not Found` — the image or a target folder does not exist.
- `500 Internal Server Error` — generic message; details are logged server-side only.

Sets the preferred cover image for one or more folders. Persisted in SQLite.

#### `GET /api/health`

Response `200 OK`: `{ "status": "ok" }`.

For systemd / monitoring health checks.

---

## 7. Frontend Specification

### Technology
- **HTML5**, **CSS3**, **Vanilla JavaScript (ES2020+)**.
- No build tools. No npm. No frameworks.

### Pages / Views

#### Album Grid View
- Displays folders and photos in a responsive CSS Grid.
- Each folder is shown as a card: thumbnail + folder name + counts (e.g. "44 photos", "1 album").
- Each photo or video is shown as a thumbnail.
- Video thumbnails display a small play-icon overlay (CSS or SVG) to distinguish them from still images.
- All thumbnail `<img>` elements use `loading="lazy"` to avoid fetching off-screen images.
- If a thumbnail does not yet exist (background worker backlog), a CSS placeholder is shown: a solid muted background colour with a subtle image icon. The full-size image is never used as a fallback — this would crush performance on large folders.
- Clicking a folder navigates deeper. Clicking a thumbnail opens the Photo/Video Viewer.
- Breadcrumb trail at the top (`Home / 1980-89 / 1981`).

#### Admin Mode & Cover Selection

The gallery is read-only for public visitors. Write operations (setting folder covers) require admin mode.

**Entering admin mode:**
- Visit `https://album.example.com/#admin=<key>` (the URL from first-run logs).
- The frontend reads the key from `window.location.hash`, stores it in `localStorage` as `album_admin_key`, and immediately removes it from the URL via `history.replaceState()`.
- A small indicator (e.g. a padlock icon) appears in the header to confirm admin mode is active.

**UI behaviour:**
- In admin mode, every thumbnail shows a small "Set as cover" icon on hover/focus.
- Clicking it opens a small modal / dropdown with checkboxes for the image's folder and every parent folder up the tree.

Example: For an image at `1980-89/1981/summer holiday/beach.jpg`, the checkbox list shows:
- [x] `summer holiday` (immediate parent)
- [x] `1981`
- [ ] `1980-89`
- [ ] `Home` (album root)

The user checks whichever folders should use this image as their cover, then confirms. The frontend sends a single `POST /api/cover` with all selected targets.

- If the key is missing or invalid, the backend returns `403` and the frontend shows a warning.
- Without admin mode, cover selection UI is completely hidden — visitors see a clean read-only gallery.

**On touch devices:**
- The "Set as cover" icon is always visible in admin mode. The checkbox modal is tap-friendly with large touch targets.

#### Photo/Video Viewer (overlay or dedicated view)
- **Images**: displayed scaled to fit the viewport using an `<img>` tag (`object-fit: contain`).
- **Videos**: displayed using the HTML5 `<video>` element with native browser controls (`controls` attribute). The video is scaled to fit the viewport. No external player library is required — all browsers from 2020 onwards support `<video>` with MP4/H.264.
- **Navigation** (works across both images and videos in the same folder):
  - **◀ Previous**: previous item in current folder's filename order.
  - **▶ Next**: next item in current folder's filename order.
  - **▲ Up**: return to the parent album grid.
- **Actions**:
  - **Download**: direct link to `/photoalbum/<path>/<file>`.
  - **Copy Link**: copies the direct media URL to clipboard.

### Colour Scheme (Fresh, Modern)
The frontend supports both light and dark modes via CSS custom properties and the `prefers-color-scheme` media query. A manual toggle is also provided in the UI header.

**Light mode (default):**
- Background: `#fafafa` (very light warm grey).
- Text: `#1a1a1a` (charcoal).
- Accents: muted teal `#0d9488` or ochre `#d97706` for icons, hover states, and the active breadcrumb.
- Cards: `#ffffff` with subtle shadow (`0 1px 3px rgba(0,0,0,0.1)`).

**Dark mode:**
- Background: `#0f172a` (slate 900).
- Text: `#e2e8f0` (slate 200).
- Accents: `#2dd4bf` (teal 400) or `#fbbf24` (amber 400).
- Cards: `#1e293b` (slate 800) with subtle border.

In both modes, let the photographs provide the colour. The dark background helps images with bright highlights pop.

---

## 8. State Management

### SQLite Schema

```sql
CREATE TABLE folder_covers (
    folder_path TEXT PRIMARY KEY,      -- relative path from album root
    image_name  TEXT NOT NULL,         -- filename of the chosen cover image
    updated_at  INTEGER NOT NULL       -- unix timestamp
);

CREATE INDEX idx_covers_path ON folder_covers(folder_path);

CREATE TABLE photo_metadata (
    photo_path TEXT PRIMARY KEY,       -- relative path from album root
    width      INTEGER,
    height     INTEGER,
    modified   INTEGER NOT NULL        -- source file mtime for cache invalidation
);

CREATE INDEX idx_photo_meta_path ON photo_metadata(photo_path);
```

**WAL mode**: The Rust service opens the database with `PRAGMA journal_mode = WAL;`. This allows concurrent reads from the API while the background thumbnail worker writes dimension cache updates, without lock contention.

### Caching Strategy

- **Folder listings**: Read directly from the filesystem on every `GET /api/album` request. `readdir` on a single folder is sub-millisecond on SSD, and this guarantees consistency without invalidation logic.
- **Image dimensions**: Read from the `photo_metadata` SQLite table. The background thumbnail worker populates this cache as it processes images. If a file's `mtime` has changed since the cached `modified` value, the worker re-reads dimensions and updates the row.
- **Thumbnails**: Served directly by Caddy as static files — no API involvement.
- **In-memory LRU**: Can be added later for folder listings if needed, but is not required for the expected scale.

---

## 9. Deployment

This section describes the Debian (systemd) deployment. Complete deployment guides for **macOS (launchd)** and **Windows (NSSM)** are provided in `DEPLOY.md`.

### Rust Service (Debian)

Packaged as a single binary `/usr/local/bin/album-service`.

Systemd unit: `/etc/systemd/system/album-service.service`

```ini
[Unit]
Description=Album Photo Watcher & API
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/album-service
Restart=always
RestartSec=5
User=album
Group=album
WorkingDirectory=/var/lib/album
Environment="SIMPLE_ALBUM_CONFIG=/etc/album/album.toml"

[Install]
WantedBy=multi-user.target
```

### Caddy Rate Limiting

To prevent API abuse, Caddy can rate-limit requests to `/api/*`. This protects against accidental or malicious load spikes without affecting static file serving.

```caddyfile
# Inside the album.example.com block
rate_limit {
    zone api_limit {
        key {remote_host}
        events 60
        window 1m
    }
}
```

This allows 60 API requests per minute per IP — generous for normal browsing, restrictive for abuse.

Commands:
```bash
sudo systemctl daemon-reload
sudo systemctl enable --now album-service
sudo journalctl -u album-service -f
```

### Caddy Configuration

```caddyfile
album.example.com {
    # Static frontend assets
    root * /var/www/album-static
    file_server

    # API reverse proxy (port must match album.toml server.bind)
    reverse_proxy /api/* localhost:8080

    # Photos and thumbnails (direct static serving)
    handle_path /photoalbum/* {
        root * /var/album
        file_server
    }

    # Security headers
    header {
        X-Content-Type-Options nosniff
        X-Frame-Options DENY
        Content-Security-Policy "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'"
        Referrer-Policy strict-origin-when-cross-origin
    }
}
```

**CSP note:** `style-src 'unsafe-inline'` is required because the frontend uses inline styles for dynamic positioning (e.g. the photo viewer). No external scripts are permitted.

**Note**: `/var/album` is the photo tree. `/var/www/album-static` contains `index.html`, `style.css`, `app.js`.

### Permissions

- Photo tree `/var/album`: owned by user `album`, group `album`, readable by Caddy (via `www-data` group membership or ACLs).
- SQLite DB `/var/lib/album/album.db`: writable by `album` user.

### Systemd Hardening

Additional directives in the service unit for sandboxing:

```ini
# Resource limits
# Memory peaks measured with the auto worker count (8): ~460 MB for
# 12 MP photos, ~790 MB for 24 MP photos. Lower `[worker] threads` in
# album.toml to reduce the requirement (2 workers: ~200 MB).
MemoryMax=1G
CPUQuota=80%
TasksMax=50

# Filesystem sandboxing
ReadWritePaths=/var/album /var/lib/album
ReadOnlyPaths=/usr/local/bin/album-service
ProtectSystem=strict
ProtectHome=true
```

---

## 10. Security Considerations

### Path Traversal

All path parameters (`path`, `folder_path`) are validated before filesystem access:
- Reject any component containing `..` before joining.
- Canonicalize the resolved path and verify it starts with the configured album root.
- Return `400 Bad Request` for any traversal attempt.

### XSS Prevention

The frontend is the primary XSS surface because folder names and filenames originate from the filesystem and are rendered into the DOM.

**Rules:**
- Use `textContent` for all user-controlled strings (folder names, filenames, breadcrumb labels). Never use `innerHTML`.
- Photo viewer captions and alt text must also use `textContent`.
- An XSS vulnerability would allow an attacker to exfiltrate the admin key from `localStorage` and modify covers.

### Admin Key Protection

- Delivered via URL fragment (`#admin=...`) so it never reaches server logs or `Referer` headers.
- Stripped from the URL immediately on page load.
- Stored in `localStorage` (same-origin only). If a strict threat model is needed later, this could be moved to a `Secure`, `HttpOnly` cookie, but that requires a login flow.
- Rotate the key by editing `album.toml` and restarting the service.

### CSRF & CORS

The API lives behind the same origin as the frontend via Caddy reverse proxy. **No CORS headers are set.** Foreign origins cannot make authenticated requests because:
1. The custom `X-Admin-Key` header triggers a preflight OPTIONS request.
2. The backend does not respond to OPTIONS from foreign origins.
3. The browser blocks the actual request.

For defence in depth, the backend also validates the `Origin` header on POST requests.

### Malicious File DoS

- There is currently no per-file size limit for thumbnail generation (a configurable limit, e.g. 50 MB, is planned but not yet implemented); very large files cost CPU and decode memory, contained by the worker cap and `CPUQuota`.
- Per-job timeouts are not implemented; the systemd `MemoryMax`, `CPUQuota`, and `TasksMax` directives contain the blast radius of a pathological file (see [Resource Protection & Limits](#resource-protection--limits)).
- The systemd `MemoryMax` and `CPUQuota` directives contain runaway resource consumption.
- The `image` crate is pure Rust and memory-safe.

### Error Handling

- API error responses contain only a generic message. Detailed errors (file paths, stack traces, SQLite errors) are logged server-side.
- The frontend displays user-friendly messages and falls back gracefully (e.g. broken thumbnail image shows a placeholder).

---

## 11. Technology Stack

| Layer | Technology |
|-------|------------|
| Edge / Static Server | Caddy 2 |
| Backend Language | Rust (Edition 2024) |
| Web Framework | Axum (see [Web Framework Choice](#web-framework-choice)) |
| File Watching | `notify` crate (auto-selects inotify / FSEvents / ReadDirectoryChangesW) |
| Image Processing | `image` crate (pure Rust), `kamadak-exif` (EXIF orientation), FFmpeg (system binary for video thumbnails) |
| Database | `rusqlite` (embedded SQLite) |
| Config | `toml` crate |
| Frontend | HTML5, CSS Grid/Flexbox, Vanilla JS (ES2020+) |

---

## 12. Web Framework Choice

The design specifies **Axum** as the web framework, but for an API this small (3 endpoints), the question of whether it is necessary is fair.

### Why Axum is a good choice
- **Correctness**: It handles HTTP/1.1 parsing, routing, header management, and error responses correctly. Writing this from scratch is error-prone.
- **Ecosystem**: Built on `hyper` and `tokio`, it is well-maintained, well-documented, and integrates cleanly with `tower` middleware.
- **Future-proofing**: If you later add search, authentication, or upload endpoints, Axum scales without rework.
- **Binary size**: Adds roughly 2–3 MB to the release binary. For a Debian server, this is negligible.

### Alternatives considered
- **Custom `std::net::TcpListener` server**: Possible in ~200 lines, but you would manually parse requests, handle chunked encoding, keep-alive, and routing. Not recommended — HTTP edge cases are subtle.
- **`rouille`**: A synchronous, minimal framework. It would work for this API, but it is less actively maintained than Axum and lacks async ecosystem support.
- **`actix-web`**: More feature-rich but heavier than Axum. Overkill for this project.

### Recommendation
Use **Axum**. The development velocity and correctness guarantees outweigh the minimal dependency cost. If you strongly prefer avoiding async, `rouille` is a viable fallback, but Axum is the pragmatic modern Rust choice.

---

## 13. Future Considerations

The following features are **not** part of the initial scope, but the architecture is designed so they can be added later without major rework:

1. **Full-text search**: Folder names and media filenames could be indexed in SQLite FTS5 and exposed via a new `GET /api/search?q=...` endpoint.
2. **Authentication**: The album is currently public. If needed, Caddy can enforce basic auth, or an authentication layer (e.g. OAuth2 proxy, session cookies) can be inserted in front of the API routes.

---

## 14. Success Criteria

- [ ] User can add/remove folders and photos to `/var/album` and see changes reflected in the web UI within seconds.
- [ ] Thumbnails are generated automatically and stored in per-folder `thumbs/` directories.
- [ ] Album grid shows folders with a representative thumbnail and counts.
- [ ] Admin can set/change the cover image for any folder via a pre-shared key; public visitors cannot.
- [ ] Photo viewer scales to viewport, supports Prev/Next/Up navigation, Download, and Copy Link.
- [ ] Zero frontend build step. Zero database server setup.
- [ ] Folders and photos are displayed in filename order, giving the user control via file naming.
- [ ] Dark mode is available and respects the user's system preference, with a manual toggle override.
- [ ] Service starts within seconds regardless of backlog; thumbnail generation happens in the background.
- [ ] Recursive inotify watch avoids `max_user_watches` exhaustion on Linux.
- [ ] Image dimensions are cached in SQLite; API does not open files to read metadata on every request.
- [ ] Common video formats (MP4, MOV, AVI, WebM, MKV) are supported with auto-generated thumbnails and native HTML5 playback.
- [ ] Slow or interrupted uploads never leave corrupt thumbnails: the stability pre-pass defers in-flight writes, mtime staleness self-heals any race, and the startup scan repairs legacy bad thumbnails on restart.
- [ ] Unlimited upload volumes are safe: memory is bounded by `[worker] threads` and CPU by the systemd quota, so large backlogs add latency without risking OOM kills or starving other services.
