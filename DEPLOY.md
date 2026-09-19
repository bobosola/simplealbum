# Deployment Guide

This document covers deploying the Album photo service on **Linux (Debian/Ubuntu)**, **macOS**, and **Windows**.

The Rust binary is fully cross-platform. The `notify` crate automatically selects the correct filesystem watcher for each OS:
- **Linux**: inotify
- **macOS**: FSEvents
- **Windows**: ReadDirectoryChangesW

No conditional compilation or source changes are required.

---

## Prerequisites (All Platforms)

You need the compiled `album` binary and the frontend files:

```
album                    ← Rust binary (produced by cargo build --release)
static/index.html        ← Frontend
static/style-<version>.css
static/app-<version>.js
static/og-image.png      ← Link-preview image (see below)
```

**NB:** the CSS and JS files are currently named (and renamed after updates) for cache-busting purposes, e.g. `app-YYYY-MM-DD-HHMM.js` and `style-YYYY-MM-DD-HHMM.css` . Ensure that all references to these files are updated accordingly. Note that `index.html` itself is not versioned, so a browser holding a cached copy will keep requesting the previous asset names until it revalidates.

**Link previews:** `album.toml` must set `server.public_url` (the externally visible base URL, trailing slash included) and `server.site_name`. `public_url` is what builds the absolute URLs that `/api/share` returns, and crawlers reject relative ones. `static/index.html` additionally carries static Open Graph tags that hardcode the production origin, with `static/og-image.png` as the preview image, so both must be uploaded with the rest of `static/` or crawlers get a 404 and previews lose their picture. Note that the *service* only ever uses per-item thumbnails — it never references `og-image.png` — so that file is purely a frontend concern. Keep the hardcoded origin in `index.html` and `public_url` in step if the site ever moves. Crawlers cache previews aggressively — after changing any of this, re-scrape with the Facebook Sharing Debugger or by appending a throwaway query string.

**Upgrading an existing deployment:** see [Upgrading an Existing Deployment](#upgrading-an-existing-deployment) below for the full checklist. (If you are upgrading from a build older than the one that added `public_url` and `site_name`: they are required fields, so the service will refuse to start without them. Add them to `album.toml` **before** swapping in the new binary — serde ignores unknown fields, so an older binary reads the new config without complaint. That ordering avoids a failed start.)

### Customising the frontend for your deployment

Nothing in `app.js` is site-specific: it reads the album name from the `<h1>`, and its `/api` and `/photoalbum` prefixes are virtual paths that match the Caddy config. Everything you need to change lives in two files.

**`static/index.html`** — eight places:

| Line | What to change |
|---|---|
| `<title>` | Browser tab / fallback title |
| `<h1>` | The album name shown in the header |
| `return to main site` link | Points at `/index.html` by default |
| `og:site_name`, `og:title` | The album name |
| `og:image:alt` | The album name |
| `og:image`, `twitter:image` | **Absolute** URLs to your own preview image |
| `twitter:title` | The album name |
| `og:description`, `twitter:description` | Your own one-liner |

The `og:image` and `twitter:image` values must be absolute and must point at a real, publicly fetchable file. A 404 here is worse than omitting the tag, because crawlers cache the failure.

**`static/og-image.png`** — entirely optional, and entirely yours:

- 1200×630 (1.91:1) is the recommended size. Below 600×315 most platforms drop to a small thumbnail, and below 100×100 the image is discarded.
- Keep it under **600KB** — WhatsApp's limit. JPEG or PNG; if you switch to JPEG, update `og:image:type` in `index.html` to match.
- There is no requirement to generate one from scratch. Any photo cropped to 1200×630 works, and is arguably a better fit for a photo album than a designed card.
- If you would rather not have one at all, delete the file **and** the four `og:image` / `twitter:image` / `og:image:type` / `og:image:*` lines. The preview then shows text only.

**`album.toml`** — `server.public_url`, `server.site_name` (keep it matching the `<h1>`), `server.bind`, `album.root`, `state.db_path`.

Build from source (requires [Rust](https://rustup.rs)):

```bash
cd /path/to/album
cargo build --release
```

The binary appears at `target/release/album` (Linux/macOS) or `target\release\album.exe` (Windows).

---

# Copying from a dev server

If you have built and tested the application on a dev server, you can save time on the live server by copying over the photos, thumbnails, and SQLite files from dev. However, if dev and live are on different platorms then you will of course have to recompile the application binary for the dev platform's architecture. All the other files can be copied over with path changes made where appropriate  in the `album.toml` file.

---

# Upgrading an Existing Deployment

> **This section applies only to installations older than the release that
> changed the thumbnail encoder, the video poster-frame timestamp, EXIF
> dimension handling, the SQLite schema and the frontend.** A fresh installation
> from the current source needs none of it: the database schema and the frontend
> are already consistent with the binary.
>
> The test is whether the two halves of your deployed frontend agree. The
> versioned asset name in `static/` must be the same one named by the
> `<script src=...>` tag in the `static/index.html` you are serving. If they
> match, you are current and this section does not apply to you. If `index.html`
> names a script that is no longer present in `static/`, you are running a mixed
> pair, and either the steps below or a plain re-deploy of both files will fix it.
>
> No specific filename is named here on purpose. An earlier revision of this
> guide pinned the test to `app-2026-09-16-2109.js`, which stopped being a valid
> answer as soon as the asset was renamed again — a test that can only go stale.

That upgrade changes the thumbnail encoder, the video poster frame timestamp,
EXIF dimension handling, the SQLite schema and the frontend. **No change to
`album.toml` is needed** — no configuration field was added, removed or renamed.

The commands below are the systemd/Linux ones. On macOS use the `launchd`
equivalents and on Windows the `nssm` ones, both given further down this file.

### 1. Back up the database

Step 4 migrates the schema automatically on first start, and a migration is the
one step in this procedure that cannot be undone by putting the old binary back.
Take a copy first:

```bash
sudo systemctl stop album-service
sudo cp /var/lib/album/album.db /var/lib/album/album.db.bak
```

### 2. Build on the live server

The binary has to be compiled for the platform it runs on, so build it on the
live server (or cross-compile deliberately) rather than copying it from dev:

```bash
cd /path/to/simplealbum
git pull
cargo build --release
```

No crates were added by this upgrade, so this build needs no new downloads from
crates.io. Copy `Cargo.lock` along with the source, as always.

### 3. Upload the frontend

```
static/index.html                 changed: names the new script, header fix
static/app-<version>.js           the newly built script (see static/)
```

`style-*.css` and `og-image.png` are unchanged. Upload both files, then delete
the superseded `app-*.js` from the server. `index.html` is not
versioned, so a browser holding a cached copy will keep requesting the old script
until it revalidates — an ordinary reload is normally enough, and a hard refresh
is always enough. Check the network tab if the site still looks unchanged.

### 4. Swap the binary and restart

```bash
sudo install -m 755 target/release/album /usr/local/bin/album
sudo systemctl start album-service
sudo journalctl -u album-service -n 20 --no-pager
```

Startup runs the migrations: `ALTER TABLE photo_metadata ADD COLUMN duration`,
`ALTER TABLE photo_metadata ADD COLUMN probed`, and the removal of two indexes
that duplicated the primary keys. A clean start with no errors is all you should
see. Thumbnail files are used as they are.

Two things about `folder_covers` are worth knowing on the first start:

- Rows whose paths were stored un-normalised by an older build (a target like
  `"1980-89/"`, or an image like `"./1980-89/a.jpg"`) are rewritten once into
  their canonical form, because no lookup could ever have matched them. You will
  see `Repaired N stored folder cover(s) whose paths were not in canonical
  form`; `N` is normally `0`.
- Every *video* row is probed once more, because `probed` starts at `0` for rows
  that predate the column. That is what repairs durations recorded by builds
  that could not store them, and it is why a video whose container exposes no
  duration at all stops being re-probed forever.

### 5. Optional: correct metadata written by the old build

Two consequences of the old build are deliberately *not* repaired automatically,
because the repair path only refills rows that are missing or rows whose `probed`
flag is unset (which is what forces the one-time re-probe of every video row).
Both are cosmetic, and both are worth fixing only if they bother you:

**Portrait photos show swapped dimensions.** Rows written by the old build hold
the unrotated (landscape) width and height for photos whose EXIF orientation is
5–8. To correct every row without regenerating a single thumbnail:

```bash
sqlite3 /var/lib/album/album.db "DELETE FROM photo_metadata;"
sudo systemctl restart album-service
```

Every file is queued, but the thumbnails are still current, so the worker takes
the metadata-only branch and just reads each file header — no image is decoded
and no thumbnail is rewritten. Expect a few minutes for a few thousand photos.

**PNG thumbnails contain JPEG bytes.** They are named `.png` and hold JPEG data,
which is exactly what this upgrade fixes, but a thumbnail that exists and is
newer than its source is treated as current and left alone. Browsers sniff image
content, so these display correctly as they are. To re-encode them, delete the
`thumbs` folders under the album root and let the worker rebuild them (this one
does regenerate everything, bounded by `[worker] threads`):

```bash
find /var/album -type d -name thumbs -prune -exec rm -rf {} +
sudo systemctl restart album-service
```

### What the upgrade fixes, so you can confirm it worked

- Videos show a duration next to their dimensions (previously never displayed,
  because the value was hardcoded to zero), and a clip of less than half a second
  shows `0:01` rather than nothing.
- Portrait photos processed after the upgrade report their displayed dimensions,
  not the stored ones.
- Thumbnails of PNG and WebP sources are genuinely PNG and WebP.
- A folder moved or copied into the album gets its thumbnails within seconds,
  without a service restart.
- The service accepts API requests as soon as it starts, instead of waiting for
  the opening scan of the photo tree to finish.

# Creating from Scratch

## Part 1 — Linux (Debian / Ubuntu)

### 1.1 Install System Dependencies

```bash
sudo apt update
sudo apt install ffmpeg sqlite3
```

- **FFmpeg**: Required for extracting video thumbnails. The Rust binary shells out to `ffmpeg` and `ffprobe`.
- **SQLite**: Bundled inside the Rust binary via `rusqlite`, but the `sqlite3` CLI is useful for debugging.

### 1.2 Create Directories

```bash
sudo mkdir -p /var/album                # Your photo tree
sudo mkdir -p /var/lib/album            # SQLite database
sudo mkdir -p /var/www/album-static     # Frontend files
sudo mkdir -p /usr/local/bin            # Binary location
sudo mkdir -p /etc/album                # Config location
```

### 1.3 Copy Files

```bash
sudo cp target/release/album /usr/local/bin/album-service
sudo cp -r static/* /var/www/album-static/
sudo chmod +x /usr/local/bin/album-service
```

### 1.4 Create Config File

Create `/etc/album/album.toml`:

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


# Root of the photo tree
[album]
root = "/var/album"

# SQLite database location
[state]
db_path = "/var/lib/album/album.db"

# Thumbnail worker tuning
[worker]
# Concurrent thumbnail jobs. 0 = auto (CPU cores, clamped 2-8).
# Lower this on small servers to reduce peak memory (each job can hold a
# full decoded frame; a 24 MP JPEG measures ~100 MB while decoding, so
# 8 workers can peak at ~800 MB).
threads = 0

# Shared secret for admin (cover image) operations. Must not be empty; the
# service never generates one, so set your own secure value.
[admin]
key = "REPLACE-WITH-YOUR-OWN-KEY"
```

Generate a secure key locally and paste it here before deploying:
```bash
openssl rand -base64 32 | tr '+/' '-_' | tr -d '='
```

### 1.5 Create the `album` User

```bash
sudo useradd --system --no-create-home --home-dir /var/lib/album album
sudo chown -R album:album /var/album /var/lib/album
```

### 1.6 Systemd Service

Create `/etc/systemd/system/album-service.service`:

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
# Optional: the service defaults to `info`, which is what you want for finding
# the admin URL in the journal. Add this only to change verbosity.
#Environment="SIMPLE_ALBUM_LOG=info"

# Resource limits
# Memory: measured peaks with the default (auto) worker count:
#   12 MP photos (iPhone-class) x 8 workers: ~460 MB
#   24 MP photos (6000x4000)   x 8 workers: ~790 MB
# 1G covers both. If you lower [worker] threads in album.toml (e.g. 2),
# 512M is enough again. If you see OOM kills in `dmesg`, raise this or
# lower `threads` — they trade off against each other.
MemoryMax=1G
# CPU: the album service can never use more than 80% of one core in total,
# so it cannot starve other services (Caddy, etc.) — burst uploads just
# finish more slowly. Raise (e.g. 200%) if you want faster bursts.
CPUQuota=80%
# Tasks: service threads + ffmpeg child processes. Measured peak is ~33
# (video-heavy burst with 8 workers); 50 leaves comfortable headroom.
TasksMax=50

# Filesystem sandboxing
ReadWritePaths=/var/album /var/lib/album
ProtectSystem=strict
ProtectHome=true

[Install]
WantedBy=multi-user.target
```

> **Note on `MemoryMax`**: the thumbnail worker decodes each source image
> in full before resizing. Measured on a 14-core machine (auto worker
> count = 8): a burst of 12 MP iPhone-class JPEGs peaks at ~453 MB, and
> 24 MP photos (6000×4000) at ~785 MB — so the default unit uses 1G. The peak
> is not handed back to the OS afterwards, so the process keeps that RSS.
> On a small server, set `[worker] threads = 2` in `album.toml` and
> `MemoryMax=512M` is sufficient again. If you see OOM kills in
> `dmesg`, raise `MemoryMax` via `systemctl edit album-service` or lower
> `threads` — `Restart=always` recovers automatically, and the startup
> scan re-queues any thumbnails lost to the kill.

Enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now album-service
sudo journalctl -u album-service -f
```

Watch the journal for the admin URL built from the `admin.key` you set above.

### 1.7 Caddy Configuration

Create or edit `/etc/caddy/Caddyfile`:

```caddyfile
album.example.com {
    # Static frontend assets
    root * /var/www/album-static
    file_server

    # The CSS/JS filenames carry a build timestamp, so a one-year immutable
    # cache is safe: a changed file always arrives under a new name.
    @versioned path *.css *.js
    header @versioned Cache-Control "public, max-age=31536000, immutable"

    # index.html is NOT versioned, so it must never be reused without
    # revalidation: a browser holding a stale copy keeps asking for the asset
    # filenames that copy names, which the server has already deleted.
    # `no-cache` means "revalidate before use", not "do not store" — the ETag
    # makes it a cheap 304.
    @unversioned path / /index.html
    header @unversioned Cache-Control "no-cache"

    # API reverse proxy
    reverse_proxy /api/* localhost:8080

    # Photos and thumbnails
    handle_path /photoalbum/* {
        root * /var/album
        file_server
    }

    # Security headers (adjust or remove Content-Security-Policy if your frontend
    # loads scripts/styles from external CDNs or makes cross-origin fetch calls)
    header {
        X-Content-Type-Options nosniff
        X-Frame-Options DENY
        Content-Security-Policy "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'"
        Referrer-Policy strict-origin-when-cross-origin
    }
}
```

Reload Caddy:

```bash
sudo systemctl reload caddy
```

### 1.8 Verify

```bash
curl -s https://album.example.com/api/health
curl -s "https://album.example.com/api/album?path="
```

Add a photo to (say) `/var/album/1970-79/1970/` and refresh the page — the thumbnail should appear within seconds.

---

## Part 2 — macOS

### 2.1 Install System Dependencies

Using Homebrew (https://brew.sh):

```bash
brew install ffmpeg caddy
```

- **FFmpeg**: Required for video thumbnail extraction.
- **Caddy**: The edge web server. Optional — you can also use nginx or serve directly.

### 2.2 Create Directories

```bash
mkdir -p ~/album                # Your photo tree
mkdir -p ~/Library/Application\ Support/album   # Config
mkdir -p ~/Library/Application\ Support/album/db  # Database
mkdir -p ~/Sites/album-static   # Frontend files
mkdir -p /usr/local/bin         # Binary location
```

### 2.3 Copy Files

```bash
cp target/release/album /usr/local/bin/album-service
chmod +x /usr/local/bin/album-service
cp -r static/* ~/Sites/album-static/
```

### 2.4 Create Config File

Create `~/Library/Application Support/album/album.toml`:

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


# Root of the photo tree
[album]
root = "/Users/YOUR_USERNAME/album"

# Thumbnail worker tuning
[worker]
# Concurrent thumbnail jobs. 0 = auto (CPU cores, clamped 2-8).
# Lower this on small servers to reduce peak memory (each job can hold a
# full decoded frame; a 24 MP JPEG measures ~100 MB while decoding, so
# 8 workers can peak at ~800 MB).
threads = 0

# SQLite database location
[state]
db_path = "/Users/YOUR_USERNAME/Library/Application Support/album/db/album.db"

# Shared secret for admin (cover image) operations. Must not be empty; the
# service never generates one, so set your own secure value.
[admin]
key = "REPLACE-WITH-YOUR-OWN-KEY"
```

Replace `YOUR_USERNAME` with your actual macOS username. Generate a secure key with:
```bash
openssl rand -base64 32 | tr '+/' '-_' | tr -d '='
```

### 2.5 launchd Service

Create `~/Library/LaunchAgents/com.album.service.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.album.service</string>

    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/album-service</string>
    </array>

    <key>EnvironmentVariables</key>
    <dict>
        <key>SIMPLE_ALBUM_CONFIG</key>
        <string>/Users/YOUR_USERNAME/Library/Application Support/album/album.toml</string>
    </dict>

    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>

    <key>StandardOutPath</key>
    <string>/Users/YOUR_USERNAME/Library/Logs/album.log</string>
    <key>StandardErrorPath</key>
    <string>/Users/YOUR_USERNAME/Library/Logs/album.log</string>
</dict>
</plist>
```

Replace `YOUR_USERNAME` with your actual username. Then load and start:

```bash
launchctl load ~/Library/LaunchAgents/com.album.service.plist
launchctl start com.album.service
launchctl list | grep album
```

Check logs:

```bash
tail -f ~/Library/Logs/album.log
```

To stop:

```bash
launchctl stop com.album.service
launchctl unload ~/Library/LaunchAgents/com.album.service.plist
```

To run as a system daemon (for all users), move the plist to `/Library/LaunchDaemons/` and use `sudo`.

### 2.6 Caddy Configuration

If you installed Caddy via Homebrew, create a Caddyfile in your project directory:

```caddyfile
localhost {
    tls internal

    root * /Users/YOUR_USERNAME/Sites/album-static
    file_server

    @versioned path *.css *.js
    header @versioned Cache-Control "public, max-age=31536000, immutable"

    # index.html is NOT versioned, so it must never be reused without
    # revalidation: a browser holding a stale copy keeps asking for the asset
    # filenames that copy names, which the server has already deleted.
    # `no-cache` means "revalidate before use", not "do not store" — the ETag
    # makes it a cheap 304.
    @unversioned path / /index.html
    header @unversioned Cache-Control "no-cache"

    reverse_proxy /api/* localhost:8080

    handle_path /photoalbum/* {
        root * /Users/YOUR_USERNAME/album
        file_server
    }
}
```

Run:

```bash
caddy run --config /path/to/Caddyfile
```

Open `https://localhost:8443` in your browser. Accept the self-signed certificate warning.

### 2.7 Notes on macOS FSEvents

macOS uses **FSEvents** instead of inotify. FSEvents coalesces rapid changes and has a ~1 second delay before delivering events. This is normal macOS behaviour — thumbnails will appear slightly slower than on Linux, but the API remains responsive.

---

## Part 3 — Windows

### 3.1 Install System Dependencies

**FFmpeg**: Download from https://ffmpeg.org/download.html (Windows builds from gyan.dev or BtbN). Extract the ZIP and add the `bin` folder to your system PATH:

1. Download `ffmpeg-release-essentials.7z`
2. Extract to `C:\ffmpeg`
3. Add `C:\ffmpeg\bin` to your PATH via System Environment Variables
4. Open a **new** PowerShell/cmd window and verify:

```powershell
ffmpeg -version
ffprobe -version
```

**Caddy** (optional): Download from https://caddyserver.com/download and place `caddy.exe` in `C:\Windows` or add its directory to PATH.

### 3.2 Create Directories

In PowerShell or File Explorer:

```powershell
New-Item -ItemType Directory -Path "C:\album" -Force
New-Item -ItemType Directory -Path "$env:APPDATA\album" -Force
New-Item -ItemType Directory -Path "$env:APPDATA\album\db" -Force
New-Item -ItemType Directory -Path "C:\album-static" -Force
New-Item -ItemType Directory -Path "C:\album-service" -Force
```

### 3.3 Copy Files

```powershell
Copy-Item "target\release\album.exe" "C:\album-service\album-service.exe"
Copy-Item -Recurse "static\*" "C:\album-static\"
```

### 3.4 Create Config File

Create `$env:APPDATA\album\album.toml` ( resolves to `C:\Users\YOURNAME\AppData\Roaming\album\album.toml`):

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


# Root of the photo tree
[album]
root = "C:\\album"

# Thumbnail worker tuning
[worker]
# Concurrent thumbnail jobs. 0 = auto (CPU cores, clamped 2-8).
# Lower this on small servers to reduce peak memory (each job can hold a
# full decoded frame; a 24 MP JPEG measures ~100 MB while decoding, so
# 8 workers can peak at ~800 MB).
threads = 0

# SQLite database location
[state]
db_path = "C:\\Users\\YOURNAME\\AppData\\Roaming\\album\\db\\album.db"

# Shared secret for admin (cover image) operations. Must not be empty; the
# service never generates one, so set your own secure value.
[admin]
key = "REPLACE-WITH-YOUR-OWN-KEY"
```

Use **double backslashes** (`\\`) in TOML string values, or use forward slashes (`/`) which Rust also accepts on Windows. Generate a secure key with:
```powershell
# PowerShell
[Convert]::ToBase64String((1..32 | ForEach-Object { Get-Random -Maximum 256 }) -as [byte[]]) -replace '\+','-' -replace '/','_' -replace '=',''
```

### 3.5 NSSM Service Wrapper

Download NSSM from https://nssm.cc/download and extract `nssm.exe`.

In an **Administrator** PowerShell or Command Prompt:

```powershell
# Install the service
nssm install AlbumService "C:\album-service\album-service.exe"

# Set environment variable for config path
nssm set AlbumService AppEnvironmentExtra SIMPLE_ALBUM_CONFIG="C:\Users\YOURNAME\AppData\Roaming\album\album.toml"

# Set working directory
nssm set AlbumService AppDirectory "C:\album-service"

# Configure logging
nssm set AlbumService AppStdout "C:\album-service\album.log"
nssm set AlbumService AppStderr "C:\album-service\album.log"

# Start the service
nssm start AlbumService
```

Manage the service:

```powershell
nssm status AlbumService
nssm restart AlbumService
nssm stop AlbumService
nssm remove AlbumService confirm
```

Check logs:

```powershell
Get-Content "C:\album-service\album.log" -Wait -Tail 20
```

### 3.6 Native Windows Service (Alternative)

If you prefer a proper Windows Service without NSSM, add the `windows-service` crate to `Cargo.toml`:

```toml
windows-service = "0.8"
```

Then create a `#[cfg(windows)]` entry point that registers with the Service Control Manager. This requires code changes and is overkill for most deployments. NSSM is the recommended path.

### 3.7 Caddy Configuration

Create `C:\album-service\Caddyfile`:

```caddyfile
localhost:8443 {
    tls internal

    root * C:\album-static
    file_server

    @versioned path *.css *.js
    header @versioned Cache-Control "public, max-age=31536000, immutable"

    # index.html is NOT versioned, so it must never be reused without
    # revalidation: a browser holding a stale copy keeps asking for the asset
    # filenames that copy names, which the server has already deleted.
    # `no-cache` means "revalidate before use", not "do not store" — the ETag
    # makes it a cheap 304.
    @unversioned path / /index.html
    header @unversioned Cache-Control "no-cache"

    reverse_proxy /api/* localhost:8080

    handle_path /photoalbum/* {
        root * C:\album
        file_server
    }
}
```

Run (in an Administrator terminal):

```powershell
caddy run --config C:\album-service\Caddyfile
```

Open `https://localhost:8443` and accept the certificate warning.

### 3.8 Notes on Windows (untested)

- **File locking**: Windows locks files while they are being read. If you try to move a photo while the thumbnail worker has it open, the move may fail temporarily. The worker releases files quickly, so retrying usually succeeds.
- **PATH handling**: Make sure `ffmpeg.exe` and `ffprobe.exe` are on the system PATH, not just the user PATH, if running as a service under a different account.
- **Firewall**: Windows Defender may block incoming connections to port 8080. Allow it through Windows Firewall if accessing from other machines.
- **Long paths**: If your album path exceeds 260 characters, enable Windows long path support (requires registry change) or keep paths short.

---

## Admin Key Management

Set a secure admin key in `album.toml` **before** first startup. The service does not auto-generate one — it expects a pre-configured value so it never needs write access to the config directory at runtime.

Generate a key locally:
```bash
openssl rand -base64 32 | tr '+/' '-_' | tr -d '='
```

Paste the output into `album.toml` under `[admin] key = "..."`.

On startup the service logs the admin URL, built from your `public_url` setting:
```
Admin URL: https://album.example.com/#admin=xxxxxxxxxxxx
```

Bookmark this URL. The key is stored in your browser's `localStorage`. To revoke access:

1. Edit the config file and change `key` to a new value.
2. Restart the service.
3. All existing browser sessions lose admin access.

---

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| Thumbnails never generate | FFmpeg missing | Install FFmpeg and ensure it is on PATH |
| "Address already in use" | Port 8080 occupied | Change `bind` in `album.toml` |
| Photos appear sideways | Missing EXIF orientation | Already handled by `kamadak-exif` — ensure source images have EXIF |
| High CPU on startup | Large backlog | Normal — the background worker processes files asynchronously |
| Service restarts repeatedly / OOM-kill in `dmesg` | `MemoryMax` exceeded by concurrent image decodes (up to ~800 MB with 8 workers on 24 MP photos) | Raise `MemoryMax` (e.g. `1G`) or lower `[worker] threads` in `album.toml`; the startup scan re-queues any lost thumbnails |
| Grey or solid-colour thumbnails | Worker OOM-killed mid-generation, or rare `image` crate decode bug | Increase memory limit; if issue persists for specific files, pre-generate thumbs on another machine and copy them |
| Service won't start on macOS | launchd plist syntax error | Run `plutil -lint com.album.service.plist` |
| Service won't start on Windows | NSSM path error | Use full paths with double backslashes in NSSM |
