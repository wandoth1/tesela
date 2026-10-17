//! Squarified treemap (Bruls, Huizing, and van Wijk, 2000).
//!
//! Since children already arrive sorted by descending size (guaranteed by `TreeBuilder`), the algorithm is linear: each
//! value is examined once while growing a row and once while placing it.

use crate::geom::Rect;

/// Lay out `values` (in the given order; descending is ideal) inside `rect`, so each rectangle's area is
/// `value * rect.area / total`. Append one rectangle per value to `out`, in the same order.
///
/// `total` may be GREATER than the sum of `values`: the remainder stays as a gap at the end (so a tail of tiny items can
/// be omitted without changing the scale of the rest). If the sum equals `total`, rectangles cover `rect` exactly (the
/// last rectangle in each row is adjusted to the edge to avoid rounding gaps).
///
/// Values should be positive: a zero or negative value receives an empty rectangle.
pub fn squarify(values: &[f64], total: f64, rect: Rect, out: &mut Vec<Rect>) {
    let start = out.len();
    let (x0, y0, w0, h0) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
    let area = w0 * h0;
    if values.is_empty() {
        return;
    }
    if !pos(total) || !pos(area) {
        out.extend(values.iter().map(|_| Rect::new(rect.x, rect.y, 0.0, 0.0)));
        return;
    }
    let sum: f64 = values.iter().filter(|v| **v > 0.0).sum();
    let exact = (total - sum).abs() <= 1e-9 * total;
    let scale = area / total;
    let (mut x, mut y, mut w, mut h) = (x0, y0, w0, h0);
    let n = values.len();
    let mut i = 0;
    while i < n {
        if !pos(values[i]) {
            out.push(Rect::new(x as f32, y as f32, 0.0, 0.0));
            i += 1;
            continue;
        }
        let short = w.min(h);
        if !pos(short) {
            // No room (due to rounding): the remaining items receive empty rectangles.
            out.extend((i..n).map(|_| Rect::new(x as f32, y as f32, 0.0, 0.0)));
            break;
        }
        // Grow the row while its worst aspect ratio does not get worse.
        let mut j = i;
        let (mut row_sum, mut worst_prev) = (0.0f64, f64::INFINITY);
        let (mut rmax, mut rmin) = (0.0f64, f64::INFINITY);
        while j < n && values[j] > 0.0 {
            let a = values[j] * scale;
            let s2 = row_sum + a;
            let (mx, mn) = (rmax.max(a), rmin.min(a));
            let worst = ((short * short * mx) / (s2 * s2)).max((s2 * s2) / (short * short * mn));
            if j > i && worst > worst_prev {
                break;
            }
            row_sum = s2;
            rmax = mx;
            rmin = mn;
            worst_prev = worst;
            j += 1;
        }
        let last_row = exact && (j == n || values[j] <= 0.0);
        if w >= h {
            // Vertical row against the left edge: its items stack from top to bottom.
            let thick = if last_row { w } else { (row_sum / h).min(w) };
            let mut cy = y;
            for (k, &v) in values.iter().enumerate().take(j).skip(i) {
                let a = v * scale;
                let len = if k + 1 == j && (last_row || exact) {
                    (y + h) - cy
                } else {
                    a / thick
                };
                out.push(Rect::new(
                    x as f32,
                    cy as f32,
                    thick as f32,
                    len.max(0.0) as f32,
                ));
                cy += len;
            }
            x += thick;
            w -= thick;
        } else {
            // Horizontal row against the top edge: its items go from left to right.
            let thick = if last_row { h } else { (row_sum / w).min(h) };
            let mut cx = x;
            for (k, &v) in values.iter().enumerate().take(j).skip(i) {
                let a = v * scale;
                let len = if k + 1 == j && (last_row || exact) {
                    (x + w) - cx
                } else {
                    a / thick
                };
                out.push(Rect::new(
                    cx as f32,
                    y as f32,
                    len.max(0.0) as f32,
                    thick as f32,
                ));
                cx += len;
            }
            y += thick;
            h -= thick;
        }
        i = j;
    }
    debug_assert_eq!(out.len() - start, values.len());
}

/// `v > 0` (also false for NaN), written as a function to avoid negating an `f64` comparison.
fn pos(v: f64) -> bool {
    v > 0.0
}
