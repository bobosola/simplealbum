use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use tracing::info;

use crate::util;

/// One row of `photo_metadata`, as read back by [`Db::get_metadata`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotoMeta {
    pub width: u32,
    pub height: u32,
    pub duration: Option<u64>,
    /// Source mtime, in seconds since the epoch, at the time this row was
    /// written.
    pub modified: i64,
    /// Whether a probe of the source has actually run and its result been
    /// recorded. Rows written before this column existed default to `false`, so
    /// they are re-probed once and then left alone. See
    /// [`crate::worker::metadata_incomplete`] for why this is the flag that
    /// decides whether a video without a duration is retried.
    pub probed: bool,
}

/// Largest number of path parameters bound into one batched query. SQLite's
/// compiled variable limit is well above this, but chunking keeps the generated
/// SQL short and the bound on memory obvious.
const QUERY_CHUNK: usize = 500;

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
        // `probed` records that a metadata probe ran and its result was stored,
        // which is what distinguishes "this container genuinely has no duration"
        // from "this row was written before durations were recorded". Existing
        // rows default to 0, so each is re-probed exactly once after the
        // upgrade and never again.
        let _ = conn.execute(
            "ALTER TABLE photo_metadata ADD COLUMN probed INTEGER NOT NULL DEFAULT 0",
            [],
        );
        Ok(())
    }

    /// Rewrite any stored paths that are not in canonical form.
    ///
    /// Rows written by an earlier build kept whatever the client sent, so a
    /// `set_cover` for target `"1970-79/"` or image `"./1970-79/a.jpg"` left a
    /// row that no lookup can match: covers silently never appeared. The write
    /// path normalises now, and this repairs what is already on disk. Returns
    /// the number of rows whose paths actually changed (the table is rewritten
    /// as a whole, because a normalised key can collide with an existing one).
    pub fn normalize_stored_paths(&self) -> anyhow::Result<usize> {
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        let rows: Vec<(String, String, i64)> = {
            let mut stmt = conn.prepare(
                "SELECT folder_path, image_name, updated_at FROM folder_covers",
            )?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            rows.collect::<Result<_, _>>()?
        };

        // Rebuild the table only when something actually needs changing, so a
        // clean database costs one read and no writes on every startup.
        let repaired: Vec<(String, String, i64)> = rows
            .iter()
            .map(|(folder, image, updated)| {
                (
                    util::normalize_rel_path(folder),
                    util::normalize_rel_path(image),
                    *updated,
                )
            })
            .collect();
        let changed = repaired != rows;
        let changed_count = rows
            .iter()
            .zip(&repaired)
            .filter(|((folder, image, _), (new_folder, new_image, _))| {
                folder != new_folder || image != new_image
            })
            .count();
        if !changed {
            return Ok(0);
        }

        // Collect every normalised row in a transaction. Two source rows can
        // normalise onto the same key (`"1970-79"` and `"1970-79/"`), which the
        // primary key forbids, so the most recently updated one wins.
        let mut merged: HashMap<String, (String, i64)> = HashMap::new();
        for (folder, image, updated) in repaired {
            merged
                .entry(folder)
                .and_modify(|entry| {
                    if updated >= entry.1 {
                        *entry = (image.clone(), updated);
                    }
                })
                .or_insert((image, updated));
        }

        let tx = conn.transaction()?;
        tx.execute("DELETE FROM folder_covers", [])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO folder_covers (folder_path, image_name, updated_at)
                 VALUES (?1, ?2, ?3)",
            )?;
            for (folder, (image, updated)) in &merged {
                stmt.execute(params![folder, image, updated])?;
            }
        }
        tx.commit()?;

        Ok(changed_count)
    }

    pub fn get_cover(&self, folder_path: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT image_name FROM folder_covers WHERE folder_path = ?1",
            params![folder_path],
            |row| row.get(0),
        ).optional().unwrap_or(None)
    }

    /// Explicit covers for many folders in one query, keyed by folder path.
    ///
    /// Equivalent to calling [`Db::get_cover`] per folder, but takes the
    /// connection lock once instead of once per subfolder in a listing.
    pub fn get_covers(&self, folder_paths: &[String]) -> HashMap<String, String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut covers = HashMap::new();
        for chunk in folder_paths.chunks(QUERY_CHUNK) {
            let sql = format!(
                "SELECT folder_path, image_name FROM folder_covers WHERE folder_path IN ({})",
                placeholders(chunk.len())
            );
            let Ok(mut stmt) = conn.prepare(&sql) else { continue };
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            });
            let Ok(rows) = rows else { continue };
            covers.extend(rows.filter_map(|row| row.ok()));
        }
        covers
    }

    pub fn set_cover(&self, folder_path: &str, image_name: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
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

    pub fn get_metadata(&self, photo_path: &str) -> Option<PhotoMeta> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT width, height, duration, modified, probed
             FROM photo_metadata WHERE photo_path = ?1",
            params![photo_path],
            |row| {
                Ok(PhotoMeta {
                    width: row.get(0)?,
                    height: row.get(1)?,
                    duration: row.get(2)?,
                    modified: row.get(3)?,
                    probed: row.get::<_, i64>(4)? != 0,
                })
            },
        ).optional().unwrap_or(None)
    }

    /// Metadata for many photos in one query, keyed by photo path. Missing paths
    /// are simply absent from the map.
    ///
    /// A listing needs the metadata of every photo in the folder, and issuing
    /// one point query per file meant taking the connection lock and preparing a
    /// statement hundreds of times for a single page.
    pub fn get_metadata_for(&self, photo_paths: &[String]) -> HashMap<String, PhotoMeta> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut metadata = HashMap::new();
        for chunk in photo_paths.chunks(QUERY_CHUNK) {
            let sql = format!(
                "SELECT photo_path, width, height, duration, modified, probed
                 FROM photo_metadata WHERE photo_path IN ({})",
                placeholders(chunk.len())
            );
            let Ok(mut stmt) = conn.prepare(&sql) else { continue };
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    PhotoMeta {
                        width: row.get(1)?,
                        height: row.get(2)?,
                        duration: row.get(3)?,
                        modified: row.get(4)?,
                        probed: row.get::<_, i64>(5)? != 0,
                    },
                ))
            });
            let Ok(rows) = rows else { continue };
            metadata.extend(rows.filter_map(|row| row.ok()));
        }
        metadata
    }

    pub fn set_metadata(
        &self,
        photo_path: &str,
        width: u32,
        height: u32,
        duration: Option<u64>,
        modified: i64,
        probed: bool,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO photo_metadata (photo_path, width, height, duration, modified, probed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(photo_path) DO UPDATE SET
                width = excluded.width,
                height = excluded.height,
                duration = excluded.duration,
                modified = excluded.modified,
                probed = excluded.probed",
            params![photo_path, width, height, duration, modified, probed as i64],
        )?;
        Ok(())
    }

    pub fn delete_metadata(&self, photo_path: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
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
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
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
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "DELETE FROM folder_covers
             WHERE substr(image_name, 1, length(?1) + 1) = ?1 || '/'",
            params![folder_path],
        )?;
        Ok(())
    }

    pub fn delete_cover(&self, folder_path: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "DELETE FROM folder_covers WHERE folder_path = ?1",
            params![folder_path],
        )?;
        Ok(())
    }

    /// Delete a folder cover only if it references the given image name.
    /// Used when a photo is deleted to clean up its parent folder's cover.
    pub fn delete_cover_if_matches(&self, folder_path: &str, image_name: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "DELETE FROM folder_covers WHERE folder_path = ?1 AND image_name = ?2",
            params![folder_path, image_name],
        )?;
        Ok(())
    }
}

fn placeholders(n: usize) -> String {
    std::iter::repeat_n("?", n).collect::<Vec<_>>().join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().expect("in-memory database")
    }

    fn seed(db: &Db, paths: &[&str]) {
        for path in paths {
            db.set_metadata(path, 400, 300, None, 1, true).unwrap();
        }
    }

    fn meta(width: u32, height: u32, duration: Option<u64>, modified: i64, probed: bool) -> PhotoMeta {
        PhotoMeta { width, height, duration, modified, probed }
    }

    #[test]
    fn metadata_round_trips() {
        let db = db();
        assert_eq!(db.get_metadata("a/b.jpg"), None);
        db.set_metadata("a/b.jpg", 400, 300, Some(12), 1234, true).unwrap();
        assert_eq!(
            db.get_metadata("a/b.jpg"),
            Some(meta(400, 300, Some(12), 1234, true))
        );
        // A second write updates in place rather than duplicating.
        db.set_metadata("a/b.jpg", 100, 50, None, 9999, false).unwrap();
        assert_eq!(
            db.get_metadata("a/b.jpg"),
            Some(meta(100, 50, None, 9999, false))
        );
        db.delete_metadata("a/b.jpg").unwrap();
        assert_eq!(db.get_metadata("a/b.jpg"), None);
    }

    #[test]
    fn batched_metadata_matches_the_point_lookup() {
        let db = db();
        db.set_metadata("a/1.jpg", 10, 20, None, 5, true).unwrap();
        db.set_metadata("a/2.mp4", 30, 40, Some(7), 6, true).unwrap();

        let wanted = vec!["a/1.jpg".to_string(), "a/2.mp4".to_string(), "a/missing.jpg".to_string()];
        let found = db.get_metadata_for(&wanted);

        assert_eq!(found.len(), 2, "missing rows are absent, not defaulted");
        assert_eq!(found.get("a/1.jpg"), Some(&meta(10, 20, None, 5, true)));
        assert_eq!(found.get("a/2.mp4"), Some(&meta(30, 40, Some(7), 6, true)));
        assert_eq!(db.get_metadata_for(&[]).len(), 0);
    }

    #[test]
    fn batched_covers_match_the_point_lookup() {
        let db = db();
        db.set_cover("a", "a/1.jpg").unwrap();
        db.set_cover("b", "b/2.jpg").unwrap();

        let found = db.get_covers(&["a".to_string(), "b".to_string(), "c".to_string()]);
        assert_eq!(found.get("a").map(String::as_str), Some("a/1.jpg"));
        assert_eq!(found.get("b").map(String::as_str), Some("b/2.jpg"));
        assert_eq!(found.get("c"), None);
        assert!(db.get_covers(&[]).is_empty());
    }

    #[test]
    fn probed_defaults_to_false_for_rows_written_before_the_column_existed() {
        let db = db();
        {
            let conn = db.conn.lock().unwrap();
            // Simulate a legacy row: written without a `probed` value.
            conn.execute(
                "INSERT INTO photo_metadata (photo_path, width, height, modified)
                 VALUES ('old.mp4', 1920, 1080, 42)",
                [],
            )
            .unwrap();
        }
        let row = db.get_metadata("old.mp4").expect("legacy row");
        assert!(!row.probed, "a legacy row must be re-probed once after upgrading");
        assert_eq!(row.duration, None);
    }

    #[test]
    fn unnormalised_stored_paths_are_repaired() {
        let db = db();
        {
            let conn = db.conn.lock().unwrap();
            // Exactly the rows the old `set_cover` would have written for
            // targets "1970-79/" and "./1970-79".
            conn.execute(
                "INSERT INTO folder_covers (folder_path, image_name, updated_at)
                 VALUES ('1970-79/', '1970-79/a.jpg', 1),
                        ('./1970-79', './1970-79/b.jpg', 2),
                        ('1971', '1971/c.jpg', 3)",
                [],
            )
            .unwrap();
        }

        assert_eq!(db.normalize_stored_paths().unwrap(), 2);
        assert_eq!(db.get_cover("1970-79").as_deref(), Some("1970-79/b.jpg"));
        assert_eq!(db.get_cover("1970-79/"), None);
        assert_eq!(db.get_cover("1971").as_deref(), Some("1971/c.jpg"));

        // A clean database is left alone, and the repair is a no-op on re-run.
        assert_eq!(db.normalize_stored_paths().unwrap(), 0);
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
