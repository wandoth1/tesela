//! Icons: A subset of 30 of Fluent UI System Icons (MIT, © Microsoft; see `assets/icons/LICENSE`).
//!
//! The SVGs are saved as they come from the official repository. They are a single `<path>` filled with the orders
//! `M L H V C Z` absolute, so instead of relying on a full SVG rasterizer, a proprietary SVG rasterizer.
//! ~100 lines converts them to a transparency mask at the size in pixels requested by the screen (PerMonitorV2:
//! `puntos × pixels_per_point`) and searches it as a white texture that Egui dyes. Non-zero winding filling, coverage
//! exact horizontally and 8 vertical sub-lines. A test verifies that the 30 are interpreted and that the coverage of
//! known shapes (square, hole) is exact.

use egui::{Color32, ColorImage, Context, TextureHandle, TextureOptions, Vec2};
use std::collections::HashMap;

/// A subset icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    ArrowClockwise,
    ArrowUp,
    BinRecycle,
    BrightnessHigh,
    Cart,
    Checkmark,
    ChevronRight,
    Cloud,
    Copy,
    DarkTheme,
    DataTreemap,
    Delete,
    DeleteOff,
    Desktop,
    Dismiss,
    Document,
    Eye,
    Folder,
    FolderOpen,
    HardDrive,
    Home,
    Info,
    LocalLanguage,
    LockClosed,
    Open,
    Search,
    Settings,
    ShieldCheckmark,
    Stop,
    TextBulletList,
    Warning,
}

macro_rules! svg {
    ($name:literal) => {
        include_str!(concat!("../assets/icons/", $name, ".svg"))
    };
}

impl Icon {
    /// All icons.
    pub const ALL: [Icon; 31] = [
        Icon::ArrowClockwise,
        Icon::ArrowUp,
        Icon::BinRecycle,
        Icon::BrightnessHigh,
        Icon::Cart,
        Icon::Checkmark,
        Icon::ChevronRight,
        Icon::Cloud,
        Icon::Copy,
        Icon::DarkTheme,
        Icon::DataTreemap,
        Icon::Delete,
        Icon::DeleteOff,
        Icon::Desktop,
        Icon::Dismiss,
        Icon::Document,
        Icon::Eye,
        Icon::Folder,
        Icon::FolderOpen,
        Icon::HardDrive,
        Icon::Home,
        Icon::Info,
        Icon::LocalLanguage,
        Icon::LockClosed,
        Icon::Open,
        Icon::Search,
        Icon::Settings,
        Icon::ShieldCheckmark,
        Icon::Stop,
        Icon::TextBulletList,
        Icon::Warning,
    ];

    /// The original SVG.
    pub fn svg(self) -> &'static str {
        match self {
            Icon::ArrowClockwise => svg!("arrow_clockwise"),
            Icon::ArrowUp => svg!("arrow_up"),
            Icon::BinRecycle => svg!("bin_recycle"),
            Icon::BrightnessHigh => svg!("brightness_high"),
            Icon::Cart => svg!("cart"),
            Icon::Checkmark => svg!("checkmark"),
            Icon::ChevronRight => svg!("chevron_right"),
            Icon::Cloud => svg!("cloud"),
            Icon::Copy => svg!("copy"),
            Icon::DarkTheme => svg!("dark_theme"),
            Icon::DataTreemap => svg!("data_treemap"),
            Icon::Delete => svg!("delete"),
            Icon::DeleteOff => svg!("delete_off"),
            Icon::Desktop => svg!("desktop"),
            Icon::Dismiss => svg!("dismiss"),
            Icon::Document => svg!("document"),
            Icon::Eye => svg!("eye"),
            Icon::Folder => svg!("folder"),
            Icon::FolderOpen => svg!("folder_open"),
            Icon::HardDrive => svg!("hard_drive"),
            Icon::Home => svg!("home"),
            Icon::Info => svg!("info"),
            Icon::LocalLanguage => svg!("local_language"),
            Icon::LockClosed => svg!("lock_closed"),
            Icon::Open => svg!("open"),
            Icon::Search => svg!("search"),
            Icon::Settings => svg!("settings"),
            Icon::ShieldCheckmark => svg!("shield_checkmark"),
            Icon::Stop => svg!("stop"),
            Icon::TextBulletList => svg!("text_bullet_list"),
            Icon::Warning => svg!("warning"),
        }
    }
}

// --- `d` attribute interpreter and rasterizer --------------------------------------------------------------------

/// Line segment in the `viewBox` space.
type Edge = ((f32, f32), (f32, f32));

/// Extracts the value of the `d` attribute from the first `<path>` of the SVG.
pub fn path_data(svg: &str) -> Option<&str> {
    let p = svg.find("<path")?;
    let rest = &svg[p..];
    let d = rest.find(" d=\"")? + 4;
    let end = rest[d..].find('"')?;
    Some(&rest[d..d + end])
}

/// `viewBox` side of the Fluent SVGs (they are all 24×24).
pub const VIEWBOX: f32 = 24.0;

/// Converts `M L H V C Z` commands (absolute or relative) into segments, flattening Bezier curves.
/// Returns `Err` if another command appears (arcs, `S`, `Q`...): the included icons do not use them.
pub fn flatten(d: &str) -> Result<Vec<Edge>, String> {
    let bytes = d.as_bytes();
    let mut i = 0usize;
    let mut edges: Vec<Edge> = Vec::new();
    let (mut cx, mut cy) = (0f32, 0f32);
    let (mut sx, mut sy) = (0f32, 0f32);
    let mut cmd = b' ';

    fn skip_sep(b: &[u8], i: &mut usize) {
        while *i < b.len()
            && (b[*i] == b' '
                || b[*i] == b','
                || b[*i] == b'\n'
                || b[*i] == b'\t'
                || b[*i] == b'\r')
        {
            *i += 1;
        }
    }
    fn number(b: &[u8], i: &mut usize) -> Result<f32, String> {
        skip_sep(b, i);
        let start = *i;
        if *i < b.len() && (b[*i] == b'-' || b[*i] == b'+') {
            *i += 1;
        }
        let mut dot = false;
        while *i < b.len() && (b[*i].is_ascii_digit() || (b[*i] == b'.' && !dot)) {
            dot |= b[*i] == b'.';
            *i += 1;
        }
        if *i < b.len() && (b[*i] == b'e' || b[*i] == b'E') {
            *i += 1;
            if *i < b.len() && (b[*i] == b'-' || b[*i] == b'+') {
                *i += 1;
            }
            while *i < b.len() && b[*i].is_ascii_digit() {
                *i += 1;
            }
        }
        std::str::from_utf8(&b[start..*i])
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .ok_or_else(|| format!("número inválido en la posición {start}"))
    }
    fn at_number(b: &[u8], i: usize) -> bool {
        i < b.len() && (b[i].is_ascii_digit() || b[i] == b'-' || b[i] == b'+' || b[i] == b'.')
    }

    loop {
        skip_sep(bytes, &mut i);
        if i >= bytes.len() {
            break;
        }
        if bytes[i].is_ascii_alphabetic() {
            cmd = bytes[i];
            i += 1;
            if cmd == b'Z' || cmd == b'z' {
                if (cx, cy) != (sx, sy) {
                    edges.push(((cx, cy), (sx, sy)));
                }
                (cx, cy) = (sx, sy);
                continue;
            }
        } else if !at_number(bytes, i) {
            return Err(format!("carácter inesperado en la posición {i}"));
        }
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (cx, cy) } else { (0.0, 0.0) };
        match cmd.to_ascii_uppercase() {
            b'M' => {
                let (x, y) = (number(bytes, &mut i)? + ox, number(bytes, &mut i)? + oy);
                // Implicitly closes the previous subpath (fill always closes).
                if (cx, cy) != (sx, sy) {
                    edges.push(((cx, cy), (sx, sy)));
                }
                (cx, cy) = (x, y);
                (sx, sy) = (x, y);
                cmd = if rel { b'l' } else { b'L' }; // The following pairs are `L` commands.
            }
            b'L' => {
                let (x, y) = (number(bytes, &mut i)? + ox, number(bytes, &mut i)? + oy);
                edges.push(((cx, cy), (x, y)));
                (cx, cy) = (x, y);
            }
            b'H' => {
                let x = number(bytes, &mut i)? + ox;
                edges.push(((cx, cy), (x, cy)));
                cx = x;
            }
            b'V' => {
                let y = number(bytes, &mut i)? + if rel { cy } else { 0.0 };
                edges.push(((cx, cy), (cx, y)));
                cy = y;
            }
            b'C' => {
                let mut p = [(0f32, 0f32); 3];
                for q in &mut p {
                    *q = (number(bytes, &mut i)? + ox, number(bytes, &mut i)? + oy);
                }
                // 16 segments per curve: more than enough for icon curves of 24 units at 64 px or less.
                const N: usize = 16;
                let (p0, p1, p2, p3) = ((cx, cy), p[0], p[1], p[2]);
                let mut prev = p0;
                for k in 1..=N {
                    let t = k as f32 / N as f32;
                    let u = 1.0 - t;
                    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    let pt = (
                        a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0,
                        a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1,
                    );
                    edges.push((prev, pt));
                    prev = pt;
                }
                (cx, cy) = p3;
            }
            other => return Err(format!("orden de trazado no soportada: {}", other as char)),
        }
    }
    if (cx, cy) != (sx, sy) {
        edges.push(((cx, cy), (sx, sy)));
    }
    Ok(edges)
}

/// `size × size` coverage mask (0..=255) of some `viewBox` segments of 24 units, filled with
/// non-zero winding.
pub fn rasterize(edges: &[Edge], size: u32) -> Vec<u8> {
    const SUB: u32 = 8;
    let s = size as usize;
    let k = size as f32 / VIEWBOX;
    // Segments in pixels, without horizontal ones (they do not cross any sampling lines).
    let px: Vec<(f32, f32, f32, f32, i32)> = edges
        .iter()
        .filter(|((_, y0), (_, y1))| y0 != y1)
        .map(|&((x0, y0), (x1, y1))| {
            let (x0, y0, x1, y1) = (x0 * k, y0 * k, x1 * k, y1 * k);
            if y0 < y1 {
                (x0, y0, x1, y1, 1)
            } else {
                (x1, y1, x0, y0, -1)
            }
        })
        .collect();
    let mut acc = vec![0f32; s * s];
    let mut xs: Vec<(f32, i32)> = Vec::new();
    for row in 0..s {
        for sub in 0..SUB {
            let y = row as f32 + (sub as f32 + 0.5) / SUB as f32;
            xs.clear();
            for &(x0, y0, x1, y1, dir) in &px {
                if y >= y0 && y < y1 {
                    xs.push((x0 + (y - y0) * (x1 - x0) / (y1 - y0), dir));
                }
            }
            xs.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut wind = 0;
            for w in xs.windows(2) {
                wind += w[0].1;
                if wind != 0 {
                    add_span(&mut acc[row * s..(row + 1) * s], w[0].0, w[1].0);
                }
            }
        }
    }
    acc.iter()
        .map(|c| ((c / SUB as f32).clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect()
}

/// Adds the exact horizontal coverage of the interval `[a, b)` to the row.
fn add_span(row: &mut [f32], a: f32, b: f32) {
    let (a, b) = (a.max(0.0), b.min(row.len() as f32));
    if b <= a {
        return;
    }
    let (ia, ib) = (a.floor() as usize, (b.ceil() as usize).min(row.len()));
    for (x, px) in row.iter_mut().enumerate().take(ib).skip(ia) {
        let l = (x as f32).max(a);
        let r = ((x + 1) as f32).min(b);
        *px += (r - l).max(0.0);
    }
}

/// Icon mask to `size` px.
pub fn rasterize_icon(icon: Icon, size: u32) -> Vec<u8> {
    let d = path_data(icon.svg())
        .expect("todos los SVG incluidos tienen un <path d=...> (lo prueba un test)");
    let edges = flatten(d).expect("todos los SVG incluidos se interpretan (lo prueba un test)");
    rasterize(&edges, size)
}

// --- Texture cache ---------------------------------------------------------------------------------------------

/// Icon textures by (icon, size in pixels).
#[derive(Default)]
pub struct IconCache {
    textures: HashMap<(Icon, u32), TextureHandle>,
}

impl IconCache {
    /// Texture of the icon at `px` pixels from the side (white with the coverage as transparency: egui dyes it).
    pub fn texture(&mut self, ctx: &Context, icon: Icon, px: u32) -> &TextureHandle {
        let px = px.clamp(8, 256);
        self.textures.entry((icon, px)).or_insert_with(|| {
            let mask = rasterize_icon(icon, px);
            let pixels: Vec<Color32> = mask
                .iter()
                .map(|&a| Color32::from_rgba_unmultiplied(255, 255, 255, a))
                .collect();
            let img = ColorImage::new([px as usize, px as usize], pixels);
            ctx.load_texture(format!("icon-{icon:?}-{px}"), img, TextureOptions::LINEAR)
        })
    }

    /// Draws the `size` dotted icon on `rect` with color `tint`.
    pub fn paint(&mut self, painter: &egui::Painter, rect: egui::Rect, icon: Icon, tint: Color32) {
        let ppp = painter.ctx().pixels_per_point();
        let px = (rect.width().max(rect.height()) * ppp).round() as u32;
        let tex = self.texture(painter.ctx(), icon, px);
        painter.image(
            tex.id(),
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            tint,
        );
    }

    /// Adds the icon as a widget and returns its response.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        icon: Icon,
        size: f32,
        tint: Color32,
    ) -> egui::Response {
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
        if ui.is_rect_visible(rect) {
            self.paint(ui.painter(), rect, icon, tint);
        }
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coverage(mask: &[u8]) -> f64 {
        mask.iter().map(|&a| a as f64 / 255.0).sum::<f64>() / mask.len() as f64
    }

    #[test]
    fn un_cuadrado_cubre_exactamente_su_area() {
        // 12 × 12 of a 24 viewBox: the fourth part.
        let e = flatten("M6 6H18V18H6Z").unwrap();
        let m = rasterize(&e, 24);
        assert!((coverage(&m) - 0.25).abs() < 1e-3, "{}", coverage(&m));
        // At another scale the fraction is the same (the edges fall between pixels).
        let m = rasterize(&e, 48);
        assert!((coverage(&m) - 0.25).abs() < 1e-3);
        // With edges at half a pixel, partial coverage is also accurate horizontally.
        let e = flatten("M0.5 0H23.5V24H0.5Z").unwrap();
        assert!((coverage(&rasterize(&e, 24)) - 23.0 / 24.0).abs() < 1e-3);
    }

    #[test]
    fn el_devanado_inverso_hace_un_agujero_y_el_mismo_sentido_no() {
        // Frame: large square and small square in opposite direction = hole (non-zero winding).
        let hole = flatten("M0 0H24V24H0Z M6 6V18H18V6Z").unwrap();
        assert!((coverage(&rasterize(&hole, 24)) - 0.75).abs() < 1e-3);
        // Same sense: the interior is still stuffed.
        let solid = flatten("M0 0H24V24H0Z M6 6H18V18H6Z").unwrap();
        assert!((coverage(&rasterize(&solid, 24)) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn un_circulo_de_bezier_mide_pi_r2() {
        // Circle of radius 8 centered at (12, 12) with 4 curves (kappa = 0.5523).
        let k = 8.0 * 0.552_284_75;
        let d = format!(
            "M20 12C20 {a} {b} 20 12 20C{c} 20 4 {a} 4 12C4 {c} {c} 4 12 4C{b} 4 20 {c} 20 12Z",
            a = 12.0 + k,
            b = 12.0 + k,
            c = 12.0 - k
        );
        let m = rasterize(&flatten(&d).unwrap(), 48);
        let expect = std::f64::consts::PI * 64.0 / 576.0;
        assert!(
            (coverage(&m) - expect).abs() < 0.004,
            "{} frente a {expect}",
            coverage(&m)
        );
    }

    #[test]
    fn numeros_pegados_y_ordenes_relativas() {
        // "4.5-3" is two numbers; relative `l`.
        let e = flatten("M4.5-3l1.5.5").unwrap();
        assert_eq!(e.len(), 2); // One segment and the implicit close.
        assert_eq!(e[0], ((4.5, -3.0), (6.0, -2.5)));
        assert!(flatten("M0 0A1 1 0 0 1 2 2").is_err());
        assert!(flatten("M0 0Q1 1 2 2").is_err());
    }

    #[test]
    fn los_30_iconos_se_interpretan_y_tienen_una_cobertura_razonable() {
        for icon in Icon::ALL {
            let d = path_data(icon.svg()).unwrap_or_else(|| panic!("{icon:?}: sin <path d>"));
            let edges = flatten(d).unwrap_or_else(|e| panic!("{icon:?}: {e}"));
            assert!(!edges.is_empty());
            let m = rasterize(&edges, 24);
            let c = coverage(&m);
            assert!((0.02..0.6).contains(&c), "{icon:?}: cobertura {c}");
            // Inside the viewBox: no segment goes beyond 0..24 (with curve margin).
            for ((x0, y0), (x1, y1)) in &edges {
                for v in [x0, y0, x1, y1] {
                    assert!((-0.5..=24.5).contains(v), "{icon:?}: {v}");
                }
            }
        }
        // And the licenses go along with the icons.
        assert!(include_str!("../assets/icons/LICENSE").contains("MIT License"));
    }

    #[test]
    fn los_svg_son_los_de_fluent_con_un_unico_path_y_viewbox_24() {
        for icon in Icon::ALL {
            let s = icon.svg();
            assert!(s.contains("viewBox=\"0 0 24 24\""), "{icon:?}");
            assert_eq!(s.matches("<path").count(), 1, "{icon:?}");
        }
    }
}
