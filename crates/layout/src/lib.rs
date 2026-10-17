//! Tesela treemap layout (PLAN.md §2.9 and §3.8). Pure: no GPU, I/O, or `unsafe`.
//!
//! * [`squarify`]: the squarified algorithm by Bruls et al., linear because children arrive sorted.
//! * [`Layout::compute`]: depth 2 (one labeled level and one hinted level), grouping "Other" for blocks smaller than
//!   24×24 px (a suffix of the sorted list that can be opened), and drawing LOD (nothing below ~3×3 px, capped at ~15,000 rectangles).
//! * [`Layout::hit`]: hit testing with a spatial grid.
//! * [`LayoutCache`]: cache keyed by `(root, viewport, depth, version)`.
//! * [`Children`]: a directory's children as shown by the map and list (with the system block placed by size and hidden
//!   when it weighs less than 1%).

mod cache;
mod geom;
mod grid;
mod layout;
mod order;
mod squarify;

pub use cache::LayoutCache;
pub use geom::Rect;
pub use layout::{Cell, CellKind, Depth, Key, Layout, LayoutStats, NO_CELL, Others, Params, Root};
pub use order::{Children, SYSTEM_BLOCK_MIN_PERCENT};
pub use squarify::squarify;

#[cfg(test)]
mod tests;
