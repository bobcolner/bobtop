//! Bounded LRU of rendered previews keyed by path.
//!
//! Cache hits return synchronously and skip the spawn → channel round
//! trip entirely. Capacity is conservative (32 entries) — a `Preview`
//! with 5k lines is a few hundred KiB of styled spans, so the bound
//! caps memory at ~20 MiB worst case.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use lru::LruCache;

use super::Preview;

pub struct PreviewCache {
    inner: LruCache<PathBuf, Arc<Preview>>,
}

impl PreviewCache {
    pub fn new(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1)).unwrap();
        Self {
            inner: LruCache::new(cap),
        }
    }

    pub fn get(&mut self, path: &PathBuf) -> Option<Arc<Preview>> {
        self.inner.get(path).cloned()
    }

    pub fn put(&mut self, path: PathBuf, preview: Arc<Preview>) {
        self.inner.put(path, preview);
    }

    pub fn invalidate(&mut self, path: &PathBuf) {
        self.inner.pop(path);
    }

    /// Drop every cached preview. Used when a setting changes that
    /// would render every cached entry stale — e.g. toggling hidden
    /// files would change the contents of every cached directory.
    pub fn clear(&mut self) {
        self.inner.clear();
    }
}

impl Default for PreviewCache {
    fn default() -> Self {
        Self::new(32)
    }
}
