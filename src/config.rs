use std::path::PathBuf;
use std::io::Write;
use rand::{rngs::OsRng, TryRngCore};
use base64::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub album: AlbumConfig,
    pub state: StateConfig,
    pub admin: AdminConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub bind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumConfig {
    pub root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateConfig {
    pub db_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminConfig {
    pub key: String,
}

impl Default for Config {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let album_root = home.join("album");
        let data_dir = dirs::data_dir().unwrap_or_else(|| home.join(".local/share"));
        let db_path = data_dir.join("album/album.db");
        Config {
            server: ServerConfig { bind: "127.0.0.1:8080".to_string() },
            album: AlbumConfig { root: album_root },
            state: StateConfig { db_path },
            admin: AdminConfig { key: String::new() },
        }
    }
}

pub fn load_or_create() -> anyhow::Result<(Config, PathBuf)> {
    let path = find_config_path()?;
    let config = if path.exists() {
        let content = std::fs::read_to_string(&path)?;
        let mut cfg: Config = toml::from_str(&content)?;
        if cfg.admin.key.is_empty() {
            cfg.admin.key = generate_key();
            save_config(&path, &cfg)?;
            info!("First run detected. Admin key generated and saved to {}", path.display());
            info!("Admin URL: https://album.example.com/#admin={}", cfg.admin.key);
        }
        cfg
    } else {
        let mut cfg = Config::default();
        cfg.admin.key = generate_key();
        std::fs::create_dir_all(path.parent().unwrap_or(PathBuf::from(".").as_path()))?;
        save_config(&path, &cfg)?;
        info!("First run detected. Created config at {}", path.display());
        info!("Admin URL: https://album.example.com/#admin={}", cfg.admin.key);
        cfg
    };
    Ok((config, path))
}

fn find_config_path() -> anyhow::Result<PathBuf> {
    if let Ok(env_path) = std::env::var("SIMPLE_ALBUM_CONFIG") {
        return Ok(PathBuf::from(env_path));
    }
    if let Some(config_dir) = dirs::config_dir() {
        let p = config_dir.join("album/album.toml");
        if p.exists() {
            return Ok(p);
        }
    }
    let etc = PathBuf::from("/etc/album/album.toml");
    if etc.exists() {
        return Ok(etc);
    }
    if let Some(config_dir) = dirs::config_dir() {
        return Ok(config_dir.join("album/album.toml"));
    }
    Ok(PathBuf::from("album.toml"))
}

fn save_config(path: &std::path::Path, config: &Config) -> anyhow::Result<()> {
    let content = toml::to_string_pretty(config)?;
    let mut file = std::fs::File::create(path)?;
    file.write_all(content.as_bytes())?;
    Ok(())
}

fn generate_key() -> String {
    let mut bytes = [0u8; 32];
    OsRng.try_fill_bytes(&mut bytes).expect("RNG failure");
    BASE64_URL_SAFE_NO_PAD.encode(&bytes)
}
