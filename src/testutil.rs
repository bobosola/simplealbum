//! Shared helpers for tests that need a real directory tree or a config.
//!
//! Test-only, compiled for `cargo test` and never into the binary.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::config::{AdminConfig, AlbumConfig, Config, ServerConfig, StateConfig, WorkerConfig};

static SEQ: AtomicU32 = AtomicU32::new(0);

/// A unique temporary directory, removed on drop.
pub struct TempTree(pub PathBuf);

impl TempTree {
    pub fn new() -> Self {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("simplealbum-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        TempTree(dir)
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }

    /// Create a file, and any parent directories it needs.
    pub fn file(&self, rel: &str) {
        let path = self.path(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    pub fn dir(&self, rel: &str) {
        std::fs::create_dir_all(self.path(rel)).unwrap();
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A valid [`Config`] rooted at `root`, for exercising handlers directly.
pub fn config_for(root: &Path, db_path: &Path) -> Config {
    Config {
        server: ServerConfig {
            bind: "127.0.0.1:0".to_string(),
            public_url: "https://album.test/photos/".to_string(),
            site_name: "Test Album".to_string(),
        },
        album: AlbumConfig {
            root: root.to_path_buf(),
        },
        state: StateConfig {
            db_path: db_path.to_path_buf(),
        },
        worker: WorkerConfig { threads: 1 },
        admin: AdminConfig {
            key: "test-admin-key".to_string(),
        },
    }
}

/// The admin key [`config_for`] installs.
pub const TEST_ADMIN_KEY: &str = "test-admin-key";
