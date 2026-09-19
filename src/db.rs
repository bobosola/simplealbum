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
        let db = Db::from_connection(conn)?;
        info!("Database opened with WAL mode: {}", path.display());
        Ok(db)
    }

    /// An in-memory database, for tests only. It runs the same schema setup as
    /// [`Db::open`], so the prefix-delete SQL is exercised for real.
    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Self> {
        Db::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> anyhow::Result<Self> {
        let db = Db { conn: Mutex::new(conn) };
        db.init()?;
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
    ///
    /// The length is `length(?1) + 1` because the right-hand side includes the
    /// separator: comparing an N-character prefix against the N+1-character
    /// string `folder || '/'` can never be true, which made an earlier version
    /// of this query a silent no-op.
    pub fn delete_metadata_under(&self, folder_path: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM photo_metadata
             WHERE substr(photo_path, 1, length(?1) + 1) = ?1 || '/'",
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
             WHERE substr(image_name, 1, length(?1) + 1) = ?1 || '/'",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().expect("in-memory database")
    }

    fn seed(db: &Db, paths: &[&str]) {
        for path in paths {
            db.set_metadata(path, 400, 300, None, 1).unwrap();
        }
    }

    #[test]
    fn metadata_round_trips() {
        let db = db();
        assert_eq!(db.get_metadata("a/b.jpg"), None);
        db.set_metadata("a/b.jpg", 400, 300, Some(12), 1234).unwrap();
        assert_eq!(db.get_metadata("a/b.jpg"), Some((400, 300, Some(12), 1234)));
        // A second write updates in place rather than duplicating.
        db.set_metadata("a/b.jpg", 100, 50, None, 9999).unwrap();
        assert_eq!(db.get_metadata("a/b.jpg"), Some((100, 50, None, 9999)));
        db.delete_metadata("a/b.jpg").unwrap();
        assert_eq!(db.get_metadata("a/b.jpg"), None);
    }

    #[test]
    fn delete_metadata_under_purges_only_the_subtree() {
        let db = db();
        seed(
            &db,
            &[
                "1970-79/1970/a.jpg",
                "1970-79/1970/deep/b.jpg",
                "1970-79/1971/c.jpg",
                // Shares the `1970-79/197` prefix: a `LIKE '1970-79/197%'`
                // would wrongly delete this one.
                "1970-79/19700/d.jpg",
                // The folder's own key is not "under" itself.
                "1970-79/1970",
            ],
        );

        db.delete_metadata_under("1970-79/1970").unwrap();

        assert_eq!(db.get_metadata("1970-79/1970/a.jpg"), None);
        assert_eq!(db.get_metadata("1970-79/1970/deep/b.jpg"), None);
        assert!(db.get_metadata("1970-79/1971/c.jpg").is_some());
        assert!(db.get_metadata("1970-79/19700/d.jpg").is_some());
        assert!(db.get_metadata("1970-79/1970").is_some());
    }

    #[test]
    fn prefix_deletes_treat_wildcards_as_literal_text() {
        let db = db();
        seed(&db, &["100%/a.jpg", "100x/a.jpg", "a_b/p.jpg", "aXb/p.jpg"]);

        db.delete_metadata_under("100%").unwrap();
        db.delete_metadata_under("a_b").unwrap();

        assert_eq!(db.get_metadata("100%/a.jpg"), None);
        assert!(db.get_metadata("100x/a.jpg").is_some());
        assert_eq!(db.get_metadata("a_b/p.jpg"), None);
        assert!(db.get_metadata("aXb/p.jpg").is_some());
    }

    #[test]
    fn delete_covers_under_purges_pointers_into_the_folder() {
        let db = db();
        // A cover on an ancestor that points at a photo in the removed folder
        // must go too, or the grid would show a letterboxed dead cover.
        db.set_cover("1970-79", "1970-79/1970/a.jpg").unwrap();
        db.set_cover("1970-79/1970", "1970-79/1970/a.jpg").unwrap();
        // Unrelated cover, and one whose name merely shares a prefix.
        db.set_cover("1970-79/1971", "1970-79/1971/b.jpg").unwrap();
        db.set_cover("1970-79/19700", "1970-79/19700/d.jpg").unwrap();

        db.delete_covers_under("1970-79/1970").unwrap();

        assert_eq!(db.get_cover("1970-79"), None);
        assert_eq!(db.get_cover("1970-79/1970"), None);
        assert!(db.get_cover("1970-79/1971").is_some());
        assert!(db.get_cover("1970-79/19700").is_some());
    }

    #[test]
    fn delete_cover_if_matches_requires_the_exact_image() {
        let db = db();
        db.set_cover("1970-79/1971", "1970-79/1971/b.jpg").unwrap();

        db.delete_cover_if_matches("1970-79/1971", "1970-79/1971/other.jpg").unwrap();
        assert!(db.get_cover("1970-79/1971").is_some());

        db.delete_cover_if_matches("1970-79/1971", "1970-79/1971/b.jpg").unwrap();
        assert_eq!(db.get_cover("1970-79/1971"), None);
    }
}
