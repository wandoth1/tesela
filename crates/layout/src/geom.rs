//! Rectángulo en píxeles lógicos (f32). Origen arriba a la izquierda; el eje Y crece hacia abajo.

/// Rectángulo `[x, x + w) × [y, y + h)`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    /// Rectángulo con esquina y tamaño.
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    /// Borde derecho (exclusivo).
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    /// Borde inferior (exclusivo).
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    /// Área en px².
    pub fn area(&self) -> f32 {
        self.w * self.h
    }

    /// ¿Contiene el punto? (intervalo semiabierto: dos rectángulos contiguos no comparten ningún punto).
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }

    /// ¿Contiene `o` por completo (con una tolerancia `eps` para el redondeo)?
    pub fn contains_rect(&self, o: &Rect, eps: f32) -> bool {
        o.x >= self.x - eps
            && o.y >= self.y - eps
            && o.right() <= self.right() + eps
            && o.bottom() <= self.bottom() + eps
    }

    /// Superficie de intersección con `o` (0 si no se tocan).
    pub fn intersection_area(&self, o: &Rect) -> f32 {
        let w = self.right().min(o.right()) - self.x.max(o.x);
        let h = self.bottom().min(o.bottom()) - self.y.max(o.y);
        if w > 0.0 && h > 0.0 { w * h } else { 0.0 }
    }

    /// Rectángulo encogido `d` por cada lado (nunca con tamaño negativo).
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

    /// Desplazado `(dx, dy)`.
    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }
}
