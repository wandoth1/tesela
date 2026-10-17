//! Directory children as seen by the interface (map and list): sorted by descending size.
//!
//! `TreeBuilder` already leaves them contiguous and sorted, except for the synthetic `SYSTEM_INFO` node ("Windows and
//! other system data"), which is ALWAYS last, even if it is huge. For the map and list it is placed by size and hidden
//! when it weighs less than 1% of the directory (PLAN.md §2.8). Only a volume root has one, so normally this is the
//! tree's contiguous range, with no memory allocation.

use std::ops::Range;
use tesela_model::{Kids, NodeId, Tree, View, flags};

/// Below this percentage of the directory size, the system block is hidden.
pub const SYSTEM_BLOCK_MIN_PERCENT: u64 = 1;

/// Visible directory children, in descending size order.
#[derive(Clone, Debug)]
pub enum Children {
    /// The tree's contiguous range (no system block to reposition).
    Range(Range<NodeId>),
    /// Custom order: a system block was positioned by size (or hidden).
    List(Vec<NodeId>),
}

impl Children {
    /// Visible children of `node` (without a cleanup overlay).
    pub fn of(tree: &Tree, node: NodeId) -> Children {
        Children::of_view(&View::new(tree, None), node)
    }

    /// Visible children of `node` through a view (with its cleanup overlay, if any).
    pub fn of_view(view: &View, node: NodeId) -> Children {
        let tree = view.tree();
        let kids = view.kids(node);
        if kids.is_empty() {
            return Children::Range(0..0);
        }
        let last = kids.get(kids.len() - 1);
        if tree.node(last).flags & flags::SYSTEM_INFO == 0 {
            return match kids {
                Kids::Range(r) => Children::Range(r),
                Kids::List(v) => Children::List(v),
            };
        }
        let sys_alloc = view.alloc(last);
        let parent_alloc = view.alloc(node);
        let visible = sys_alloc > 0
            && (sys_alloc as u128) * 100
                >= (parent_alloc as u128) * SYSTEM_BLOCK_MIN_PERCENT as u128;
        let mut ids: Vec<NodeId> = kids.iter().take(kids.len() - 1).collect();
        if visible {
            // `partition_point` requires the predicate to be `true` first and `false` after: this holds because
            // children are sorted by descending visible size.
            let pos = ids.partition_point(|&c| view.alloc(c) >= sys_alloc);
            ids.insert(pos, last);
        }
        Children::List(ids)
    }

    /// Number of visible children.
    pub fn len(&self) -> usize {
        match self {
            Children::Range(r) => r.len(),
            Children::List(v) => v.len(),
        }
    }

    /// Are there none?
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The child at position `i` (panics if it does not exist).
    pub fn get(&self, i: usize) -> NodeId {
        match self {
            Children::Range(r) => r.start + i as u32,
            Children::List(v) => v[i],
        }
    }

    /// Iterator over ids starting at position `from`.
    pub fn iter_from(&self, from: usize) -> impl Iterator<Item = NodeId> + '_ {
        (from..self.len()).map(|i| self.get(i))
    }
}
