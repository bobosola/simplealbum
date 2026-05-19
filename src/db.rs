use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::Mutex;
use tracing::info;

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        let db = Db { conn: Mutex::new(conn) };
        db.init()?;
        info!("Database opened with WAL mode: {}", path.display());
        Ok(db)
    }

    fn init(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS folder_covers (
                folder_path TEXT PRIMARY KEY,
                image_name  TEXT NOT NULL,
                updated_at  INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_covers_path ON folder_covers(folder_path);

            CREATE TABLE IF NOT EXISTS photo_metadata (
                photo_path TEXT PRIMARY KEY,
                width      INTEGER,
                height     INTEGER,
                modified   INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_photo_meta_path ON photo_metadata(photo_path);
            "
        )?;
        Ok(())
    }

    pub fn get_cover(&self, folder_path: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT image_name FROM folder_covers WHERE folder_path = ?1",
            params![folder_path],
            |row| row.get(0),
        ).optional().unwrap_or(None)
    }

    pub fn set_cover(&self, folder_path: &str, image_name: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        conn.execute(
            "INSERT INTO folder_covers (folder_path, image_name, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(folder_path) DO UPDATE SET
                image_name = excluded.image_name,
                updated_at = excluded.updated_at",
            params![folder_path, image_name, now],
        )?;
        Ok(())
    }

    pub fn get_metadata(&self, photo_path: &str) -> Option<(u32, u32, i64)> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT width, height, modified FROM photo_metadata WHERE photo_path = ?1",
            params![photo_path],
            |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?, row.get::<_, i64>(2)?)),
        ).optional().unwrap_or(None)
    }

    pub fn set_metadata(&self, photo_path: &str, width: u32, height: u32, modified: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO photo_metadata (photo_path, width, height, modified)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(photo_path) DO UPDATE SET
                width = excluded.width,
                height = excluded.height,
                modified = excluded.modified",
            params![photo_path, width, height, modified],
        )?;
        Ok(())
    }

    pub fn delete_metadata(&self, photo_path: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM photo_metadata WHERE photo_path = ?1",
            params![photo_path],
        )?;
        Ok(())
    }

    pub fn delete_cover(&self, folder_path: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM folder_covers WHERE folder_path = ?1",
            params![folder_path],
        )?;
        Ok(())
    }

    /// Delete a folder cover only if it references the given image name.
    /// Used when a photo is deleted to clean up its parent folder's cover.
    pub fn delete_cover_if_matches(&self, folder_path: &str, image_name: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM folder_covers WHERE folder_path = ?1 AND image_name = ?2",
            params![folder_path, image_name],
        )?;
        Ok(())
    }
}
