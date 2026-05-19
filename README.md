# Simple Album

A fast, self-hosted photo album web application. Drop your photos into a folder tree, and Simple Album automatically generates thumbnails and serves them through a clean web interface. No databases to configure, no build pipelines, no JavaScript frameworks — just a single Rust binary and a vanilla JS frontend.

- **Zero-configuration thumbnail generation** — images and videos are resized automatically on first access
- **Folder covers** — pick any photo as the thumbnail for its parent folder (admin mode)
- **Live filesystem watcher** — add or remove photos at any time; the grid updates automatically
- **Video support** — native HTML5 video player with automatic frame extraction for thumbnails
- **Dark mode** — automatic or manual toggle, persisted in localStorage
- **Keyboard navigation** — arrow keys and Escape in the image viewer
- **Browser history integration** — back/forward buttons work within the SPA
- **Single binary** — one compiled executable, no runtime dependencies beyond FFmpeg

---

## Supported Formats

| Type | Extensions |
|---|---|
| Images | `.jpg`, `.jpeg`, `.png`, `.webp` |
| Videos | `.mp4`, `.mov`, `.avi`, `.webm`, `.mkv` |

> **HEIC/HEIF (iPhone default)** is **not supported**. Before importing from an iPhone or iPad, convert to JPEG:
>
> - **iPhone/iPad**: Select photos → Share → Save to Files — the Files app automatically exports as JPEG.
> - **iPhone/iPad (permanent fix)**: Settings → Camera → Formats → Choose **"Most Compatible"** — future photos will be saved as JPEG.
> - **Mac**: Select photos in the Photos app → File → Export → Export Unmodified Originals (or use Image Capture, which exports JPEG by default).

---

## Quick Start

### Prerequisites

- [Rust](https://rustup.rs/) (to build from source)
- [FFmpeg](https://ffmpeg.org/download.html) — must be on your `PATH` for video thumbnails
- Any modern web browser
- Any reverse proxy or web server that supports reverse proxying (see Architecture below)

### Build

```bash
cargo build --release
```

The binary appears at `./target/release/album`.

### Create a config file

On first startup, Simple Album creates a default config if none exists. You can also create one manually:

```toml
[server]
bind = "127.0.0.1:18080"

[album]
root = "/path/to/your/photos"

[state]
db_path = "/path/to/album.db"

[admin]
key = ""
```

Leave `admin.key` empty — a secure key is auto-generated on first startup and printed to the logs.

### Run

```bash
SIMPLE_ALBUM_LOG=info ./target/release/album
```

Or point to a specific config:

```bash
SIMPLE_ALBUM_LOG=info SIMPLE_ALBUM_CONFIG=/path/to/album.toml ./target/release/album
```

### View in browser

The backend listens on `127.0.0.1:18080` by default. For a complete setup with TLS and static file serving, place a reverse proxy in front. See the Architecture section below.

**Admin mode**: The startup log prints an admin URL like:

```
Admin URL: https://your-domain.com/#admin=xxxxxxxxxxxx
```

Open that URL (or append `#admin=...` to any page). A star icon (⭐) appears in the header. Click any photo's star overlay to set it as a folder cover.

---

## Architecture

Simple Album is designed to work with **any** reverse proxy or web server:

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
3. Serve `/photos/*` from your album root directory

A sample `Caddyfile.local` is included for local development with self-signed TLS.

---

## Performance

On an Apple M4 Mac Mini, generating **8,200+ thumbnails from scratch** takes approximately **1 minute 40 seconds** (~86 images/second). Thumbnails are generated in the background on startup; the web UI is available immediately and populates progressively.

| Metric | Value |
|---|---|
| 8,192 image thumbnails | ~95 seconds |
| 15 video thumbnails (via FFmpeg) | ~6 seconds |
| Total from-scratch | ~1m 41s |

The worker pool limits concurrent jobs to your CPU's available parallelism (clamped to 2–8) to avoid RAM exhaustion.

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

## What's NOT in this repository

- **Test images** — the repo does not include any sample photos. Create your own `testdata/` folder or point the config at an existing photo collection.
- **Database files** — `album.db`, `album.db-wal`, and `album.db-shm` are generated at runtime. Do not commit them.
- **Compiled binary** — build with `cargo build --release`.

---

## Cross-Platform

The Rust backend compiles and runs on **Linux**, **macOS**, and **Windows** without source changes:

- **Linux**: inotify filesystem watcher
- **macOS**: FSEvents filesystem watcher
- **Windows**: ReadDirectoryChangesW filesystem watcher

See `DEPLOY.md` for per-platform service installation instructions.

---

## License

MIT
