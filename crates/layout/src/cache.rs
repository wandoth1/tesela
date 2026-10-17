//! Layout cache keyed by `(root, viewport, depth, version)` (PLAN.md §3.8).

use crate::layout::{Key, Layout};
use std::sync::Arc;
use tesela_model::{Tree, View};

/// Small cache of recent layouts. Keeping more than one allows zooming between two levels and back without recalculating.
#[derive(Debug)]
pub struct LayoutCache {
    entries: Vec<(Key, Arc<Layout>)>,
    capacity: usize,
    hits: u64,
    misses: u64,
}

impl LayoutCache {
    /// Cache of up to `capacity` layouts (minimum 1).
    pub fn new(capacity: usize) -> LayoutCache {
        LayoutCache {
            entries: Vec::new(),
            capacity: capacity.max(1),
            hits: 0,
            misses: 0,
        }
    }

    /// Layout for `key`: the cached one if the key matches exactly, or a new one (evicting the oldest).
    pub fn get(&mut self, tree: &Tree, key: Key) -> Arc<Layout> {
        self.get_view(View::new(tree, None), key)
    }

    /// Like [`LayoutCache::get`] for a view with an overlay: the caller changes `key.version` when the overlay changes.
    pub fn get_view(&mut self, view: View, key: Key) -> Arc<Layout> {
        if let Some(pos) = self.entries.iter().position(|(k, _)| *k == key) {
            self.hits += 1;
            let e = self.entries.remove(pos);
            let l = e.1.clone();
            self.entries.push(e);
            return l;
        }
        self.misses += 1;
        let layout = Arc::new(Layout::compute_view(view, key));
        if self.entries.len() >= self.capacity {
            self.entries.remove(0);
        }
        self.entries.push((key, layout.clone()));
        layout
    }

    /// Number of times a cached layout was returned.
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Number of times a layout had to be calculated.
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Discard everything (for example, when opening another tree).
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}
