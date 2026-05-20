# macOS Local Development Testing Guide

This guide covers running the Album photo service locally on a Mac for development and testing. No Docker, no systemd, no production hardening — just the binary, a local config, and Caddy.

---

## Prerequisites

Install the following via Homebrew:

```bash
brew install ffmpeg caddy
```

- **FFmpeg**: Required for video thumbnail generation. The binary must be on your PATH.
- **Caddy**: The edge web server for TLS and reverse proxying.

Verify:

```bash
ffmpeg -version | head -1
caddy version
```

---

## 1. Build the Binary

From the project root:

```bash
cd /Users/bobosola/Sites/simplealbum
cargo build --release
```

The binary is produced at `./target/release/album`.

---

## 2. Create Test Photo Tree

Create some sample folders under your configured album root (`/Users/bobosola/photos`):

```
/Users/bobosola/photos/
├── 1970-79/
│   ├── 1970/
│   │   └── (sample images)
│   ├── 1971/
│   │   └── (sample images)
│   │   └── summer holiday/
│   │       └── (sample images)
├── 1980-89/
│   ├── 1981/
│   │   └── (sample images)
│   └── 1982/
│       └── (sample images)
```

Create the structure and copy in some of your own photos:

```bash
mkdir -p /Users/bobosola/photos/1970-79/1970
mkdir -p /Users/bobosola/photos/1970-79/1971/summer\ holiday
mkdir -p /Users/bobosola/photos/1980-89/1981
mkdir -p /Users/bobosola/photos/1980-89/1982

# Copy your own photos
# cp ~/Desktop/some_photos/*.jpg /Users/bobosola/photos/1970-79/1970/
```

You can add or remove photos at any time while the service is running.

---

## 3. Create Local Config File

A pre-made local config exists at `album.local.toml`:

```toml
[server]
bind = "127.0.0.1:18080"

[album]
root = "/Users/bobosola/photos"

[state]
db_path = "/Users/bobosola/Sites/simplealbum/album.db"

[admin]
key = ""
```

On first startup the service will generate an admin key and write it back into this file. If you want a completely fresh start, delete the database files first:

```bash
rm -f album.db album.db-shm album.db-wal
```

---

## 4. Start the Rust Service

### Terminal 1 — Album Service

```bash
cd /Users/bobosola/Sites/simplealbum
SIMPLE_ALBUM_LOG=info SIMPLE_ALBUM_CONFIG=/Users/bobosola/Sites/simplealbum/album.local.toml ./target/release/album
```

**What this does:**
- `SIMPLE_ALBUM_LOG=info` — Shows startup messages, file scan progress, and the generated admin key.
- `SIMPLE_ALBUM_CONFIG=...` — Forces the service to read your local test config instead of searching system paths.
- `./target/release/album` — Runs the compiled binary.

**Expected output:**

```
INFO album: Config loaded from /Users/bobosola/Sites/simplealbum/album.local.toml
INFO album: Album root: /Users/bobosola/photos
INFO album: API binding: 127.0.0.1:18080
INFO album: FFmpeg detected.
INFO album::db: Database opened with WAL mode: /Users/bobosola/Sites/simplealbum/album.db
INFO album: Starting initial scan...
INFO album: Initial scan queued. Starting watcher and API...
INFO album::watcher: Filesystem watcher started on /Users/bobosola/photos
INFO album: API server listening on 127.0.0.1:18080
INFO album: Admin URL: https://localhost:8443/#admin=xxxxxxxxxxxx
```

**Copy the Admin URL from the log** and bookmark it. That URL (with the `#admin=...` fragment) puts your browser into admin mode.

**To stop:** Press `Ctrl+C` in this terminal.

---

## 5. Start Caddy

### Terminal 2 — Caddy

```bash
cd /Users/bobosola/Sites/simplealbum
caddy run --config Caddyfile.local
```

The local Caddyfile (`Caddyfile.local`) is already in the repo:

```caddyfile
localhost:8443 {
    tls internal

    # Static frontend assets
    root * /Users/bobosola/Sites/simplealbum/static
    file_server

    # API reverse proxy
    reverse_proxy /api/* localhost:18080

    # Photos and thumbnails
    handle_path /photos/* {
        root * /Users/bobosola/photos
        file_server
    }
}
```

**What this does:**
- `tls internal` — Caddy generates a self-signed certificate automatically.
- Serves the frontend HTML/CSS/JS from `./static/`.
- Proxies `/api/*` to the Rust service on `localhost:18080`.
- Serves photos and thumbnails directly from `/Users/bobosola/photos`.

**To stop:** Press `Ctrl+C` in this terminal.

---

## 6. Open in Browser

Navigate to:

```
https://localhost:8443
```

Your browser will show a certificate warning because Caddy uses its own internal CA. Accept the warning (or trust the CA permanently — see below).

### Enter admin mode

Paste the admin URL from the service logs:

```
https://localhost:8443/#admin=xxxxxxxxxxxx
```

A padlock icon appears in the header. You can now set folder covers.

---

## 7. Test the Filesystem Watcher

With both services running:

1. Copy a new image into `/Users/bobosola/photos/1980-89/1981/`:
   ```bash
   cp ~/Desktop/some_photo.jpg /Users/bobosola/photos/1980-89/1981/
   ```
2. Wait 1–3 seconds (macOS FSEvents has a slight delay).
3. Refresh the browser page.
4. The new photo appears automatically. A thumbnail is generated in `/Users/bobosola/photos/1980-89/1981/thumbs/`.

Delete a file and refresh — it disappears from the grid.

---

## 8. Stop and Restart

### Stop both services

- **Terminal 1** (album service): `Ctrl+C`
- **Terminal 2** (Caddy): `Ctrl+C`

### Restart cleanly

1. Delete old thumbnails if you want a completely fresh state:
   ```bash
   find /Users/bobosola/photos -type d -name thumbs -exec rm -rf {} + 2>/dev/null
   rm -f album.db album.db-shm album.db-wal
   ```

2. Start both services again in separate terminals (steps 4 and 5).

---

## 9. Trust Caddy's Internal CA (Optional)

If you are tired of the certificate warning, the easiest way is to let Caddy install its root CA automatically:

```bash
caddy trust
```

Enter your sudo password when prompted. After this, `https://localhost:8443` opens without warnings.

If `caddy trust` is unavailable, install the certificate manually:

```bash
sudo security add-trusted-cert -d -r trustRoot \
  -k /Library/Keychains/System.keychain \
  "$HOME/Library/Application Support/Caddy/pki/authorities/local/root.crt"
```

Or open it in Keychain Access:

```bash
open "$HOME/Library/Application Support/Caddy/pki/authorities/local/root.crt"
```

Then double-click the certificate, expand **Trust**, and set **"When using this certificate"** to **"Always Trust"**.

---

## 10. Quick Reference

| Task | Command |
|------|---------|
| Build | `cargo build --release` |
| Start album service | `SIMPLE_ALBUM_LOG=info SIMPLE_ALBUM_CONFIG=album.local.toml ./target/release/album` |
| Start Caddy | `caddy run --config Caddyfile.local` |
| Stop either service | `Ctrl+C` in its terminal |
| Fresh database | `rm -f album.db album.db-shm album.db-wal` |
| Fresh thumbnails | `find /Users/bobosola/photos -type d -name thumbs -exec rm -rf {} +` |
| View logs | Service logs to stdout (because of `SIMPLE_ALBUM_LOG=info`) |
| Caddy logs | Also to stdout in its terminal |

---

## macOS-Specific Notes

- **FSEvents delay**: macOS uses FSEvents instead of inotify. File changes are delivered with a ~1 second coalescing delay. This is normal — not a bug.
- **Port 8080 conflicts**: The local config uses `18080` to avoid collisions with other services.
- **No launchd**: This guide runs the binary directly in a terminal. For production-style background running on macOS, see `DEPLOY.md` Part 2 (launchd).
- **"Address already in use"**: If the service fails to start with this error, a previous instance is still holding port 18080. Kill it:
  ```bash
  lsof -ti:18080 | xargs kill -9 2>/dev/null || echo "Port is free"
  ```
  Or kill all album processes: `pkill -9 album`
