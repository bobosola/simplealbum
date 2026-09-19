//! Cached per-folder photo and album counts.
//!
//! Building a folder listing needs, for every subfolder, the number of photos
//! in its whole subtree and the number of albums directly inside it. Computing
//! that one folder at a time re-walks the same subtrees every time the user
//! descends a level, so a single depth-first pass records the counts for
//! *every* folder it visits, and the whole map is cached until the filesystem
//! changes. The watcher invalidates the cache, so a count is never stale after
//! an add, rename or delete.
//!
//! Working out the cost this way is what keeps a listing off the critical path:
//! the first request after a change pays for one walk of the subtree, and every
//! folder beneath it (and every later request) is answered from memory.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::util;

/// Upper bound on cached folders. A personal album has hundreds of folders, so
/// this only guards against pathological growth; on overflow the cache is
/// dropped wholesale and rebuilt on the next request rather than growing
/// without limit.
const MAX_ENTRIES: usize = 16_384;

/// A `(photos-in-subtree, albums-directly-inside)` pair.
pub type Counts = (usize, usize);

/// Photo/album counts for every folder walked, invalidated as a whole.
///
/// Entries are tagged with the generation they were computed under, so
/// [`CountCache::invalidate`] is a single atomic increment rather than a lock
/// held over a clear: a request that is already mid-walk simply publishes its
/// results under the old generation and they are ignored.
#[derive(Default)]
pub struct CountCache {
    generation: AtomicU64,
    entries: Mutex<HashMap<String, (u64, Counts)>>,
}

impl CountCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop every cached count. Called by the filesystem watcher for each
    /// change inside the album.
    pub fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn get(&self, folder: &str) -> Option<Counts> {
        let generation = self.generation.load(Ordering::SeqCst);
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .get(folder)
            .filter(|(g, _)| *g == generation)
            .map(|(_, counts)| *counts)
    }

    fn extend(&self, counts: HashMap<String, Counts>) {
        let generation = self.generation.load(Ordering::SeqCst);
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.len() + counts.len() > MAX_ENTRIES {
            entries.clear();
        }
        for (folder, value) in counts {
            entries.insert(folder, (generation, value));
        }
    }

    /// Counts for `folder`, walking its subtree on a miss.
    ///
    /// Every folder reached by that walk is cached, so descending into any of
    /// them afterwards costs nothing. A concurrent invalidation can only cause
    /// a redundant walk, never a wrong answer: results published after a
    /// generation bump are ignored by the next [`CountCache::get`].
    pub fn count(&self, root: &Path, folder: &str) -> Counts {
        if let Some(counts) = self.get(folder) {
            return counts;
        }
        let counts = count_subtree(root, folder);
        let value = counts.get(folder).copied().unwrap_or((0, 0));
        self.extend(counts);
        value
    }
}

/// Compute `(photos-in-subtree, immediate-subalbum-count)` for `folder` and
/// every directory beneath it in one walk.
///
/// The walk is the same shape as the worker's: dotfiles and `thumbs` are
/// skipped, and a symlinked directory is never descended into (a self-referential
/// link would otherwise spin forever, and one pointing outside the album would
/// count files the album does not own). A symlinked *file* is counted, because
/// `entry.metadata()` resolves the link.
///
/// Folders are discovered in pre-order and then totalled in reverse, so each
/// child is finished before its parent without recursion.
fn count_subtree(root: &Path, folder: &str) -> HashMap<String, Counts> {
    let mut order: Vec<String> = Vec::new();
    let mut direct_photos: HashMap<String, usize> = HashMap::new();
    let mut children: HashMap<String, Vec<String>> = HashMap::new();

    let mut stack = vec![folder.to_string()];
    while let Some(current) = stack.pop() {
        order.push(current.clone());
        direct_photos.entry(current.clone()).or_insert(0);

        let Ok(entries) = std::fs::read_dir(root.join(&current)) else {
            continue;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || name == "thumbs" {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let is_symlink = file_type.is_symlink();
            let is_dir = if is_symlink {
                entry.metadata().map(|m| m.is_dir()).unwrap_or(false)
            } else {
                file_type.is_dir()
            };
            let sub = if current.is_empty() {
                name.to_string()
            } else {
                format!("{}/{}", current, name)
            };
            if is_dir && !is_symlink {
                children.entry(current.clone()).or_default().push(sub.clone());
                stack.push(sub);
            } else if !is_dir && util::is_media_file(&name) {
                *direct_photos.entry(current.clone()).or_insert(0) += 1;
            }
        }
    }

    // Reversing a pre-order discovery guarantees every child is processed
    // before its parent, so subtree totals can be summed upwards.
    let mut totals: HashMap<String, Counts> = HashMap::with_capacity(order.len());
    for folder in order.iter().rev() {
        let mut photos = direct_photos.get(folder).copied().unwrap_or(0);
        let kids = children.get(folder).cloned().unwrap_or_default();
        for kid in &kids {
            photos += totals.get(kid).map(|(p, _)| *p).unwrap_or(0);
        }
        totals.insert(folder.clone(), (photos, kids.len()));
    }
    totals
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempTree;
    use std::path::PathBuf;

    fn tree() -> (PathBuf, TempTree) {
        let t = TempTree::new();
        t.file("a/b/one.jpg");
        t.file("a/two.mp4");
        t.file("c/three.png");
        // Must be ignored: generated thumbnails, dotfiles and non-media files.
        t.file("a/thumbs/one_thumb.jpg");
        t.file(".hidden/four.jpg");
        t.file("a/notes.txt");
        let root = t.0.clone();
        (root, t)
    }

    #[test]
    fn counts_are_recursive_and_albums_only_immediate() {
        let (root, _guard) = tree();
        let counts = count_subtree(&root, "");

        // Two top-level albums, three media files in total.
        assert_eq!(counts.get(""), Some(&(3, 2)));
        // `a` holds its own video plus `b`'s photo, and one subalbum (`b`).
        assert_eq!(counts.get("a"), Some(&(2, 1)));
        assert_eq!(counts.get("a/b"), Some(&(1, 0)));
        assert_eq!(counts.get("c"), Some(&(1, 0)));
        // Ignored paths never appear.
        assert_eq!(counts.get("a/thumbs"), None);
        assert_eq!(counts.get(".hidden"), None);
    }

    #[test]
    fn cache_populates_descendants_and_honours_invalidation() {
        let (root, _guard) = tree();
        let cache = CountCache::new();

        assert_eq!(cache.count(&root, "a"), (2, 1));
        // Descendants were cached by the same walk, so these do not re-read.
        assert_eq!(cache.get("a/b"), Some((1, 0)));

        cache.invalidate();
        assert_eq!(cache.get("a/b"), None, "invalidation must drop old entries");
        assert_eq!(cache.count(&root, "a/b"), (1, 0));
    }

    #[test]
    fn a_missing_folder_counts_as_empty() {
        let (root, _guard) = tree();
        let cache = CountCache::new();
        assert_eq!(cache.count(&root, "does-not-exist"), (0, 0));
    }
}
