//! Cleanup overlay (PLAN.md §3.6): the tree changes after cleanup WITHOUT rescanning or copying the `Tree`.
//!
//! * A bitset of removed nodes (125 KB per million nodes).
//! * An `FxHashMap<NodeId, Delta>` for ancestors: removing an item subtracts its size along at most ~30 ancestors.
//! * Visible size = base − delta. The layout reorders visible children of touched nodes.
//! * Discarded on rescan (the new tree is a different one).
//!
//! Invariant: every ancestor of a changed item is in `delta`, so a node NOT in `delta` has exactly the same children and
//! order as the base tree. Histograms (types, age) are not recalculated: the interface marks them as approximate until a rescan.

use crate::tree::{NodeId, Tree};
use crate::{NO_PARENT, flags};
use rustc_hash::FxHashMap;
use std::ops::Range;

/// Amount subtracted from a node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Delta {
    pub alloc: u64,
    pub logical: u64,
    pub files: u64,
}

/// The overlay.
#[derive(Clone, Debug, Default)]
pub struct Overlay {
    removed: Vec<u64>,
    delta: FxHashMap<NodeId, Delta>,
    version: u64,
}

impl Overlay {
    /// Empty overlay for a tree with `tree.len()` nodes.
    pub fn new(tree: &Tree) -> Overlay {
        Overlay {
            removed: vec![0; tree.len().div_ceil(64)],
            delta: FxHashMap::default(),
            version: 0,
        }
    }

    /// Counter that changes with each modification (included in the layout cache key).
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Has anything changed?
    pub fn is_empty(&self) -> bool {
        self.version == 0
    }

    fn bit(&self, id: NodeId) -> bool {
        self.removed
            .get(id as usize / 64)
            .is_some_and(|w| w >> (id % 64) & 1 != 0)
    }

    /// Is this node marked as removed (itself, not its ancestors)?
    pub fn is_removed(&self, id: NodeId) -> bool {
        self.bit(id)
    }

    /// Has anything changed in its children or size (is it in `delta`)?
    pub fn is_touched(&self, id: NodeId) -> bool {
        self.delta.contains_key(&id)
    }

    /// Amount subtracted from `id`.
    pub fn delta(&self, id: NodeId) -> Delta {
        self.delta.get(&id).copied().unwrap_or_default()
    }

    /// Add `d` to `from` (if `include_self`) and all its ancestors. At each node, the subtraction is capped at what the
    /// node contains; what continues upward is the amount ACTUALLY subtracted there (the difference from before), not
    /// `d`. Otherwise, an ancestor already capped (by an earlier partial cleanup of its directory) would over-subtract above it.
    fn add_up(&mut self, tree: &Tree, from: NodeId, d: Delta, include_self: bool) {
        let mut d = d;
        let mut cur = from;
        if !include_self {
            match tree.node(cur).parent {
                NO_PARENT => return,
                p => cur = p,
            }
        }
        loop {
            let e = self.delta.entry(cur).or_default();
            let prev = *e;
            let n = tree.node(cur);
            // Never subtract more than was present.
            e.alloc = e.alloc.saturating_add(d.alloc).min(n.alloc);
            e.logical = e.logical.saturating_add(d.logical).min(n.logical);
            e.files = e.files.saturating_add(d.files).min(n.files_below as u64);
            d = Delta {
                alloc: e.alloc - prev.alloc,
                logical: e.logical - prev.logical,
                files: e.files - prev.files,
            };
            match n.parent {
                NO_PARENT => break,
                p => cur = p,
            }
        }
    }

    /// Mark `id` (and everything it contains) as removed and subtract its visible size from its ancestors. Idempotent.
    /// The root cannot be removed.
    pub fn remove(&mut self, tree: &Tree, id: NodeId) {
        if id == 0 || id as usize >= tree.len() || self.bit(id) || !self.is_visible(tree, id) {
            return;
        }
        let n = tree.node(id);
        let d = Delta {
            alloc: self.alloc(tree, id),
            logical: self.logical(tree, id),
            files: if n.is_dir() {
                self.files_below(tree, id)
            } else {
                u64::from(n.flags & flags::SYSTEM_INFO == 0)
            },
        };
        if self.removed.len() < tree.len().div_ceil(64) {
            self.removed.resize(tree.len().div_ceil(64), 0);
        }
        self.removed[id as usize / 64] |= 1 << (id % 64);
        self.add_up(tree, id, d, false);
        self.version += 1;
    }

    /// Subtract `alloc`, `logical`, and `files` from `id` and its ancestors (partial cleanup of a directory that remains).
    pub fn shrink(&mut self, tree: &Tree, id: NodeId, alloc: u64, logical: u64, files: u64) {
        if id as usize >= tree.len() || !self.is_visible(tree, id) || (alloc | logical | files) == 0
        {
            return;
        }
        // No more than its remaining visible amount.
        let d = Delta {
            alloc: alloc.min(self.alloc(tree, id)),
            logical: logical.min(self.logical(tree, id)),
            files: files.min(self.files_below(tree, id)),
        };
        self.add_up(tree, id, d, true);
        self.version += 1;
    }

    /// Is `id` visible? It is not if it or any ancestor was removed.
    pub fn is_visible(&self, tree: &Tree, id: NodeId) -> bool {
        let mut cur = id;
        loop {
            if self.bit(cur) {
                return false;
            }
            match tree.node(cur).parent {
                NO_PARENT => return true,
                p => cur = p,
            }
        }
    }

    /// Visible size on disk.
    pub fn alloc(&self, tree: &Tree, id: NodeId) -> u64 {
        if self.bit(id) {
            return 0;
        }
        tree.node(id).alloc.saturating_sub(self.delta(id).alloc)
    }

    /// Visible logical size.
    pub fn logical(&self, tree: &Tree, id: NodeId) -> u64 {
        if self.bit(id) {
            return 0;
        }
        tree.node(id).logical.saturating_sub(self.delta(id).logical)
    }

    /// Visible descendant files.
    pub fn files_below(&self, tree: &Tree, id: NodeId) -> u64 {
        if self.bit(id) {
            return 0;
        }
        (tree.node(id).files_below as u64).saturating_sub(self.delta(id).files)
    }

    /// Bytes subtracted from the root by the overlay (scanned data no longer on disk).
    pub fn removed_alloc(&self) -> u64 {
        self.delta(0).alloc
    }
}

/// Visible children of a directory (see [`View::kids`]).
#[derive(Clone, Debug)]
pub enum Kids {
    /// The base tree's contiguous range (nothing changed in this directory).
    Range(Range<NodeId>),
    /// Remaining children, sorted by descending visible size (then by name for equal sizes).
    List(Vec<NodeId>),
}

impl Kids {
    pub fn len(&self) -> usize {
        match self {
            Kids::Range(r) => r.len(),
            Kids::List(v) => v.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn get(&self, i: usize) -> NodeId {
        match self {
            Kids::Range(r) => r.start + i as u32,
            Kids::List(v) => v[i],
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = NodeId> + '_ {
        (0..self.len()).map(|i| self.get(i))
    }
}

/// A tree viewed through an overlay (or as-is, without one). `Copy`: two references.
#[derive(Clone, Copy)]
pub struct View<'a> {
    tree: &'a Tree,
    overlay: Option<&'a Overlay>,
}

impl<'a> View<'a> {
    pub fn new(tree: &'a Tree, overlay: Option<&'a Overlay>) -> View<'a> {
        View {
            tree,
            overlay: overlay.filter(|o| !o.is_empty()),
        }
    }

    pub fn tree(&self) -> &'a Tree {
        self.tree
    }

    pub fn overlay(&self) -> Option<&'a Overlay> {
        self.overlay
    }

    /// Overlay version (0 without an overlay), for the layout cache key.
    pub fn version(&self) -> u64 {
        self.overlay.map_or(0, Overlay::version)
    }

    pub fn alloc(&self, id: NodeId) -> u64 {
        match self.overlay {
            Some(o) => o.alloc(self.tree, id),
            None => self.tree.node(id).alloc,
        }
    }

    pub fn logical(&self, id: NodeId) -> u64 {
        match self.overlay {
            Some(o) => o.logical(self.tree, id),
            None => self.tree.node(id).logical,
        }
    }

    pub fn files_below(&self, id: NodeId) -> u64 {
        match self.overlay {
            Some(o) => o.files_below(self.tree, id),
            None => self.tree.node(id).files_below as u64,
        }
    }

    /// Is this visible (neither it nor its ancestors were removed)?
    pub fn is_visible(&self, id: NodeId) -> bool {
        self.overlay.is_none_or(|o| o.is_visible(self.tree, id))
    }

    /// Was this directory touched (did anything inside it change)?
    pub fn is_touched(&self, id: NodeId) -> bool {
        self.overlay.is_some_and(|o| o.is_touched(id))
    }

    /// Visible children of `id`. For an untouched directory, this is the base tree range (no allocation); for a touched
    /// directory, it is the remaining children sorted by visible size (the synthetic system node stays last, as in the
    /// base tree; the layout positions it by size).
    pub fn kids(&self, id: NodeId) -> Kids {
        let r = self.tree.children(id);
        let Some(o) = self.overlay.filter(|o| o.is_touched(id)) else {
            return Kids::Range(r);
        };
        let mut v: Vec<NodeId> = r.filter(|&c| !o.is_removed(c)).collect();
        let t = self.tree;
        v.sort_by(|&a, &b| {
            let (sa, sb) = (
                t.node(a).flags & flags::SYSTEM_INFO != 0,
                t.node(b).flags & flags::SYSTEM_INFO != 0,
            );
            sa.cmp(&sb)
                .then(o.alloc(t, b).cmp(&o.alloc(t, a)))
                .then_with(|| t.name_bytes(a).cmp(t.name_bytes(b)))
        });
        Kids::List(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::{FinishOptions, TreeBuilder};
    use crate::record::{Batch, EntryData};

    const NOW: u32 = 1_760_000_000;

    fn dir() -> EntryData {
        EntryData {
            flags: flags::DIR,
            ..EntryData::default()
        }
    }

    fn file(alloc: u64) -> EntryData {
        EntryData {
            alloc,
            logical: alloc + 10,
            mtime: NOW - 86_400,
            ..EntryData::default()
        }
    }

    /// R/{a/{x 100, y 50}, b/{z 30}, w 20}: root 200, a 150, b 30, w 20.
    fn sample() -> Tree {
        let mut b = TreeBuilder::new();
        let mut root = Batch::detached(0);
        root.push(0, b"R", dir());
        b.add_batch(root).unwrap();
        let mut l0 = Batch::listing(0, 1);
        l0.push(0, b"a", dir());
        l0.push(0, b"b", dir());
        l0.push(0, b"w", file(20));
        b.add_batch(l0).unwrap();
        let mut la = Batch::listing(1, 4);
        la.push(1, b"x", file(100));
        la.push(1, b"y", file(50));
        b.add_batch(la).unwrap();
        let mut lb = Batch::listing(2, 6);
        lb.push(2, b"z", file(30));
        b.add_batch(lb).unwrap();
        b.finish(&FinishOptions::new(NOW))
    }

    fn id(t: &Tree, parent: NodeId, name: &str) -> NodeId {
        t.child_by_name(parent, name.as_bytes())
            .unwrap_or_else(|| panic!("{name}"))
    }

    #[test]
    fn el_arbol_de_prueba_es_el_esperado() {
        let t = sample();
        assert_eq!((t.node(0).alloc, t.node(0).files_below), (200, 4));
        assert_eq!(t.children(0).count(), 3);
        assert_eq!(t.name_lossy(1), "a");
    }

    #[test]
    fn sin_overlay_la_vista_es_el_arbol() {
        let t = sample();
        let v = View::new(&t, None);
        assert_eq!(v.alloc(0), 200);
        assert_eq!(v.version(), 0);
        assert!(matches!(v.kids(0), Kids::Range(_)));
        // An empty overlay is treated as absent.
        let o = Overlay::new(&t);
        let v = View::new(&t, Some(&o));
        assert!(v.overlay().is_none() && matches!(v.kids(0), Kids::Range(_)));
    }

    #[test]
    fn eliminar_un_archivo_resta_a_todos_sus_ancestros() {
        let t = sample();
        let a = id(&t, 0, "a");
        let x = id(&t, a, "x");
        let mut o = Overlay::new(&t);
        o.remove(&t, x);
        assert!(o.is_removed(x) && !o.is_visible(&t, x));
        assert_eq!(
            (o.alloc(&t, a), o.logical(&t, a), o.files_below(&t, a)),
            (50, 60, 1)
        );
        assert_eq!((o.alloc(&t, 0), o.files_below(&t, 0)), (100, 3));
        assert_eq!(o.alloc(&t, x), 0);
        assert!(o.is_touched(a) && o.is_touched(0) && !o.is_touched(id(&t, 0, "b")));
        assert_eq!(o.version(), 1);
        // Idempotente.
        o.remove(&t, x);
        assert_eq!((o.alloc(&t, 0), o.version()), (100, 1));
        // The visible child of `a` is only `y`; `b` still uses the tree range.
        let v = View::new(&t, Some(&o));
        assert_eq!(v.kids(a).iter().collect::<Vec<_>>(), vec![id(&t, a, "y")]);
        assert!(matches!(v.kids(id(&t, 0, "b")), Kids::Range(_)));
        assert_eq!(v.alloc(a), 50);
        assert_eq!(v.version(), 1);
    }

    #[test]
    fn eliminar_una_carpeta_oculta_todo_lo_que_contiene() {
        let t = sample();
        let a = id(&t, 0, "a");
        let (x, y) = (id(&t, a, "x"), id(&t, a, "y"));
        let mut o = Overlay::new(&t);
        o.remove(&t, a);
        assert!(!o.is_visible(&t, x) && !o.is_visible(&t, y));
        assert_eq!((o.alloc(&t, 0), o.files_below(&t, 0)), (50, 2));
        // Removing a descendant of an already removed node changes nothing.
        o.remove(&t, x);
        assert_eq!((o.alloc(&t, 0), o.version()), (50, 1));
        let v = View::new(&t, Some(&o));
        let kids: Vec<String> = v
            .kids(0)
            .iter()
            .map(|c| t.name_lossy(c).into_owned())
            .collect();
        assert_eq!(kids, ["b", "w"]);
    }

    #[test]
    fn una_limpieza_parcial_reordena_los_hijos_visibles() {
        let t = sample();
        let (a, b, w) = (id(&t, 0, "a"), id(&t, 0, "b"), id(&t, 0, "w"));
        let mut o = Overlay::new(&t);
        // `b` weighed 30 and is now 15: it moves below `w` (20).
        o.shrink(&t, b, 15, 15, 1);
        assert_eq!((o.alloc(&t, b), o.alloc(&t, 0)), (15, 185));
        let v = View::new(&t, Some(&o));
        assert_eq!(v.kids(0).iter().collect::<Vec<_>>(), vec![a, w, b]);
        // For equal visible sizes, sort by name.
        o.shrink(&t, w, 5, 5, 0);
        let v = View::new(&t, Some(&o));
        assert_eq!(
            v.kids(0).iter().collect::<Vec<_>>(),
            vec![a, b, w],
            "15 y 15: b antes que w"
        );
    }

    /// GM-M7: after a partial directory cleanup (`shrink`), removing a descendant already partly counted by `shrink`
    /// must not subtract twice from ancestors: each level passes upward only what it actually subtracted.
    #[test]
    fn shrink_en_una_carpeta_y_eliminar_un_descendiente_resta_exactamente_el_total_real() {
        let t = sample();
        let (a, b, w) = (id(&t, 0, "a"), id(&t, 0, "b"), id(&t, 0, "w"));
        let x = id(&t, a, "x");
        let mut o = Overlay::new(&t);
        // `a` weighs 150 (x = 100, y = 50): a partial cleanup removes 120.
        o.shrink(&t, a, 120, 120, 1);
        assert_eq!((o.alloc(&t, a), o.alloc(&t, 0)), (30, 80));
        // `x` still appears "visible" with 100: removing it can make `a` lose only the 30 it had left.
        o.remove(&t, x);
        assert_eq!(o.alloc(&t, a), 0, "a queda vacía");
        assert_eq!(
            o.delta(a).alloc,
            150,
            "a: lo restado está acotado a lo que tiene"
        );
        // The root subtracts EXACTLY 150 (120 + 30): b (30) and w (20) remain, not 0.
        assert_eq!(o.delta(0).alloc, 150, "doble resta en la raíz");
        assert_eq!(o.alloc(&t, 0), 50);
        assert_eq!(o.alloc(&t, b) + o.alloc(&t, w), 50);
        assert_eq!(o.removed_alloc(), 150);
        // Same for logical size and files: the root subtracts the same amount as `a` (everything `a` contained).
        assert_eq!(o.delta(0).logical, o.delta(a).logical);
        assert_eq!(o.delta(0).files, o.delta(a).files);
        assert_eq!(o.logical(&t, a), 0);
    }

    #[test]
    fn nunca_se_resta_mas_de_lo_que_hay_ni_se_toca_la_raiz() {
        let t = sample();
        let b = id(&t, 0, "b");
        let mut o = Overlay::new(&t);
        o.shrink(&t, b, 1_000, 1_000, 10);
        assert_eq!(
            (o.alloc(&t, b), o.logical(&t, b), o.files_below(&t, b)),
            (0, 0, 0)
        );
        assert_eq!(o.alloc(&t, 0), 170, "solo se resta lo que había en `b`");
        let before = o.version();
        o.remove(&t, 0);
        assert!(
            !o.is_removed(0) && o.version() == before,
            "la raíz no se puede eliminar"
        );
        // An out-of-range id changes nothing.
        o.remove(&t, 9_999);
        o.shrink(&t, 9_999, 1, 1, 1);
        assert_eq!(o.version(), before);
        assert_eq!(o.removed_alloc(), 30);
    }
}
