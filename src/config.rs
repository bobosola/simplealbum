//! Configuration loading.
//!
//! There are no defaults, no fallbacks, and no file generation. The process
//! reads exactly one file — the one named by the `SIMPLE_ALBUM_CONFIG`
//! environment variable — and refuses to start if that variable is unset or
//! if the file is missing, malformed, or incomplete. The application never
//! creates or rewrites its own configuration.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// Environment variable naming the config file. This is the only way the
/// config file is ever located.
pub const CONFIG_ENV_VAR: &str = "SIMPLE_ALBUM_CONFIG";

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub album: AlbumConfig,
    pub state: StateConfig,
    pub worker: WorkerConfig,
    pub admin: AdminConfig,
}

/// HTTP listener settings and the site's public identity.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    /// Socket address to bind, e.g. `"127.0.0.1:8080"`.
    pub bind: String,
    /// Externally visible base URL of this site, including any path prefix and
    /// a trailing slash, e.g. `"https://www.osola.org.uk/photos/"`.
    ///
    /// Used to build the absolute URLs that link-preview crawlers require.
    /// Crawlers never execute JavaScript and reject relative URLs, so this
    /// cannot be derived from the request at preview time.
    pub public_url: String,
    /// Human-readable album name, used as `og:site_name` and as the title of a
    /// share page for the album root.
    pub site_name: String,
}

/// The photo tree being served.
#[derive(Debug, Clone, Deserialize)]
pub struct AlbumConfig {
    /// Absolute path to the root of the photo tree. Must exist at startup.
    pub root: PathBuf,
}

/// Persistent state locations.
#[derive(Debug, Clone, Deserialize)]
pub struct StateConfig {
    /// SQLite database file. Missing parent directories are created.
    pub db_path: PathBuf,
}

/// Background thumbnail worker tuning.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkerConfig {
    /// Number of concurrent thumbnail jobs. `0` selects auto: the CPU core
    /// count clamped to `2..=8`. Explicit values are clamped to `1..=32`.
    ///
    /// This is the main memory lever: each job decodes a full-resolution
    /// image into RAM (a 24 MP photo decodes to ~72 MB), so the peak is
    /// roughly `threads` x that. Set a low value on small machines and keep
    /// systemd `MemoryMax` modest accordingly.
    pub threads: u32,
}

/// Admin authentication.
#[derive(Debug, Clone, Deserialize)]
pub struct AdminConfig {
    /// Shared secret required by mutating API endpoints. Must not be empty;
    /// the application never generates one for you.
    pub key: String,
}

impl Config {
    /// Read and validate the config file named by `SIMPLE_ALBUM_CONFIG`.
    ///
    /// Returns the parsed config and the path it came from, so the caller can
    /// log exactly which file is in effect.
    pub fn load() -> Result<(Config, PathBuf)> {
        let path = config_path()?;

        let contents = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "cannot read config file `{}` (from {})",
                path.display(),
                CONFIG_ENV_VAR
            )
        })?;

        let config: Config = toml::from_str(&contents)
            .with_context(|| format!("invalid config file `{}`", path.display()))?;

        config.validate()?;
        Ok((config, path))
    }

    /// Reject configurations that would otherwise fail confusingly later,
    /// such as a photo root that does not exist.
    fn validate(&self) -> Result<()> {
        if self.album.root.as_os_str().is_empty() {
            bail!("`album.root` is empty");
        }
        if !self.album.root.is_dir() {
            bail!(
                "`album.root` `{}` does not exist or is not a directory",
                self.album.root.display()
            );
        }
        if self.state.db_path.as_os_str().is_empty() {
            bail!("`state.db_path` is empty");
        }
        if self.server.bind.trim().is_empty() {
            bail!("`server.bind` is empty");
        }
        let public_url = &self.server.public_url;
        if !public_url.starts_with("http://") && !public_url.starts_with("https://") {
            bail!(
                "`server.public_url` `{}` must start with http:// or https://",
                public_url
            );
        }
        if !public_url.ends_with('/') {
            bail!(
                "`server.public_url` `{}` must end with a trailing slash",
                public_url
            );
        }
        if self.server.site_name.trim().is_empty() {
            bail!("`server.site_name` is empty");
        }
        if self.admin.key.trim().is_empty() {
            bail!(
                "`admin.key` is empty. Set a secure value in your album.toml \
                 (see README for how to generate one)"
            );
        }
        Ok(())
    }
}

/// Actionable hint appended to "path not usable" errors.
fn usage_hint() -> String {
    format!(
        "Point it at your album.toml, for example:\n    {}=/etc/album/album.toml album",
        CONFIG_ENV_VAR
    )
}

/// Resolve the config path from the environment.
///
/// The environment variable must be set, non-empty, and point at an existing
/// regular file. There are deliberately no fallback locations.
fn config_path() -> Result<PathBuf> {
    let raw = match std::env::var(CONFIG_ENV_VAR) {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => {
            bail!("{} is not set. {}", CONFIG_ENV_VAR, usage_hint())
        }
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("{} is set but is not valid UTF-8", CONFIG_ENV_VAR)
        }
    };

    if raw.trim().is_empty() {
        bail!("{} is set but empty. {}", CONFIG_ENV_VAR, usage_hint());
    }

    let path = PathBuf::from(raw);
    if !path.is_file() {
        bail!(
            "config file `{}` (from {}) does not exist or is not a regular file",
            path.display(),
            CONFIG_ENV_VAR
        );
    }
    Ok(path)
}
