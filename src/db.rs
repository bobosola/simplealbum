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
        conn.execute_batch(
            // Without a busy timeout, any lock held by another process (an
            // operator running `sqlite3` against the live database, a backup
            // tool, or a WAL checkpoint in progress) makes our next query fail
            // instantly with SQLITE_BUSY instead of waiting its turn.
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;",
        )?;
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

            CREATE TABLE IF NOT EXISTS photo_metadata (
                photo_path TEXT PRIMARY KEY,
                width      INTEGER,
                height     INTEGER,
                duration   INTEGER,
                modified   INTEGER NOT NULL
            );

            -- `folder_path` and `photo_path` are primary keys, so SQLite already
            -- maintains a unique index for each of them. The separate indexes an
            -- earlier build created were exact duplicates: extra disk, extra
            -- write work on every change, and no query could ever prefer them.
            DROP INDEX IF EXISTS idx_covers_path;
            DROP INDEX IF EXISTS idx_photo_meta_path;",
        )?;
        // `duration` was added after the first release. A table created by this
        // build already has it, in which case SQLite reports a duplicate column
        // error, which is expected and ignored.
        let _ = conn.execute("ALTER TABLE photo_metadata ADD COLUMN duration INTEGER", []);
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

    pub fn get_metadata(&self, photo_path: &str) -> Option<(u32, u32, Option<u64>, i64)> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT width, height, duration, modified FROM photo_metadata WHERE photo_path = ?1",
            params![photo_path],
            |row| Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, Option<u64>>(2)?,
                row.get::<_, i64>(3)?,
            )),
        ).optional().unwrap_or(None)
    }

    pub fn set_metadata(
        &self,
        photo_path: &str,
        width: u32,
        height: u32,
        duration: Option<u64>,
        modified: i64,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO photo_metadata (photo_path, width, height, duration, modified)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(photo_path) DO UPDATE SET
                width = excluded.width,
                height = excluded.height,
                duration = excluded.duration,
                modified = excluded.modified",
            params![photo_path, width, height, duration, modified],
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

    /// Delete metadata for everything *under* a folder path.
    ///
    /// Removing a folder from the album produces a single watcher event for the
    /// folder itself, so the rows for the media files that were inside it have
    /// to be purged by prefix or they are orphaned forever. `substr` is used
    /// instead of `LIKE` so that a folder name containing `%` or `_` cannot
    /// match unrelated rows.
    pub fn delete_metadata_under(&self, folder_path: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM photo_metadata
             WHERE substr(photo_path, 1, length(?1)) = ?1 || '/'",
            params![folder_path],
        )?;
        Ok(())
    }

    /// Delete folder covers that point at a file inside a removed folder.
    /// Same prefix-matching reasoning as [`Db::delete_metadata_under`].
    pub fn delete_covers_under(&self, folder_path: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM folder_covers
             WHERE substr(image_name, 1, length(?1)) = ?1 || '/'",
            params![folder_path],
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
