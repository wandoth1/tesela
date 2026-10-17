//! Uniform spatial grid for hit testing: each cell stores indices of layout cells that touch it (CSR format: one vector
//! of offsets and one of indices). A query checks ONE grid cell.

use crate::geom::Rect;
use crate::layout::Cell;

/// Grid cell side length, in px.
const BUCKET: f32 = 32.0;

#[derive(Debug)]
pub(crate) struct Grid {
    cols: u32,
    rows: u32,
    offsets: Vec<u32>,
    items: Vec<u32>,
}

impl Grid {
    pub(crate) fn build(cells: &[Cell], width: f32, height: f32) -> Grid {
        let cols = ((width / BUCKET).ceil() as u32).max(1);
        let rows = ((height / BUCKET).ceil() as u32).max(1);
        let n = (cols as usize) * (rows as usize);
        let span = |r: &Rect| -> (u32, u32, u32, u32) {
            let c0 = ((r.x / BUCKET).floor().max(0.0) as u32).min(cols - 1);
            let r0 = ((r.y / BUCKET).floor().max(0.0) as u32).min(rows - 1);
            let c1 = (((r.right() / BUCKET).ceil() as u32).saturating_sub(1)).clamp(c0, cols - 1);
            let r1 = (((r.bottom() / BUCKET).ceil() as u32).saturating_sub(1)).clamp(r0, rows - 1);
            (c0, r0, c1, r1)
        };
        let mut offsets = vec![0u32; n + 1];
        for c in cells {
            if c.rect.w <= 0.0 || c.rect.h <= 0.0 {
                continue;
            }
            let (c0, r0, c1, r1) = span(&c.rect);
            for r in r0..=r1 {
                for col in c0..=c1 {
                    offsets[r as usize * cols as usize + col as usize + 1] += 1;
                }
            }
        }
        for i in 0..n {
            offsets[i + 1] += offsets[i];
        }
        let mut fill = offsets.clone();
        let mut items = vec![0u32; offsets[n] as usize];
        for (idx, c) in cells.iter().enumerate() {
            if c.rect.w <= 0.0 || c.rect.h <= 0.0 {
                continue;
            }
            let (c0, r0, c1, r1) = span(&c.rect);
            for r in r0..=r1 {
                for col in c0..=c1 {
                    let b = r as usize * cols as usize + col as usize;
                    items[fill[b] as usize] = idx as u32;
                    fill[b] += 1;
                }
            }
        }
        Grid {
            cols,
            rows,
            offsets,
            items,
        }
    }

    /// Deepest cell containing the point (highest index at the same level), or `None`.
    pub(crate) fn hit(&self, cells: &[Cell], x: f32, y: f32) -> Option<u32> {
        if !(x >= 0.0 && y >= 0.0) {
            return None;
        }
        let col = (x / BUCKET) as u32;
        let row = (y / BUCKET) as u32;
        if col >= self.cols || row >= self.rows {
            return None;
        }
        let b = row as usize * self.cols as usize + col as usize;
        let mut best: Option<u32> = None;
        for &i in &self.items[self.offsets[b] as usize..self.offsets[b + 1] as usize] {
            let c = &cells[i as usize];
            if c.rect.contains(x, y) && best.is_none_or(|j| c.level >= cells[j as usize].level) {
                best = Some(i);
            }
        }
        best
    }

    /// Total grid entries (for tests and cost documentation).
    pub(crate) fn entries(&self) -> usize {
        self.items.len()
    }
}
