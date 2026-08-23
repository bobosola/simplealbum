# Deployment Guide

This document covers deploying the Album photo service on **Linux (Debian/Ubuntu)**, **macOS**, and **Windows**.

The Rust binary is fully cross-platform. The `notify` crate automatically selects the correct filesystem watcher for each OS:
- **Linux**: inotify
- **macOS**: FSEvents
- **Windows**: ReadDirectoryChangesW

No conditional compilation or source changes are required.

---

## Prerequisites (All Platforms)

You need the compiled `album` binary and the three frontend files:

```
album                ← Rust binary (produced by cargo build --release)
static/index.html    ← Frontend
static/style.css
static/app.js
```

**NB:** the CSS and JS files are currently named (and renamed after updates) for cache-busting purposes, e.g. `app-2026-08-23-1104.js` and `style-2026-08-23-1104.css` . Ensure that all references to these files are updated accordingly.

Build from source (requires [Rust](https://rustup.rs)):

```bash
cd /path/to/album
cargo build --release
```

The binary appears at `target/release/album` (Linux/macOS) or `target\release\album.exe` (Windows).

---

# Copying from a dev server

If you have built and tested the application on a dev server, you can save time on the live server by copying over the photos, thumbnails, and SQLite files from dev. However, if dev and live are on different platorms then you will of course have to recompile the application binary for the dev platform's architecture. All the other files can be copied over with path changes made where appropriate  in the `album.toml` file.

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

# Root of the photo tree
[album]
root = "/var/album"

# SQLite database location
[state]
db_path = "/var/lib/album/album.db"

# Change this to your own secure value before deploying.
# Leaving it empty causes the service to try to write back to this file on
# first startup, which will fail if the config directory is read-only.
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

# Resource limits
MemoryMax=512M
CPUQuota=80%
TasksMax=50

# Filesystem sandboxing
ReadWritePaths=/var/album /var/lib/album
ProtectSystem=strict
ProtectHome=true

> **Note on `MemoryMax`**: 512 MB is sufficient for typical collections. However, large images (e.g. 12 MP+ iPhone JPEGs at 4032×3024) decode to ~50–70 MB each in RAM. The thumbnail worker runs up to 8 concurrent jobs. If you see grey/corrupted thumbnails or OOM kills in `dmesg`, increase `MemoryMax` to `1G` or higher:
> ```bash
> sudo systemctl edit album-service
> ```
> Add `MemoryMax=1G` under `[Service]`, then `daemon-reload` and restart.

[Install]
WantedBy=multi-user.target
```

Enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now album-service
sudo journalctl -u album-service -f
```

Watch the journal for the generated admin key and URL.

### 1.7 Caddy Configuration

Create or edit `/etc/caddy/Caddyfile`:

```caddyfile
album.example.com {
    # Static frontend assets
    root * /var/www/album-static
    file_server

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

# Root of the photo tree
[album]
root = "/Users/YOUR_USERNAME/album"

# SQLite database location
[state]
db_path = "/Users/YOUR_USERNAME/Library/Application Support/album/db/album.db"

# Change this to your own secure value before deploying.
# Leaving it empty causes the service to try to write back to this file on
# first startup, which will fail if the config directory is read-only.
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

# Root of the photo tree
[album]
root = "C:\\album"

# SQLite database location
[state]
db_path = "C:\\Users\\YOURNAME\\AppData\\Roaming\\album\\db\\album.db"

# Change this to your own secure value before deploying.
# Leaving it empty causes the service to try to write back to this file on
# first startup, which will fail if the config directory is read-only.
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

On startup the service logs the admin URL:
```
Admin URL: https://your-domain.com/#admin=xxxxxxxxxxxx
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
| Service restarts repeatedly / OOM-kill in `dmesg` | `MemoryMax=512M` exceeded by large image decodes | Increase `MemoryMax` to `1G` via `systemctl edit album-service` |
| Grey or solid-colour thumbnails | Worker OOM-killed mid-generation, or rare `image` crate decode bug | Increase memory limit; if issue persists for specific files, pre-generate thumbs on another machine and copy them |
| Service won't start on macOS | launchd plist syntax error | Run `plutil -lint com.album.service.plist` |
| Service won't start on Windows | NSSM path error | Use full paths with double backslashes in NSSM |
