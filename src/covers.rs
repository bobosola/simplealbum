//! Cached, computed folder covers.
//!
//! A folder without an explicit cover falls back to
//! [`crate::api::find_first_thumb_recursive`], which walks up to three directory
//! levels looking for a usable thumbnail. A listing calls that once per
//! subfolder, so a folder holding N subfolders with M children each costs
//! O(N·M) `read_dir` calls — per request, on an endpoint that `/api/share`
//! exposes to unauthenticated crawlers.
//!
//! The answer only changes when the filesystem does, so it is memoised here and
//! dropped wholesale when the watcher reports a change. Invalidation is
//! generation-based, exactly as in [`crate::counts`]: the watcher bumps a
//! counter (one atomic increment, never a lock held across a clear) and entries
//! tagged with an older generation are ignored.
//!
//! Only the *computed* cover is cached. An admin's explicit choice lives in the
//! database, is a single indexed read, and is applied on top by the caller —
//! caching it here would mean re-invalidating on every `set_cover`, and the
//! cache exists to avoid the walk, not the query.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Upper bound on cached folders, mirroring [`crate::counts`]. On overflow the
/// stale entries are pruned first, and only if that is not enough is the map
/// dropped wholesale.
const MAX_ENTRIES: usize = 16_384;

/// Computed covers for folders, invalidated as a whole.
#[derive(Default)]
pub struct CoverCache {
    generation: AtomicU64,
    entries: Mutex<HashMap<String, (u64, Option<String>)>>,
}

impl CoverCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop every cached cover. Called by the filesystem watcher for each change
    /// inside the album — including changes inside a `thumbs` folder, because a
    /// newly generated thumbnail is exactly what makes a previously cover-less
    /// folder resolvable.
    pub fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// The cached value for `folder`, or `None` when there is no *live* entry.
    ///
    /// Note the nested `Option`: the outer layer is "was this folder resolved
    /// under the current generation", the inner one is the resolved answer,
    /// which may legitimately be "no cover available".
    pub fn get(&self, folder: &str) -> Option<Option<String>> {
        let generation = self.generation.load(Ordering::SeqCst);
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .get(folder)
            .filter(|(g, _)| *g == generation)
            .map(|(_, value)| value.clone())
    }

    pub fn insert(&self, folder: String, value: Option<String>) {
        let generation = self.generation.load(Ordering::SeqCst);
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.len() >= MAX_ENTRIES {
            // Prune the previous generations before resorting to a full clear:
            // otherwise a long-lived process with frequent invalidations spends
            // its bound on entries that can never be read again.
            entries.retain(|_, (g, _)| *g == generation);
            if entries.len() >= MAX_ENTRIES {
                entries.clear();
            }
        }
        entries.insert(folder, (generation, value));
    }

    /// Cached cover for `folder`, computing it with `resolve` on a miss.
    pub fn get_or_compute<F>(&self, folder: &str, resolve: F) -> Option<String>
    where
        F: FnOnce() -> Option<String>,
    {
        if let Some(cached) = self.get(folder) {
            return cached;
        }
        let value = resolve();
        self.insert(folder.to_string(), value.clone());
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_computed_value_is_reused_until_invalidated() {
        let cache = CoverCache::new();
        let mut calls = 0;

        let first = cache.get_or_compute("a/b", || {
            calls += 1;
            Some("thumbs/x_thumb.jpg".to_string())
        });
        assert_eq!(first, Some("thumbs/x_thumb.jpg".to_string()));
        assert_eq!(calls, 1);

        let second = cache.get_or_compute("a/b", || {
            calls += 1;
            Some("thumbs/other_thumb.jpg".to_string())
        });
        assert_eq!(second, Some("thumbs/x_thumb.jpg".to_string()));
        assert_eq!(calls, 1, "a live entry must not be recomputed");

        cache.invalidate();
        assert_eq!(cache.get("a/b"), None, "invalidation must drop old entries");

        let third = cache.get_or_compute("a/b", || {
            calls += 1;
            None
        });
        assert_eq!(third, None);
        assert_eq!(calls, 2);
    }

    #[test]
    fn folders_are_cached_independently() {
        let cache = CoverCache::new();
        assert_eq!(cache.get_or_compute("a", || Some("thumbs/a.jpg".into())), Some("thumbs/a.jpg".into()));
        assert_eq!(cache.get_or_compute("b", || None), None);
        assert_eq!(cache.get("a"), Some(Some("thumbs/a.jpg".to_string())));
        assert_eq!(cache.get("b"), Some(None));
        assert_eq!(cache.get("c"), None);
    }
}
