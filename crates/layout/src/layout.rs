//! The layout itself: from a tree node to a list of cells and their rectangles.
//!
//! # Two distinct layers (PLAN.md §2.9)
//! * **Interactive grouping** ("Other"): while laying out the root node's children, those smaller than 24×24 px (area
//!   below `min_group_px²`) are merged into ONE "Other (N items, X)" cell. Since children are sorted by size, these are
//!   always a SUFFIX of the sorted list, and the cell can be opened: it becomes another root ([`Root`] with `start` > 0)
//!   that lays out only that suffix. Grouping happens only when there are at least 2 items and one has nonzero size.
//! * **Drawing LOD**: at the hinted level, items below ~3×3 px (`lod_px`) are not drawn, and the total number of
//!   rectangles is capped (`max_rects`, ~15,000 per frame). The budget is SHARED by both levels and applied before the
//!   first layout: if the main level would exceed `max_rects`, the excess (a suffix of sorted children) is grouped into
//!   "Other" and the hinted level uses the remaining budget.
//!
//! # Depth
//! Level 0: the node's children, with labels. Level 1 (only with [`Depth::Two`]): each level 0 directory's children,
//! hinted inside it.
//!
//! # Sizes
//! Uses `alloc` (size on disk). A child with `alloc == 0` occupies no area. The caller owns the key's `version` field and
//! changes it when layout-visible sizes change (a new tree and, in increment 3, the cleanup overlay version) to
//! invalidate the cache.

use crate::geom::Rect;
use crate::grid::Grid;
use crate::order::Children;
use crate::squarify::squarify;
use std::sync::OnceLock;
use tesela_model::{NodeId, Tree, View};

/// Layout depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Depth {
    /// Only the node's children.
    One,
    /// The children and, inside each directory, a hinted view of its children.
    Two,
}

/// What to lay out: visible children of `node` at positions `start..end` (all = `0..u32::MAX`; an opened "Other" group
/// may instead be a suffix or page).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Root {
    pub node: NodeId,
    pub start: u32,
    pub end: u32,
}

impl Root {
    /// All children of `node`.
    pub fn node(node: NodeId) -> Root {
        Root {
            node,
            start: 0,
            end: u32::MAX,
        }
    }

    /// Is this an opened "Other" group (rather than the whole directory)?
    pub fn is_group(&self) -> bool {
        self.start > 0 || self.end != u32::MAX
    }
}

/// Layout parameters. Included in the cache key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    /// A cell with area below `min_group_px²` is grouped into "Other" (24 px, PLAN.md §2.9).
    pub min_group_px: f32,
    /// At the hinted level, do not draw cells with any side smaller than this (3 px).
    pub lod_px: f32,
    /// Cell limit for the layout (15,000), counting main and hinted levels TOGETHER. Applied before laying out the main
    /// level (grouping excess into "Other"), then limits the hinted level.
    pub max_rects: usize,
    /// Height of the label strip (name and size) inside a labeled cell.
    pub header_px: f32,
    /// Margin between a cell's edge and the cells it hints at.
    pub pad_px: f32,
    /// A cell at least this wide and high gets a label.
    pub label_min_w: f32,
    pub label_min_h: f32,
}

impl Default for Params {
    fn default() -> Params {
        Params {
            min_group_px: 24.0,
            lod_px: 3.0,
            max_rects: 15_000,
            header_px: 38.0,
            pad_px: 6.0,
            label_min_w: 64.0,
            label_min_h: 40.0,
        }
    }
}

impl Params {
    /// Does the label (name and size) fit in a cell of this rectangle?
    pub fn label_fits(&self, r: &Rect) -> bool {
        r.w >= self.label_min_w && r.h >= self.label_min_h
    }

    /// Interior rectangle where a cell's children are hinted: below the label if it fits, otherwise almost the whole cell.
    /// `None` if it is too small to draw anything.
    pub fn inner_rect(&self, r: &Rect) -> Option<Rect> {
        let inner = if self.label_fits(r) {
            Rect::new(
                r.x + self.pad_px,
                r.y + self.header_px,
                r.w - 2.0 * self.pad_px,
                r.h - self.header_px - self.pad_px,
            )
        } else {
            r.inset(2.0)
        };
        (inner.w >= 2.0 * self.lod_px && inner.h >= 2.0 * self.lod_px).then_some(inner)
    }
}

/// Cache key: `(root, viewport, depth, version)` plus parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub root: Root,
    /// Viewport size in integer logical pixels (the renderer chooses the origin).
    pub size: (u32, u32),
    pub depth: Depth,
    /// Data version (tree and overlay), set by the caller.
    pub version: u64,
    pub params: Params,
}

/// What a cell represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    /// A tree node.
    Node(NodeId),
    /// The "Other" group; the index refers to [`Layout::others`].
    Others(u32),
}

/// A map cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    pub rect: Rect,
    pub kind: CellKind,
    /// 0 = labeled; 1 = hinted inside a level 0 cell.
    pub level: u8,
    /// Index of the level 0 cell containing it ([`NO_CELL`] at level 0).
    pub parent: u32,
}

/// Value of [`Cell::parent`] for level 0 cells.
pub const NO_CELL: u32 = u32::MAX;

/// "Other" group: visible children `start..start + count` of `parent` (a suffix or page).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Others {
    pub parent: NodeId,
    pub start: u32,
    pub count: u32,
    /// Sum of `alloc` for grouped children.
    pub bytes: u64,
}

impl Others {
    /// Root that opens this group.
    pub fn root(&self) -> Root {
        Root {
            node: self.parent,
            start: self.start,
            end: self.start + self.count,
        }
    }
}

/// Calculation statistics (for tests and performance reports).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutStats {
    /// Level 0 cells.
    pub level0: usize,
    /// Level 1 cells.
    pub level1: usize,
    /// Items grouped into "Other".
    pub grouped: usize,
    /// `max_rects` was reached and some cells were not created.
    pub truncated: bool,
}

/// Layout result: cells, groups, and hit-test grid (built on first use).
#[derive(Debug)]
pub struct Layout {
    key: Key,
    cells: Vec<Cell>,
    others: Vec<Others>,
    stats: LayoutStats,
    grid: OnceLock<Grid>,
}

impl Layout {
    /// Calculate the layout for `key` on `tree` (without a cleanup overlay).
    pub fn compute(tree: &Tree, key: Key) -> Layout {
        Layout::compute_view(View::new(tree, None), key)
    }

    /// Calculate the layout for `key` on a tree view (with its cleanup overlay, if any): sizes are visible sizes, and
    /// children of touched directories are those that remain.
    pub fn compute_view(view: View, key: Key) -> Layout {
        let (w, h) = (key.size.0 as f32, key.size.1 as f32);
        let mut cells: Vec<Cell> = Vec::new();
        let mut others: Vec<Others> = Vec::new();
        let mut stats = LayoutStats::default();
        let params = key.params;
        let viewport = Rect::new(0.0, 0.0, w, h);

        let kids = Children::of_view(&view, key.root.node);
        let end = (key.root.end as usize).min(kids.len());
        let start = (key.root.start as usize).min(end);
        let alloc_of = |i: usize| view.alloc(kids.get(i));
        let area = (w as f64) * (h as f64);
        let total: f64 = (start..end).map(|i| alloc_of(i) as f64).sum();

        if total > 0.0 && area > 0.0 {
            let scale = area / total;
            let min_area = (params.min_group_px as f64).powi(2);
            let n = end - start;
            // Children are sorted largest to smallest: those below `min_area` form a suffix (binary search).
            let big = start + partition(n, |i| alloc_of(start + i) as f64 * scale >= min_area);
            // Sized children: those with `alloc == 0` (at the end) have no area and are not laid out.
            let sized = start + partition(n, |i| alloc_of(start + i) > 0);
            // Level 0 entries before layout: (value, kind, first and last child covered).
            let mut items: Vec<(f64, CellKind, usize, usize)> = Vec::new();
            let add_group = |others: &mut Vec<Others>, first: usize, last: usize| {
                let bytes: u64 = (first..last).map(alloc_of).fold(0u64, u64::saturating_add);
                others.push(Others {
                    parent: key.root.node,
                    start: first as u32,
                    count: (last - first) as u32,
                    bytes,
                });
                (
                    bytes as f64,
                    CellKind::Others(others.len() as u32 - 1),
                    first,
                    last,
                )
            };
            // Pages for the "no child reaches the minimum area" case (below): `fit` children fit in the minimum area.
            let fit = ((area / min_area).floor() as usize).max(2);
            let page = fit.max((sized - start).div_ceil(fit));
            let none_fits = big == start && sized > start;
            // With a TINY viewport (less than ~34×34 px), `page` can cover the whole set: paging would produce one
            // "Other" identical to the root, and opening it would make no progress. Do not group; lay out every child
            // (even below 24×24): opening "Other" always reduces the set, or there is no "Other" to open.
            let single_page = none_fits && page >= sized - start;
            if none_fits && !single_page {
                // NO child reaches 24×24 px (many similarly sized children): grouping all would create an identical root
                // and "Other" would never make progress. Use PAGES: consecutive groups of `page` children, each an
                // "Other" group that lays out only its own items when opened. With `page >= area / min_area`, the largest
                // item on a page is at least the minimum area, so opening a page always shows items; since a page is
                // smaller than the whole set, opening "Other" always reduces the set.
                let mut pages: Vec<(usize, usize, u64)> = Vec::new();
                let mut s = start;
                while s < sized {
                    let e = s.saturating_add(page).min(sized);
                    let bytes: u64 = (s..e).map(alloc_of).fold(0u64, u64::saturating_add);
                    pages.push((s, e, bytes));
                    s = e;
                }
                // Pages go from largest to smallest (children are sorted): those below the minimum area are merged into
                // one final group (the first always qualifies: it has at least 1/page_count of the total).
                let big_pages =
                    partition(pages.len(), |p| pages[p].2 as f64 * scale >= min_area).max(1);
                for &(s, e, _) in &pages[..big_pages] {
                    items.push(add_group(&mut others, s, e));
                }
                if big_pages < pages.len() {
                    let (s, e) = (pages[big_pages].0, pages[pages.len() - 1].1);
                    items.push(add_group(&mut others, s, e));
                }
            } else {
                // The candidate "Other" suffix contains items below the minimum area, with or without size.
                let group_count = end - big;
                let group_bytes: u64 = (big..end).map(alloc_of).fold(0u64, u64::saturating_add);
                // A one-item group is pointless, as is a group with no size.
                let grouped = group_count >= 2 && group_bytes > 0 && !single_page;
                // Without a group, lay out every item with size (even if some are below 24×24 px).
                let prefix_end = if grouped { big } else { sized };
                for i in start..prefix_end {
                    items.push((alloc_of(i) as f64, CellKind::Node(kids.get(i)), i, i + 1));
                }
                if grouped {
                    items.push(add_group(&mut others, big, end));
                }
            }
            // BUDGET, before the first layout: the main level never exceeds `max_rects` cells. If it does, the excess
            // (a suffix, since children are sorted largest to smallest) is merged into ONE "Other" group, which lays out
            // only those items when opened (always fewer than the containing root).
            let cap = params.max_rects.max(2);
            if items.len() > cap {
                stats.truncated = true;
                let tail = cap - 1;
                let (first, last) = (items[tail].2, items[items.len() - 1].3);
                // Any groups left in the tail disappear (their children move into the new group).
                let keep_others = items[tail..]
                    .iter()
                    .find_map(|it| match it.1 {
                        CellKind::Others(i) => Some(i as usize),
                        CellKind::Node(_) => None,
                    })
                    .unwrap_or(others.len());
                others.truncate(keep_others);
                items.truncate(tail);
                items.push(add_group(&mut others, first, last));
            }
            stats.grouped = others.iter().map(|o| o.count as usize).sum();
            let vals: Vec<f64> = items.iter().map(|it| it.0).collect();
            let mut rects = Vec::with_capacity(vals.len());
            squarify(&vals, total, viewport, &mut rects);
            for (r, it) in rects.iter().zip(&items) {
                cells.push(Cell {
                    rect: *r,
                    kind: it.1,
                    level: 0,
                    parent: NO_CELL,
                });
            }
            stats.level0 = cells.len();

            if key.depth == Depth::Two {
                hint_level(view, &params, &mut cells, &mut stats);
            }
        }
        Layout {
            key,
            cells,
            others,
            stats,
            grid: OnceLock::new(),
        }
    }

    /// Key used to calculate the layout.
    pub fn key(&self) -> &Key {
        &self.key
    }

    /// All cells: level 0 first (largest to smallest), then hinted cells.
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// "Other" groups (cell [`CellKind::Others`]`(i)` is `others()[i]`).
    pub fn others(&self) -> &[Others] {
        &self.others
    }

    /// Calculation statistics.
    pub fn stats(&self) -> LayoutStats {
        self.stats
    }

    /// Deepest cell under point `(x, y)` (in layout space, with origin at the viewport corner).
    /// Uses the spatial grid, built on the first query.
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        let (w, h) = (self.key.size.0 as f32, self.key.size.1 as f32);
        let grid = self.grid.get_or_init(|| Grid::build(&self.cells, w, h));
        grid.hit(&self.cells, x, y).map(|i| i as usize)
    }

    /// Grid entries (0 if it has not been used yet).
    pub fn grid_entries(&self) -> usize {
        self.grid.get().map_or(0, Grid::entries)
    }

    /// Index of `node`'s level 0 cell, if it is in this layout.
    pub fn cell_of_node(&self, node: NodeId) -> Option<usize> {
        self.cells[..self.stats.level0]
            .iter()
            .position(|c| c.kind == CellKind::Node(node))
    }
}

/// First index `i` in `0..n` where `pred(i)` is false (`pred` must be true first, then false).
fn partition(n: usize, pred: impl Fn(usize) -> bool) -> usize {
    let (mut lo, mut hi) = (0usize, n);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if pred(mid) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Hinted level: children of each level 0 directory, largest to smallest (so the rectangle budget is spent first where
/// it is most visible).
fn hint_level(view: View, params: &Params, cells: &mut Vec<Cell>, stats: &mut LayoutStats) {
    let tree = view.tree();
    let level0 = cells.len();
    let lod_area = (params.lod_px as f64).powi(2);
    let mut rects: Vec<Rect> = Vec::new();
    let mut vals: Vec<f64> = Vec::new();
    for pi in 0..level0 {
        let CellKind::Node(node) = cells[pi].kind else {
            continue;
        };
        let total_alloc = view.alloc(node);
        if !tree.node(node).is_dir() || total_alloc == 0 {
            continue;
        }
        let kids = view.kids(node);
        if kids.is_empty() {
            continue;
        }
        let Some(inner) = params.inner_rect(&cells[pi].rect) else {
            continue;
        };
        if cells.len() >= params.max_rects {
            stats.truncated = true;
            break;
        }
        let scale = (inner.w as f64) * (inner.h as f64) / total_alloc as f64;
        // Children below the minimum drawing area are not created.
        let mut count = partition(kids.len(), |i| {
            view.alloc(kids.get(i)) as f64 * scale >= lod_area
        });
        let budget = params.max_rects - cells.len();
        if count > budget {
            count = budget;
            stats.truncated = true;
        }
        if count == 0 {
            continue;
        }
        vals.clear();
        vals.extend((0..count).map(|i| view.alloc(kids.get(i)) as f64));
        rects.clear();
        // `total` is the directory's `alloc`: any undistributed amount (the tiny tail and the directory's own space)
        // remains as a gap at the end, without changing the scale of cells that are drawn.
        squarify(&vals, total_alloc as f64, inner, &mut rects);
        for (i, r) in rects.iter().enumerate() {
            if r.w < params.lod_px || r.h < params.lod_px {
                continue;
            }
            cells.push(Cell {
                rect: *r,
                kind: CellKind::Node(kids.get(i)),
                level: 1,
                parent: pi as u32,
            });
            stats.level1 += 1;
        }
    }
}
