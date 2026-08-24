# Simple Photo Album

This is a self-hosted cross-platform simple web photo album application. It's basically a web viewer for your existing image folders. It serves photos and videos from any folder tree on your server. These can be organised and named however you like. The application supports many thousands of image or video files. 

Your files are served through a clean web interface ordered by the file and folder names as per the underlying folder tree. You can see a live example at [https://www.osola.org.uk/photos](https://www.osola.org.uk/photos) which has over 8,000 photos. 

It's a single Rust binary with a static front end consisting of:
- one HTML file
- one CSS file
- one vanilla JS file

You can deploy these in the site root as a stand-alone photo album site or in a subfolder such as `/photos` as a part of another site. Just edit the CSS and HTML files to your taste. No build step or framework is required.

# Features

Here's what's included:

- **Read-only for your photos & videos** — your image and video files are not altered in any way
- **Automatic thumbnail generation** — image and video thumbnails are created and sized automatically on first detection in a `thumbs` folder within each image folder and deleted when the parent image is deleted
- **Simple admin mode to choose folder thumbnails** — pick any photo as the thumbnail for its parent or grandparent folder
- **Live filesystem watcher service** —  the site updates automatically as you add or remove photos
- **Video support** — native HTML5 video player with automatic frame extraction for thumbnails
- **Dark mode** — persisted automatic or manual toggle
- **Sharing** — buttons for image URL copy and download are on the image view page
- **Keyboard & swipe navigation** — standard keyboard navigation in the image viewer, with swipe left and right for touch screens
- **Image pre-loading** — automatic next and previous image pre-loading to improve the user experience and avoid load lag which can otherwise occur, particularly on small screen devices
- **Browser history integration** — default browser back and forward actions work as expected
- **Single binary** — one compiled executable, no runtime dependencies beyond FFmpeg
- **SQLite backed state** — the cover photo choices and thumbnail metadata is held in a fully self-managed SQLite database (no user intervention, login, or maintenance is required)

# Non-Features

These features have been deliberately omitted. Use your favourite LLM to add them if you need them.

- No ability to rename or reorder your photos — rename them alphabetically if you want to change the display order or name
- No ability to allow different user perms — everyone can see all the photos
- No intermediate (space-consuming) range of thumbnail sizes — you get just the default ones
- No upload interface as these are generally slow and ponderous to use for large numbers of files — just use SFTP with [Filezilla](https://filezilla-project.org) or even good old `scp` to quickly upload your photos to your server
- No image editing features

---

## Supported Formats

| Type | Extensions |
|---|---|
| Images | `.jpg`, `.jpeg`, `.png`, `.webp` |
| Videos | `.mp4`, `.mov`, `.avi`, `.webm`, `.mkv` |

> **HEIC/HEIF (iPhone default)** is **not supported**. Before importing from an iPhone or iPad, convert to JPEG:
>
> - **iPhone/iPad**: Select photos → Share → Save to Files — the Files app automatically exports as JPEG.
> - **Mac**: Select photos in the Photos app → File → Export → Export Unmodified Originals

---

## How it works

**On startup (one-time background scan):**
`scan_existing()` in `worker.rs` walks the entire photo tree recursively to find images that need thumbnails generated. But this is just a flat queue of jobs — it doesn't build a tree data structure, and it doesn't persist any directory hierarchy. Once the initial scan finishes, it's done. 

**At runtime**, the watcher detects new files within seconds (FSEvents has ~1 second coalescing delay on macOS; inotify is near-instant on Linux). The thumbnail appears automatically after the worker finishes.

**On every API call (`GET /api/album`):**
`get_album()` in `api.rs` reads **only the single folder** being requested. It doesn't recurse. It just lists the immediate children of (say) `/var/album/2020-29/2026` or whatever path you asked for, checks which are folders vs media files, and returns them.

**The one bit of recursion that does happen:**
For each subfolder shown in the grid, the API walks each subfolder to count totals for the badge text (e.g. "12 photos, 1 album"). But it doesn't build a tree; it just counts and returns numbers.

**In short:**
- No persistent directory tree in memory
- No tree in the database (SQLite only stores cover choices and image dimensions)
- Each page load triggers exactly one `readdir` on the folder you're viewing
- The filesystem itself *is* the directory tree — the app reads it live on every request

This is by design. It means the gallery is always consistent with the filesystem. Add a folder on disk, refresh the page, it appears immediately. No sync step, no cache invalidation.

The web server handles URL path mapping (as put together by the processes described above) then retrieves and serves the requested image files, if they exist.

---

## Quick Start

### Prerequisites

- [Rust](https://rustup.rs/) (to build from source)
- [FFmpeg](https://ffmpeg.org/download.html) — must be on your `PATH` for video thumbnails
- Any modern web browser
- Any web server that supports reverse proxying (see Architecture below)
- The ability to set up the binary as a service application on your server (described in detail in the Deploy docs).

### Build

```bash
git clone https://github.com/bobosola/simplealbum
cd simplealbum
cargo build --release
```

The binary appears at `./target/release/album`.

### Create a config file

On first startup, Simple Album creates a default config if none exists. You may need to create one manually on your live server depending on write perms:

```toml
# API bind address and port
[server]
bind = "127.0.0.1:8080"

# Root of the photo tree
[album]
root = "/path/to/your/photos"

# SQLite database location
[state]
db_path = "/path/to/album.db"

# Change this to your own secure value before deploying.
# Leaving it empty causes the service to try to write back to this file on
# first startup, which will fail if the config directory is read-only.
[admin]
key = "REPLACE-WITH-YOUR-OWN-KEY"
```

Set `admin.key` to a secure value before starting — the service reads it from the config and does not write back to the file.

### Run

For local testing and debugging, set the logging level environment variable and run the app in one command thus:

```bash
SIMPLE_ALBUM_LOG=info ./target/release/album
```

Or optionally point to a specific config file:

```bash
SIMPLE_ALBUM_LOG=info SIMPLE_ALBUM_CONFIG=/path/to/album.toml ./target/release/album
```

### View in browser

The backend listens on `127.0.0.1:8080` by default. For a complete setup with TLS and static file serving, place a reverse proxy in front. See the Architecture section below.

### Simple Admin mode for cover images: 

The startup log prints an admin URL like:

```
Admin URL: https://your-domain.com/#admin=xxxxxxxxxxxx
```

where the key value is the value set in the `album.toml` file. Open that URL (or append `/#admin=...` to any page) to enter Admin mode. A star icon (⭐) will appear in the header. Click any photo's star icon to set it as a folder cover image. 

For simplicity, the admin mode uses a hash-prefixed path rather than a GET string parameter or admin password login. The "path-with-hash" approach is a [URI fragment](https://developer.mozilla.org/en-US/docs/Web/URI/Reference/Fragment) which ensures that the key does not leave the browser and prevents it from being sent to a server, or stored externally, such as in server logs.

---

## Architecture

Simple Album is designed to work with any reverse proxy or web server:

```
┌─────────┐     ┌─────────────────────────────┐     ┌──────────────────┐
│ Browser │────▶│  Caddy / Nginx / Apache / … │────▶│ Rust Album API   │
└─────────┘     │  • TLS termination          │     │ (localhost:8080) │
                │  • Static files (index.html)│     └──────────────────┘
                │  • Photo/thumbnail serving  │              │
                │  • /api/* reverse proxy     │     ┌────────┴──────────┐
                └─────────────────────────────┘     │ SQLite + watcher  │
                                                    │ + thumb worker    │
                                                    └───────────────────┘
```

**Caddy is not a prerequisite.** It is used in the example configs because it handles TLS and reverse proxying with minimal configuration, but you can substitute **Nginx, Apache, Traefik, or any other proxy** that supports `reverse_proxy`/`proxy_pass` semantics. The only requirements from the proxy are:

1. Serve `static/index.html`, `static/style.css`, and `static/app.js` at `/`
2. Proxy `/api/*` to the Rust backend
3. Serve `/photoalbum/*` from your album root directory

A sample `Caddyfile.local` is included for local development with self-signed TLS.

---

## Thumbnail Generation Performance

An Apple M4 Mac Mini  generated **8,200+ thumbnails from scratch** in approximately **1 minute 40 seconds** (~86 images/second). Thumbnails are generated in the background on startup; the web UI is available immediately and populates progressively.

| Metric | Value |
|---|---|
| 8,192 image thumbnails | ~95 seconds |
| 15 video thumbnails (via FFmpeg) | ~6 seconds |
| Total from-scratch | ~1m 41s |

The worker pool limits concurrent jobs to your CPU's available parallelism (clamped to 2–8) to avoid RAM exhaustion.

---

## Data Storage

Simple Album uses an embedded **SQLite** database to cache photo dimensions and persist folder cover selections. SQLite was chosen over flat files (JSON, XML, etc.) because it provides indexed lookups, concurrent read/write access via WAL mode, and atomic updates. A separate database server or manual file-locking logic is not required.

## Logging

Simple Album logs to the terminal (stdout/stderr) only — there is no log file when run manually. When running as a system service (see [`DEPLOY.md`](DEPLOY.md)), stdout/stderr is captured as described below. 

Control verbosity with the `SIMPLE_ALBUM_LOG` environment variable:

```bash
SIMPLE_ALBUM_LOG=debug ./target/release/album
```

Available levels: `trace`, `debug`, `info` (default), `warn`, `error`.

When running as a system service (see [`DEPLOY.md`](DEPLOY.md)) you can view the log as follows:

- **Linux (systemd)**: Use `journalctl` to read the log and find the admin URL:
  ```bash
  sudo journalctl -u album-service -f          # follow live output
  sudo journalctl -u album-service --no-pager -n 50  # last 50 lines
  ```
- **macOS (launchd)**: Check `~/Library/Logs/album.log`
- **Windows (NSSM)**: Check `C:\album-service\album.log`

---

## Configuration

| Environment Variable | Purpose |
|---|---|
| `SIMPLE_ALBUM_LOG` | Logging level (`info`, `warn`, `debug`, `trace`) |
| `SIMPLE_ALBUM_CONFIG` | Path to config TOML file (overrides default search) |

Config search order (if `SIMPLE_ALBUM_CONFIG` is not set):
1. `dirs::config_dir()/album/album.toml` (platform-specific)
2. `/etc/album/album.toml` (Linux fallback)

---

## Documentation

| File | Contents |
|---|---|
| [`DESIGN.md`](DESIGN.md) | Architecture, API specification, data model, security model |
| [`DEPLOY.md`](DEPLOY.md) | Production deployment on Linux (systemd), macOS (launchd), and Windows (NSSM) |
| [`DEPLOY_MAC_DEV.md`](DEPLOY_MAC_DEV.md) | Local development on macOS with Caddy |

---

## Not included in the repo

- **Test images** — the repo does not include any sample photos. Create your own `testdata/` folder or point the config at an existing photo collection.
- **SQLite database files** — `album.db`, `album.db-wal`, and `album.db-shm` are generated at runtime. Do not commit them.
- **Compiled binary** — build with `cargo build --release`.

---

## Cross-Platform

The Rust backend compiles and runs on **Linux**, **macOS**, and **Windows** without source changes:

- **Linux**: inotify filesystem watcher
- **macOS**: FSEvents filesystem watcher
- **Windows**: ReadDirectoryChangesW filesystem watcher (untested, so YMMV)

See `DEPLOY.md` for per-platform service installation instructions.

---

## License

MIT

## Acknowledgements

Simple Album was designed and directed by me (Bob Osola) and built by the Kimi 2.6 coding agent. The documentation was originally written by Kimi but humanised by me.
