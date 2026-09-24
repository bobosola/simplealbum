# Simple Photo Album

This is a self-hosted cross-platform simple web photo album application. It's basically a web viewer for your existing image folders tree. It serves photos and videos from any folder tree on your web server. These can be organised and named however you like. The application supports many thousands of image or video files. 

It is very lean on resource use (see [Resource Requirements](#resource-requirements) for measurements). During normal operation its CPU use is effectively zero and its memory use is small — about 12 MB idle. Generating thumbnails is a short, CPU-bound burst rather than a heavy one: memory peaks at roughly 100 MB per concurrent worker, so it is bounded by the `[worker] threads` setting rather than by the number of photos and videos. In short, it is a low-resource application that suits a small web server.

It is also fast by architecture rather than by tuning: photos are plain files that the web server sends straight from the filesystem, so displaying one costs the application no measurable CPU — no database lookup, no re-encoding, no image bytes through the service — where an album that indexes its photos in a database and re-sizes them on demand pays a cost for every view. The viewer preloads each photo's neighbours, so stepping back and forth is normally a browser cache hit, and the only request that reaches the application while browsing is the folder listing, which is itself cached.

You will need the ability to:

- install `FFmpeg` on your web server — only if your album contains videos
  (images need no external tools)
- install the binary application and run it as a service
- edit your web server config file

The [`DEPLOY.md`](DEPLOY.md) file has full details.

Your files are served through a clean web interface ordered by the file and folder names as per the underlying folder tree. You can see a live example at [https://www.osola.org.uk/photos](https://www.osola.org.uk/photos) which has over 8,000 photos. 

It's a single Rust binary with a static front end consisting of:
- one HTML file
- one CSS file
- one vanilla JS file
- one PNG (`og-image.png`), the link-preview image for a link to the album's *own page* — the bare URL, or a `#path=...` URL copied out of the address bar. Links the app produces do not use it: the Share button, and its Copy button, both hand out an `/api/share` link, which carries the shared folder's or photo's own thumbnail as the preview image

The CSS and JS filenames are versioned (e.g. `style-YYYY-MM-DD-HHMM.css`) and renamed on every change. Serve them with a long-lived immutable cache header (`Cache-Control: public, max-age=31536000, immutable` — the sample Caddy configs in [`DEPLOY.md`](DEPLOY.md) and `Caddyfile.local` do this) and browsers can keep them for a year without ever going stale. The PNG is optional — replace it with any 1200x630 image, or delete it and the `og:image*` / `twitter:image` lines in `index.html` that reference it.

You can deploy these in the site root as a stand-alone photo album site or in a subfolder such as `/photos` as a part of another site. Edit the CSS and HTML to your taste, set `public_url` and `site_name` in `album.toml` (the latter should match your `<h1>`), and see [Customising the frontend for your deployment](DEPLOY.md#customising-the-frontend-for-your-deployment) for the full list of deployment-specific values. No build step or framework is required.

# Uploading photos & videos

There is deliberately no upload interface included (KISS principle). You can use any file manager which can connect to your remote server to copy over your folders and photos. For android devices, the free Total Commander app with its SFTP plugin works well. Apple devices may have similar apps.

I have included the script [`sync_photos.sh`](sync_photos.sh) which my LLM claims is cross-platform but I have only tested it from Mac to Debian (you may need to run `chmod +x sync_photos.sh` before first use). This uses `rsync` to automate the one-way upload process as much as possible. Just set your server and album folder details once in the script, then you can drag a single photo, multiple photos, or a folder directly into the terminal prompt or pass them via the command line thus:
```bash
./sync_photos.sh photo1.jpg photo2.jpg
```
or an entire folder, e.g.
```bash
./sync_photos.sh photos/special_day_out
```
You will then be prompted for:

- a destination folder (somewhere in the base album folder as set by you in the script)
- your SSH password or keyphrase

The script will create a new destination folder if needed, provided that its parent path exists. There are some perms options you can set in the script if you need them, but the defaults should be fine for the simple case of uploading to a web server.

Two practical notes:

- **Awkward filenames:** the drag-and-drop prompt re-parses what you paste as shell-style words — unescaping quotes and backslashes, but never executing anything — so a name containing an unescaped `'` or `"` can still confuse its quoting. Pass those as quoted arguments instead — `./sync_photos.sh "Bob's 50th.jpg"` — which bypasses the prompt entirely.
- **`rsync` version:** creating a missing remote parent directory uses `--mkpath`, which needs rsync 3.2.3 or newer. That is fine on Debian, but the `rsync` bundled with macOS is much older, so install a current one first (`brew install rsync`).


# Features

Here's what's included:

- **Read-only for your photos & videos** — your image and video files are not altered in any way
- **Album files sit outside your website's document root** — the web server maps the public
 `/photoalbum/` URL onto the real album directory (e.g. `/var/album`), which is not part of your
  site's file tree. The photos are public by design, but no album data lives among your website
  files, so removing the album is a single config-block change.
- **Automatic thumbnail generation** — image and video thumbnails are created and sized automatically on first detection in a `thumbs` folder within each image folder and deleted when the parent image is deleted
- **Simple admin mode to choose folder thumbnails** — optionally pick any photo as the thumbnail for its own folder or any ancestor of it (folder thumbnails otherwise default to the first image in the folder)
- **Live filesystem watcher service** —  the site updates automatically as you add or remove photos
- **Video support** — native HTML5 video player with automatic frame extraction for thumbnails
- **Dark mode** — persisted automatic or manual toggle
- **Sharing** — a standard share icon offers copy-link plus sharing a file or folder to common social media platforms
- **Keyboard & swipe navigation** — standard keyboard navigation in the image viewer, with swipe left and right for touch screens
- **Photo zoom** — pinch to zoom into a photo, drag to pan it, double-tap to toggle; zooming is handled by the viewer rather than the browser, so it still works in full screen (where Android blocks page pinch-zoom) and leaves the toolbar at its normal size
- **Full screen viewing** — the viewer has a full-screen button that hides the browser chrome, which matters most in landscape on a phone, where the status and URL bars otherwise take about a third of the screen; a phone opening a photo in landscape goes full screen automatically
- **Image pre-loading** — automatic next and previous image pre-loading to improve the user experience and avoid load lag which can otherwise occur, particularly on small screen devices
- **Browser history integration** — default browser back and forward actions work as expected
- **Single binary** — one compiled executable, and no runtime dependencies at all for a photo-only album (FFmpeg is used only to read and thumbnail videos)
- **SQLite backed state** — the cover photo choices and thumbnail metadata is held in a fully self-managed SQLite database (no user intervention, login, or maintenance is required)

# Non-Features

These features have been deliberately omitted (KISS principle again):

- No ability to rename or reorder your photos — rename them alphabetically if you want to change the display order or name
- No ability to allow different user perms — everyone can see all the photos
- No intermediate (space-consuming) range of thumbnail sizes — you get just the default ones
- No image editing features
- No installable app or PWA — this is a web site, not an app, and nothing ever asks the visitor to install it; phones use the viewer's own full screen button instead

# Known Limitations

- **Symlinked folders are listed but not traversed.** A folder that exists only as a symbolic link inside the album tree appears in the gallery but is not descended into, so it shows no photo counts and no cover. This is deliberate: following links would let a self-referential one (`album/loop → album`) walk forever. Symlinked individual photo files work normally.
- **Two media files with the same name stem in one folder share a thumbnail.** Videos are always thumbnailed as JPEG, so `clip.mp4` and `clip.mov` in the same folder both map to `clip_thumb.jpg` and will overwrite one another; the same applies to `photo.JPG` and `photo.jpg` on a case-sensitive filesystem. Keep stems unique within a folder.
- **Cover fallback looks three levels down.** A folder's cover is taken from the folder itself, an immediate child, or a grandchild. A folder whose only photos sit deeper than that shows a placeholder (an admin can still set one by hand).

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
- No tree in the database (SQLite only stores cover choices, image dimensions, and video durations)
- Each page load triggers exactly one `readdir` of the folder you're viewing, plus the count walk described above (and a thumbnail lookup for each subfolder's cover)
- The filesystem itself *is* the directory tree — the app reads it live on every request

This means the gallery is always consistent with the filesystem. If you add a folder on disk and refresh the page, the new images and folder appear immediately.

The web server handles URL path mapping (as put together by the processes described above) then retrieves and serves the requested image files, if they exist.

---

## Quick Start

### Prerequisites

- [Rust](https://rustup.rs/) (to build from source)
- [FFmpeg](https://ffmpeg.org/download.html) — must be on your `PATH` for video thumbnails; not needed at all if your album has no videos
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

Create `album.toml` yourself — the service never generates one, never writes back to it, and ships with no built-in paths or settings. Every setting below is required:

```toml
# API bind address and port
[server]
bind = "127.0.0.1:8080"

# Externally visible base URL of this site, including any path prefix and a
# trailing slash. Link-preview crawlers require absolute URLs, so this must be
# the URL people actually share.
public_url = "https://album.example.com/"

# Album name, shown as og:site_name and when sharing the album root.
site_name = "Photo Album"


# Root of the photo tree. Must exist at startup.
[album]
root = "/path/to/your/photos"

# SQLite database location
[state]
db_path = "/path/to/album.db"

# Thumbnail worker tuning
[worker]
# Concurrent thumbnail jobs. 0 = auto (CPU cores, clamped 2-8).
threads = 0

# Shared secret for admin (cover image) operations. Must not be empty; the
# service never generates one, so set your own secure value.
[admin]
key = "REPLACE-WITH-YOUR-OWN-KEY"
```

Set `admin.key` to a secure value before starting.

### Run

The env var `SIMPLE_ALBUM_CONFIG` is required — it names the config file to use. There is no search path and no default config, so the service exits with an explanatory error if it is unset:

```bash
SIMPLE_ALBUM_LOG=info SIMPLE_ALBUM_CONFIG=/path/to/album.toml ./target/release/album
```

### View in browser

The backend listens on the address configured in `server.bind` of `album.toml` (e.g. `127.0.0.1:8080`). For a complete setup with TLS and static file serving, place a reverse proxy in front. See the Architecture section below.

### Simple Admin mode for cover images: 

The startup log prints an admin URL like:

```
Admin URL: https://album.example.com/#admin=xxxxxxxxxxxx
```

The base of that URL is your `public_url` setting, and the key is the value set in `album.toml`. Open that URL (or append `/#admin=...` to any page) to enter Admin mode. A star icon (⭐) will appear in the header. Click any photo's star icon to set it as a folder cover image. 

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

1. Serve the files in `static/` — the HTML, the versioned CSS and JS, and `og-image.png` — wherever you deploy them (site root or a subfolder)
2. Proxy `/api/*` to the Rust backend
3. Serve `/photoalbum/*` from your album root directory

A sample `Caddyfile.local` is included for local development with self-signed TLS.

---

## Thumbnail Generation Performance

An Apple M4 Mac Mini generated **8,200+ thumbnails from scratch** in approximately **1 minute 40 seconds** (~86 images/second). Thumbnails are generated in the background on startup; the web UI is available immediately and populates progressively.

| Metric | Value |
|---|---|
| 8,192 image thumbnails | ~95 seconds |
| 15 video thumbnails (via FFmpeg) | ~6 seconds |
| Total from-scratch | ~1m 41s |

The worker pool limits concurrent jobs to your CPU's available parallelism (clamped to 2–8) to avoid RAM exhaustion.

---

## Resource Requirements

Measured against the release build on an Apple M4, with `[worker] threads = 2`
unless noted. A small VPS is slower per core, so read the CPU figures as "one
core per worker" rather than as absolute throughput.

| State | Memory (RSS) | CPU / latency |
|---|---|---|
| Idle, watching 421 folders / 8,000 files | **12 MB** | **0%** |
| Generating thumbnails, 2 workers | 49–249 MB, depending on image size | ~2 cores, saturated |
| Generating thumbnails, 8 workers | ~453 MB (12 MP), ~785 MB (24 MP) | all cores |
| 8,000 small images from scratch | 14 MB | 8.8 s at 2 workers |
| First folder listing after a change | — | 17 ms (full 8,000-file walk) |
| Later folder listings | — | <1 ms (cached) |

Peak memory is `threads × one decoded frame`, and **not** the size of the
library: 8,000 images peaked at 14 MB, while sixteen 24 MP photos peaked at
785 MB with 8 workers. One 24 MP photo occupies ~100 MB while it is decoded
(~55 MB at 12 MP). Freed memory is not handed back to the OS, so a peak becomes
the steady state — size `MemoryMax` for the peak, not for idle.

On a small server (1–2 cores, 1 GB) that is:

- **Small at rest.** ~12 MB and no measurable CPU; the filesystem watcher is
  not a load, even with hundreds of folders watched.
- **A medium, one-off burst while thumbnails are built.** CPU-bound at roughly
  0.1 core-seconds per 12–24 MP photo, so ~1,000 photos is ~2 minutes of one
  core. The web UI stays usable throughout and fills in progressively.
- **Sized by memory, not by disk, CPU or library size.** Set `[worker] threads`
  so that `threads × 100 MB` fits your `MemoryMax` — `threads = 2` with
  `MemoryMax=512M` on a 1 GB VPS, `threads = 4` with 1 GB. Serving photos costs
  the service nothing: they are static files served by the web server.

What a *visitor* costs is bandwidth rather than server CPU: a thumbnail is
~14 KB, while the viewer preloads the neighbouring originals — on a real album,
a sample of 17 photos had a median of 0.8 MB and a maximum of 4.4 MB, so a step
through the viewer fetches a few MB. That traffic goes through the web server,
not through this service.

---

## Data Storage

Simple Album uses an embedded **SQLite** database to cache photo dimensions and persist folder cover selections. SQLite was chosen over flat files (JSON, XML, etc.) because it provides indexed lookups, concurrent read/write access via WAL mode, and atomic updates. A separate database server or manual file-locking logic is not required.

**Backing up:** the cached photo dimensions and video durations are rebuilt automatically from the filesystem, so losing them costs nothing but a re-scan. Your **folder cover choices are not** — they exist only in this database, and nothing on disk can reconstruct them. Deleting `album.db` permanently loses every cover you have set by hand. If you have set any covers, back the database up, and copy `album.db` together with its `album.db-wal` and `album.db-shm` companions (or stop the service first), because recent writes may still be sitting in the write-ahead log.

## Logging

Simple Album logs to the terminal (stdout/stderr) only — there is no log file when run manually. When running as a system service (see [`DEPLOY.md`](DEPLOY.md)), stdout/stderr is captured as described below. 

Control verbosity with the `SIMPLE_ALBUM_LOG` environment variable. It defaults to `info`, so the startup log — including the admin URL — is visible without setting anything:

```bash
SIMPLE_ALBUM_LOG=debug SIMPLE_ALBUM_CONFIG=/path/to/album.toml ./target/release/album
```

Available levels: `trace`, `debug`, `info`, `warn`, `error`. An unparseable value is reported and `info` is used instead.

> **The startup log contains your admin key.** The admin URL printed at `info` level embeds the key, because that is how an operator finds it after installation without having to open `album.toml`. Treat the log as a secret: do not ship it to third-party log aggregation, attach it to a bug report, or include it in a support bundle unless you have rotated the key first. The key only authorises folder-cover changes; to rotate it, edit `admin.key` in `album.toml` and restart the service.

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
| `SIMPLE_ALBUM_CONFIG` | **Required.** Path to the config TOML file. |

`SIMPLE_ALBUM_CONFIG` is the only way the config file is located. The service does not create a config file, does not search for one, and does not fall back to built-in paths or settings. If the variable is unset, empty, or points at a missing file, startup fails with an error explaining what to set.

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

Simple Album was designed and directed by me (Bob Osola). It was built using the [`pi`](https://pi.dev) harness using Kimi 2.6, and updated with DeepSeek-V4.1-Flash. The documentation was originally written by the models but gently humanised by me.
