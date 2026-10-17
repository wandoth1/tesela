//! Rectangle in logical pixels (`f32`). Origin at top left; the Y axis grows downward.

/// Rectangle `[x, x + w) × [y, y + h)`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    /// Rectangle from a corner and size.
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    /// Right edge (exclusive).
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    /// Bottom edge (exclusive).
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    /// Area in px².
    pub fn area(&self) -> f32 {
        self.w * self.h
    }

    /// Does it contain the point? (half-open interval: adjacent rectangles share no points).
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }

    /// Does it fully contain `o` (with tolerance `eps` for rounding)?
    pub fn contains_rect(&self, o: &Rect, eps: f32) -> bool {
        o.x >= self.x - eps
            && o.y >= self.y - eps
            && o.right() <= self.right() + eps
            && o.bottom() <= self.bottom() + eps
    }

    /// Intersection area with `o` (0 if they do not touch).
    pub fn intersection_area(&self, o: &Rect) -> f32 {
        let w = self.right().min(o.right()) - self.x.max(o.x);
        let h = self.bottom().min(o.bottom()) - self.y.max(o.y);
        if w > 0.0 && h > 0.0 { w * h } else { 0.0 }
    }

    /// Rectangle inset by `d` on each side (size is never negative).
    pub fn inset(&self, d: f32) -> Rect {
        let w = (self.w - 2.0 * d).max(0.0);
        let h = (self.h - 2.0 * d).max(0.0);
        Rect::new(
            self.x + (self.w - w) / 2.0,
            self.y + (self.h - h) / 2.0,
            w,
            h,
        )
    }

    /// Offset by `(dx, dy)`.
    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }
}
