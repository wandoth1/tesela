//! El treemap: se dibuja como un `Mesh` teselado UNA vez y cacheado (PLAN.md §3.8, medido en la fase 0: ~10× menos
//! coste que una `RectShape` por celda). Esquinas de 4 px y separación de 2 px vía `Tessellator`. Las etiquetas, el
//! hover, la selección y la veladura de la búsqueda son overlays que se pintan cada frame sobre el mesh.
//!
//! El mesh se rehace solo si cambia la clave `(layout, origen, pixels_per_point, tema)`. El zoom animado de ~180 ms
//! (respetando `SPI_GETCLIENTAREAANIMATION`) transforma dos meshes ya hechos: no hay que volver a teselar.

use crate::app::Env;
use crate::args;
use crate::describe::{self, InfoKind, Special};
use crate::explore::{AddState, Explore, Sel};
use crate::icons::Icon;
use crate::platform;
use crate::theme::{self, Palette};
use crate::widgets;
use egui::epaint::{Mesh, TessellationOptions, Tessellator, Vertex};
use egui::{
    Color32, CornerRadius, FontId, Key, Modifiers, Pos2, Rect, Sense, Shape, Stroke, StrokeKind,
    Ui, pos2, vec2,
};
use std::sync::Arc;
use tesela_cleaner::paths::PathKey;
use tesela_cleaner::{Protection, Verdict};
use tesela_layout::{
    Cell, CellKind, Depth, Key as LayoutKey, Layout, LayoutCache, NO_CELL, Params, Root,
};
use tesela_model::{FileType, NodeId, Tree, View as TreeView, flags};

/// Duración del zoom animado.
const ZOOM_SECS: f64 = 0.18;

/// Cómo se pinta una celda.
#[derive(Clone, Copy, Debug)]
pub struct CellStyle {
    pub fill: Color32,
    pub text: Color32,
    pub special: Option<Special>,
    pub cloud: bool,
    pub lower_bound: bool,
    /// Ruta protegida (denegada, intocable o carpeta conocida): borde punteado y candado (PLAN.md §2.7).
    pub protected: Option<Verdict>,
}

#[derive(Clone, PartialEq)]
struct MeshKey {
    layout: LayoutKey,
    origin: (i32, i32),
    ppp_bits: u32,
    dark: bool,
}

struct MeshEntry {
    key: MeshKey,
    mesh: Arc<Mesh>,
    layout: Arc<Layout>,
    styles: Vec<CellStyle>,
    origin: Pos2,
}

enum Pending {
    /// Entrar en una celda: el mapa anterior es el exterior.
    In { outer: Arc<Mesh>, cell: Rect },
    /// Salir a la carpeta superior: el mapa anterior es el interior y su celda se busca en el layout nuevo.
    Out { inner: Arc<Mesh>, from: Root },
}

struct Anim {
    outer: Arc<Mesh>,
    inner: Arc<Mesh>,
    cell: Rect,
    map: Rect,
    start: f64,
    zoom_in: bool,
}

/// Cifras del dibujo (para los tests y las mediciones).
#[derive(Clone, Copy, Debug, Default)]
pub struct MapStats {
    pub cells: usize,
    pub vertices: usize,
    pub rebuilds: u64,
    pub zooms: u64,
}

/// Estado del mapa dentro de la exploración.
pub struct MapState {
    cache: LayoutCache,
    mesh: Option<MeshEntry>,
    pub(crate) hover: Option<usize>,
    pending: Option<Pending>,
    anim: Option<Anim>,
    last_rect: Option<Rect>,
    /// Layout del último frame (para localizar celdas desde fuera, p. ej. en los tests).
    pub(crate) layout: Option<Arc<Layout>>,
    pub(crate) stats: MapStats,
    /// Nodos de la carpeta actual que están en la cesta (con la huella con la que se calcularon).
    pub(crate) marks: (u64, Arc<std::collections::HashSet<NodeId>>),
}

impl Default for MapState {
    fn default() -> MapState {
        MapState {
            cache: LayoutCache::new(4),
            mesh: None,
            hover: None,
            pending: None,
            anim: None,
            last_rect: None,
            layout: None,
            stats: MapStats::default(),
            marks: (0, Arc::default()),
        }
    }
}

impl MapState {
    /// Prepara el zoom hacia dentro: el mapa que se ve ahora será el exterior que se agranda.
    pub(crate) fn begin_zoom_in(&mut self, animate: bool, sel: Sel) {
        self.pending = None;
        self.anim = None;
        if !animate {
            return;
        }
        let Some(e) = &self.mesh else { return };
        let idx = cell_of_sel(&e.layout, sel);
        if let Some(i) = idx {
            let cell = screen_rect(&e.layout.cells()[i].rect, e.origin);
            self.pending = Some(Pending::In {
                outer: e.mesh.clone(),
                cell,
            });
        }
    }

    /// Prepara el zoom hacia fuera desde la carpeta `from`.
    pub(crate) fn begin_zoom_out(&mut self, animate: bool, from: Root) {
        self.pending = None;
        self.anim = None;
        if !animate || from.is_group() {
            return;
        }
        if let Some(e) = &self.mesh {
            self.pending = Some(Pending::Out {
                inner: e.mesh.clone(),
                from,
            });
        }
    }

    /// ¿Hay un zoom animándose?
    pub fn animating(&self) -> bool {
        self.anim.is_some() || self.pending.is_some()
    }

    /// Cifras del último dibujo.
    pub fn stats(&self) -> MapStats {
        self.stats
    }
}

/// Selección correspondiente a una celda.
pub(crate) fn sel_of_cell(layout: &Layout, idx: usize) -> Sel {
    match layout.cells()[idx].kind {
        CellKind::Node(id) => Sel::Node(id),
        CellKind::Others(i) => Sel::Group(layout.others()[i as usize]),
    }
}

/// Celda (la más profunda) de una selección.
pub(crate) fn cell_of_sel(layout: &Layout, sel: Sel) -> Option<usize> {
    match sel {
        Sel::Node(id) => layout
            .cells()
            .iter()
            .rposition(|c| c.kind == CellKind::Node(id)),
        Sel::Group(o) => layout.cells().iter().position(
            |c| matches!(c.kind, CellKind::Others(i) if layout.others()[i as usize] == o),
        ),
    }
}

/// Rectángulo del layout (origen en la esquina del mapa) en coordenadas de pantalla.
fn screen_rect(r: &tesela_layout::Rect, origin: Pos2) -> Rect {
    Rect::from_min_size(origin + vec2(r.x, r.y), vec2(r.w, r.h))
}

// --- Estilo y mesh ---------------------------------------------------------------------------------------------------

/// Estilo de todas las celdas (los padres de nivel 0 van antes que sus hijas insinuadas), sin protección.
#[allow(dead_code)]
fn styles_for(layout: &Layout, tree: &Tree, pal: &Palette) -> Vec<CellStyle> {
    styles_with(layout, tree, pal, None)
}

/// Como [`styles_for`] marcando las celdas de nivel 0 cuya ruta está protegida.
fn styles_with(
    layout: &Layout,
    tree: &Tree,
    pal: &Palette,
    prot: Option<&Protection>,
) -> Vec<CellStyle> {
    let mut out: Vec<CellStyle> = Vec::with_capacity(layout.cells().len());
    for c in layout.cells() {
        let st = match c.kind {
            CellKind::Others(_) => {
                let fill = pal.others();
                CellStyle {
                    fill,
                    text: theme::text_on(pal, fill),
                    special: None,
                    cloud: false,
                    lower_bound: false,
                    protected: None,
                }
            }
            CellKind::Node(id) => {
                let n = tree.node(id);
                let special = describe::special(tree, id);
                let base = match special {
                    Some(Special::SystemBlock) => pal.system_block(),
                    Some(Special::InfoOnly(_)) => pal.info_only(),
                    None if n.is_dir() => match tree.hist(id).dominant_type() {
                        Some(t) => pal.type_color(t),
                        None if tree.node(id).alloc == 0
                            || tree.hist(id).type_pct.iter().all(|&p| p == 0) =>
                        {
                            pal.others()
                        }
                        None => pal.mixed(),
                    },
                    None => pal.type_color(FileType::classify(tree.name_bytes(id))),
                };
                let fill = if c.level == 1 && c.parent != NO_CELL {
                    // Insinuada: el color del padre aclarado.
                    let p = out[c.parent as usize].fill;
                    theme::mix(p, Color32::WHITE, if pal.dark { 0.16 } else { 0.24 })
                } else {
                    base
                };
                // Solo las celdas con etiqueta (nivel 0): construir la ruta de cada hija insinuada no compensa.
                let protected = match (prot, c.level, special) {
                    (Some(p), 0, None) => match p.target_verdict(&PathKey::new(&tree.path(id))) {
                        Verdict::Allowed => None,
                        v => Some(v),
                    },
                    _ => None,
                };
                CellStyle {
                    fill,
                    text: theme::text_on(pal, fill),
                    special,
                    cloud: n.flags & flags::CLOUD != 0,
                    lower_bound: n.flags & (flags::INCOMPLETE | flags::INCOMPLETE_BELOW) != 0,
                    protected,
                }
            }
        };
        out.push(st);
    }
    out
}

/// Tesela todas las celdas en un único `Mesh`: esquinas de 4 px, separación de 2 px (1 px por lado), rayas
/// diagonales tenues en las carpetas incompletas y la franja de los 3 tipos principales en las "Mixto".
fn build_mesh(
    layout: &Layout,
    tree: &Tree,
    styles: &[CellStyle],
    pal: &Palette,
    ppp: f32,
    origin: Pos2,
) -> Mesh {
    let mut tess = Tessellator::new(ppp, TessellationOptions::default(), [1, 1], vec![]);
    let mut mesh = Mesh::default();
    mesh.reserve_triangles(layout.cells().len() * 6);
    mesh.reserve_vertices(layout.cells().len() * 8);
    for (c, st) in layout.cells().iter().zip(styles) {
        let r = Rect::from_min_size(origin + vec2(c.rect.x, c.rect.y), vec2(c.rect.w, c.rect.h))
            .shrink(1.0);
        if r.width() < 1.0 || r.height() < 1.0 {
            continue;
        }
        // Esquinas de 4 px; en las celdas de menos de 16 px (la inmensa mayoría en un mapa denso) rectángulos planos, que
        // tienen la tercera parte de vértices y no se nota a ese tamaño.
        let corner = if r.width().min(r.height()) >= 16.0 {
            CornerRadius::same(4)
        } else {
            CornerRadius::ZERO
        };
        if st.special == Some(Special::SystemBlock) {
            tess.tessellate_shape(
                Shape::Rect(egui::epaint::RectShape::new(
                    r,
                    corner,
                    st.fill,
                    Stroke::new(1.0, pal.system_border()),
                    StrokeKind::Inside,
                )),
                &mut mesh,
            );
        } else {
            tess.tessellate_shape(Shape::rect_filled(r, corner, st.fill), &mut mesh);
        }
        if c.level != 0 {
            continue;
        }
        // Rayas diagonales tenues en lo incompleto ("≥"): la única trama activa por defecto.
        if st.lower_bound && r.width() >= 14.0 && r.height() >= 14.0 {
            let col = if theme::luminance(st.fill) > 0.35 {
                Color32::from_rgba_unmultiplied(0, 0, 0, 34)
            } else {
                Color32::from_rgba_unmultiplied(255, 255, 255, 46)
            };
            for (a, b) in widgets::hatch_segments(r.shrink(2.0), 9.0) {
                tess.tessellate_shape(
                    Shape::line_segment([a, b], Stroke::new(1.5, col)),
                    &mut mesh,
                );
            }
        }
        // Ruta protegida: borde punteado (el candado se pinta con la etiqueta).
        if st.protected.is_some() && r.width() >= 14.0 && r.height() >= 14.0 {
            let col = if theme::luminance(st.fill) > 0.35 {
                Color32::from_rgba_unmultiplied(0, 0, 0, 150)
            } else {
                Color32::from_rgba_unmultiplied(255, 255, 255, 190)
            };
            for (a, b) in dotted_rect_segments(r.shrink(3.0), 4.0, 3.0) {
                tess.tessellate_shape(
                    Shape::line_segment([a, b], Stroke::new(1.5, col)),
                    &mut mesh,
                );
            }
        }
        // Franja inferior de 4 px con los 3 tipos principales de una carpeta "Mixto" de al menos 48 px.
        if let CellKind::Node(id) = c.kind {
            let n = tree.node(id);
            if n.is_dir()
                && st.special.is_none()
                && tree.hist(id).dominant_type().is_none()
                && r.width() >= 48.0
                && r.height() >= 48.0
            {
                let top = describe::top_types(tree.hist(id), 3);
                let sum: f32 = top.iter().map(|t| t.1 as f32).sum();
                if sum > 0.0 {
                    let mut x = r.left();
                    for (i, (t, p)) in top.iter().enumerate() {
                        let w = if i + 1 == top.len() {
                            r.right() - x
                        } else {
                            r.width() * *p as f32 / sum
                        };
                        let strip = Rect::from_min_size(pos2(x, r.bottom() - 4.0), vec2(w, 4.0));
                        tess.tessellate_shape(
                            Shape::rect_filled(strip, CornerRadius::ZERO, pal.type_color(*t)),
                            &mut mesh,
                        );
                        x += w;
                    }
                }
            }
        }
    }
    mesh
}

/// Recalcula qué nodos de las celdas de nivel 0 están en la cesta, si cambió la cesta, la carpeta o el árbol.
fn refresh_marks(ex: &mut Explore, layout: &Layout) {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for s in ex.flow.basket.sources() {
        format!("{s:?}").hash(&mut h);
    }
    (ex.version, ex.root.node, ex.root.start, ex.root.end).hash(&mut h);
    let stamp = h.finish() | 1;
    if ex.map.marks.0 == stamp {
        return;
    }
    let mut set = std::collections::HashSet::new();
    if !ex.flow.basket.is_empty() {
        let cat_nodes: Vec<NodeId> = ex
            .recs
            .as_ref()
            .map(|(_, r)| {
                r.rows
                    .iter()
                    .filter(|row| ex.flow.basket.contains_category(row.id))
                    .flat_map(|row| row.parts.iter().map(|p| p.node))
                    .collect()
            })
            .unwrap_or_default();
        for id in level0_nodes(layout) {
            let key = PathKey::new(&ex.tree.path(id));
            if ex.flow.basket.covers(&key)
                || cat_nodes.contains(&id)
                || ex.flow.basket.has_contents_of(&key)
            {
                set.insert(id);
            }
        }
    }
    ex.map.marks = (stamp, Arc::new(set));
}

/// Segmentos que dibujan el contorno de `r` punteado: rayitas de `dash` px separadas `gap` px.
pub(crate) fn dotted_rect_segments(r: Rect, dash: f32, gap: f32) -> Vec<(Pos2, Pos2)> {
    let mut out = Vec::new();
    let mut edge = |a: Pos2, b: Pos2| {
        let len = (b - a).length();
        if len <= 0.0 {
            return;
        }
        let dir = (b - a) / len;
        let mut t = 0.0;
        while t < len {
            let e = (t + dash).min(len);
            out.push((a + dir * t, a + dir * e));
            t += dash + gap;
        }
    };
    edge(r.left_top(), r.right_top());
    edge(r.right_top(), r.right_bottom());
    edge(r.right_bottom(), r.left_bottom());
    edge(r.left_bottom(), r.left_top());
    out
}

/// Copia del mesh con los vértices transformados (de `from` a `to`) y la opacidad multiplicada por `alpha`.
fn transformed(mesh: &Mesh, from: Rect, to: Rect, alpha: f32) -> Mesh {
    let sx = to.width() / from.width().max(1e-3);
    let sy = to.height() / from.height().max(1e-3);
    Mesh {
        texture_id: mesh.texture_id,
        indices: mesh.indices.clone(),
        vertices: mesh
            .vertices
            .iter()
            .map(|v| Vertex {
                pos: pos2(
                    to.min.x + (v.pos.x - from.min.x) * sx,
                    to.min.y + (v.pos.y - from.min.y) * sy,
                ),
                uv: v.uv,
                color: v.color.gamma_multiply(alpha),
            })
            .collect(),
    }
}

fn lerp_rect(a: Rect, b: Rect, t: f32) -> Rect {
    Rect::from_min_max(a.min.lerp(b.min, t), a.max.lerp(b.max, t))
}

// --- Dibujo cada frame -----------------------------------------------------------------------------------------------

/// Etiqueta de una celda de nivel 0 (nombre y tamaño) y de las insinuadas que quepan.
fn draw_labels(
    painter: &egui::Painter,
    env: &mut Env,
    ex: &Explore,
    e: &MeshEntry,
    params: &Params,
) {
    let tree = &ex.tree;
    let view = ex.view();
    let tr = env.tr;
    let mut hint_labels = 0usize;
    for (c, st) in e.layout.cells().iter().zip(&e.styles) {
        let rect = Rect::from_min_size(
            e.origin + vec2(c.rect.x, c.rect.y),
            vec2(c.rect.w, c.rect.h),
        );
        match c.level {
            0 => {
                let narrow_others = matches!(c.kind, CellKind::Others(_)) && c.rect.w < 130.0;
                if narrow_others || !params.label_fits(&c.rect) {
                    // "Otros" es una tira estrecha con frecuencia: su etiqueta se parte en líneas en vez de recortarse u omitirse.
                    if let CellKind::Others(i) = c.kind
                        && c.rect.w >= 36.0
                        && c.rect.h >= 56.0
                    {
                        let o = e.layout.others()[i as usize];
                        let text = tr.ta(
                            "others-title",
                            args!["n" => tr.int(o.count as u64), "size" => tr.size(o.bytes)],
                        );
                        let mut job = egui::text::LayoutJob::single_section(
                            text,
                            egui::TextFormat::simple(theme::font_semibold(12.0), st.text),
                        );
                        job.wrap.max_width = (rect.width() - 10.0).max(1.0);
                        job.wrap.max_rows = 5;
                        job.wrap.break_anywhere = false;
                        job.wrap.overflow_character = Some('…');
                        let galley = painter.layout_job(job);
                        painter.galley(pos2(rect.left() + 6.0, rect.top() + 8.0), galley, st.text);
                    }
                    continue;
                }
                let (name, bytes, lower) = match c.kind {
                    CellKind::Node(id) => (
                        describe::display_name(tree, id, tr),
                        view.alloc(id),
                        st.lower_bound,
                    ),
                    CellKind::Others(i) => {
                        let o = e.layout.others()[i as usize];
                        (
                            tr.ta(
                                "others-title",
                                args!["n" => tr.int(o.count as u64), "size" => tr.size(o.bytes)],
                            ),
                            o.bytes,
                            false,
                        )
                    }
                };
                let x = rect.left() + 12.0;
                let max_w = rect.width() - 24.0;
                let icon = match (st.special, st.cloud, st.protected) {
                    (Some(Special::SystemBlock), _, _) => Some(Icon::Info),
                    (Some(Special::InfoOnly(_)), _, _) => Some(Icon::Warning),
                    (None, _, Some(_)) => Some(Icon::LockClosed),
                    (None, true, None) => Some(Icon::Cloud),
                    _ => None,
                };
                let icon_w = if icon.is_some() { 20.0 } else { 0.0 };
                let r = widgets::clipped_text(
                    painter,
                    pos2(x, rect.top() + 9.0),
                    egui::Align2::LEFT_TOP,
                    &name,
                    theme::font_semibold(14.0),
                    st.text,
                    max_w - icon_w,
                );
                if let Some(ic) = icon {
                    env.icons.paint(
                        painter,
                        Rect::from_min_size(
                            pos2(r.right() + 6.0, rect.top() + 9.0),
                            vec2(16.0, 16.0),
                        ),
                        ic,
                        st.text,
                    );
                }
                if !matches!(c.kind, CellKind::Others(_)) || c.rect.h >= 56.0 {
                    let size_txt = if lower {
                        tr.size_min(bytes)
                    } else {
                        tr.size(bytes)
                    };
                    widgets::clipped_text(
                        painter,
                        pos2(x, rect.top() + 25.0),
                        egui::Align2::LEFT_TOP,
                        &size_txt,
                        FontId::proportional(13.0),
                        st.text,
                        max_w,
                    );
                }
            }
            _ => {
                // Insinuadas: una línea "Nombre · tamaño" si cabe, y no más de unas decenas.
                if hint_labels >= 60 || c.rect.w < 72.0 || c.rect.h < 22.0 {
                    continue;
                }
                if let CellKind::Node(id) = c.kind {
                    hint_labels += 1;
                    let bytes = if st.lower_bound {
                        tr.size_min(view.alloc(id))
                    } else {
                        tr.size(view.alloc(id))
                    };
                    let txt = format!("{} · {}", describe::display_name(tree, id, tr), bytes);
                    widgets::clipped_text(
                        painter,
                        pos2(rect.left() + 6.0, rect.top() + 4.0),
                        egui::Align2::LEFT_TOP,
                        &txt,
                        FontId::proportional(12.0),
                        st.text,
                        rect.width() - 12.0,
                    );
                }
            }
        }
    }
}

/// Texto accesible del mapa: dónde está y qué hay seleccionado.
fn map_label(ex: &Explore, env: &Env) -> String {
    let tr = env.tr;
    let tree = &ex.tree;
    let shown = ex.shown_children();
    let mut s = tr.ta(
        "map-label",
        args!["name" => describe::display_name(tree, ex.root.node, tr), "n" => tr.int(shown.len() as u64)],
    );
    // La agrupación "Otros" no tiene widget propio: se anuncia aquí para quien no ve el mapa.
    if let Some(l) = &ex.map.layout {
        for o in l.others() {
            s.push(' ');
            s.push_str(&tr.ta(
                "map-has-others",
                args!["label" => tr.ta("others-title", args!["n" => tr.int(o.count as u64), "size" => tr.size(o.bytes)])],
            ));
        }
    }
    match ex.selection {
        Some(Sel::Node(id)) => {
            let size = if describe::is_lower_bound(tree, id) {
                tr.size_min(ex.view().alloc(id))
            } else {
                tr.size(ex.view().alloc(id))
            };
            s.push(' ');
            s.push_str(&tr.ta(
                "map-selected",
                args!["name" => describe::display_name(tree, id, tr), "size" => size],
            ));
        }
        Some(Sel::Group(o)) => {
            s.push(' ');
            s.push_str(&tr.ta(
                "others-title",
                args!["n" => tr.int(o.count as u64), "size" => tr.size(o.bytes)],
            ));
        }
        None => {}
    }
    s
}

/// El mapa. Dibuja en todo el espacio disponible.
pub fn show(ui: &mut Ui, env: &mut Env, ex: &mut Explore) {
    let pal = env.pal;
    let avail = ui.available_rect_before_wrap();
    let rect = Rect::from_min_size(
        pos2(avail.min.x.round(), avail.min.y.round()),
        vec2(avail.width().floor(), avail.height().floor()),
    );
    let size = (rect.width().max(0.0) as u32, rect.height().max(0.0) as u32);
    let resp = ui.allocate_rect(rect, Sense::click());
    if size.0 < 40 || size.1 < 40 {
        return;
    }
    let ctx = ui.ctx().clone();
    let ppp = ctx.pixels_per_point();
    let params = Params::default();
    let lkey = LayoutKey {
        root: ex.root,
        size,
        depth: Depth::Two,
        version: ex.version,
        params,
    };
    let layout = ex
        .map
        .cache
        .get_view(TreeView::new(&ex.tree, ex.overlay.as_deref()), lkey);
    let mkey = MeshKey {
        layout: lkey,
        origin: (rect.min.x as i32, rect.min.y as i32),
        ppp_bits: ppp.to_bits(),
        dark: pal.dark,
    };
    if ex.map.mesh.as_ref().is_none_or(|m| m.key != mkey) {
        let styles = styles_with(&layout, &ex.tree, &pal, Some(&ex.protection));
        let mesh = build_mesh(&layout, &ex.tree, &styles, &pal, ppp, rect.min);
        ex.map.stats.rebuilds += 1;
        ex.map.stats.cells = layout.cells().len();
        ex.map.stats.vertices = mesh.vertices.len();
        ex.map.mesh = Some(MeshEntry {
            key: mkey,
            mesh: Arc::new(mesh),
            layout: layout.clone(),
            styles,
            origin: rect.min,
        });
    }
    ex.map.layout = Some(layout.clone());
    ex.map.last_rect = Some(rect);
    refresh_marks(ex, &layout);

    // Zoom pendiente: se convierte en animación en cuanto hay mesh nuevo.
    let now = ctx.input(|i| i.time);
    if let Some(p) = ex.map.pending.take() {
        let cur = ex.map.mesh.as_ref().map(|e| e.mesh.clone());
        match (p, cur) {
            (Pending::In { outer, cell }, Some(inner)) => {
                ex.map.anim = Some(Anim {
                    outer,
                    inner,
                    cell,
                    map: rect,
                    start: now,
                    zoom_in: true,
                });
            }
            (Pending::Out { inner, from }, Some(outer)) => {
                if let Some(i) = layout.cell_of_node(from.node) {
                    let cell = screen_rect(&layout.cells()[i].rect, rect.min);
                    ex.map.anim = Some(Anim {
                        outer,
                        inner,
                        cell,
                        map: rect,
                        start: now,
                        zoom_in: false,
                    });
                }
            }
            _ => {}
        }
        if ex.map.anim.is_some() {
            ex.map.stats.zooms += 1;
        }
    }

    let painter = ui.painter_at(rect);
    let entry = ex.map.mesh.as_ref().expect("mesh recién creado");
    painter.rect_filled(rect, CornerRadius::same(4), pal.bg);

    // --- Animación o mapa estático ---
    let mut animating = false;
    if let Some(a) = &ex.map.anim {
        let t = ((now - a.start) / ZOOM_SECS).clamp(0.0, 1.0) as f32;
        if t >= 1.0 {
            ex.map.anim = None;
        } else {
            animating = true;
            let e = 1.0 - (1.0 - t).powi(3);
            let e_in = if a.zoom_in { e } else { 1.0 - e };
            let rect_t = lerp_rect(a.cell, a.map, e_in);
            painter.add(Shape::mesh(Arc::new(transformed(
                &a.outer,
                a.cell,
                rect_t,
                1.0 - e_in,
            ))));
            painter.add(Shape::mesh(Arc::new(transformed(
                &a.inner, a.map, rect_t, e_in,
            ))));
            ctx.request_repaint();
        }
    }
    if !animating {
        painter.add(Shape::mesh(entry.mesh.clone()));
    }

    // --- Interacción ---
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Other, true, map_label(ex, env))
    });
    let pointer = ctx.input(|i| i.pointer.hover_pos());
    let entry = ex.map.mesh.as_ref().expect("mesh");
    let hovered = if resp.hovered() && !animating {
        pointer.and_then(|p| layout.hit(p.x - rect.min.x, p.y - rect.min.y))
    } else {
        None
    };
    ex.map.hover = hovered;

    // Un clic de verdad (con puntero): Intro sobre el mapa enfocado también cuenta como `clicked()` y no debe borrar la
    // selección.
    let pointer_click =
        (resp.clicked() || resp.secondary_clicked()) && resp.interact_pointer_pos().is_some();
    if pointer_click {
        resp.request_focus();
        ex.selection = hovered.map(|i| sel_of_cell(&layout, i));
        ex.scroll_to_selection = true;
    }
    if resp.secondary_clicked() {
        ex.ctx_target = ex.selection.or(Some(Sel::Node(ex.root.node)));
    }
    if resp.double_clicked()
        && let Some(i) = hovered
    {
        let sel = sel_of_cell(&layout, i);
        ex.enter(sel);
        return;
    }
    if resp.has_focus() {
        ex.nav_focus = Some(resp.id);
    }
    if resp.has_focus() && !animating {
        let dir = ctx.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowLeft) {
                Some((-1.0, 0.0))
            } else if i.consume_key(Modifiers::NONE, Key::ArrowRight) {
                Some((1.0, 0.0))
            } else if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                Some((0.0, -1.0))
            } else if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                Some((0.0, 1.0))
            } else {
                None
            }
        });
        if let Some((dx, dy)) = dir {
            let cur = ex.selection.and_then(|s| cell_of_sel(&layout, s));
            if let Some(i) = neighbor(&layout, cur, dx, dy) {
                ex.selection = Some(sel_of_cell(&layout, i));
                ex.scroll_to_selection = true;
            }
        }
    }

    // --- Overlays ---
    if !animating {
        draw_labels(&painter, env, ex, entry, &params);
        // Lo que está en la cesta lleva una insignia con el carrito.
        for c in layout.cells().iter().filter(|c| c.level == 0) {
            if let CellKind::Node(id) = c.kind
                && ex.map.marks.1.contains(&id)
                && c.rect.w >= 30.0
                && c.rect.h >= 30.0
            {
                let center = rect.min + vec2(c.rect.x + c.rect.w - 16.0, c.rect.y + 16.0);
                painter.circle_filled(center, 10.0, pal.accent);
                env.icons.paint(
                    &painter,
                    Rect::from_center_size(center, vec2(13.0, 13.0)),
                    Icon::Cart,
                    Color32::WHITE,
                );
            }
        }
        // Veladura de la búsqueda.
        if let Some(hits) = ex.search_set() {
            let veil = pal.bg.gamma_multiply(0.72);
            for c in layout.cells().iter().filter(|c| c.level == 0) {
                let matches = match c.kind {
                    CellKind::Node(id) => hits.contains(&id),
                    CellKind::Others(_) => false,
                };
                if !matches {
                    let r = Rect::from_min_size(
                        rect.min + vec2(c.rect.x, c.rect.y),
                        vec2(c.rect.w, c.rect.h),
                    )
                    .shrink(1.0);
                    painter.rect_filled(r, CornerRadius::same(4), veil);
                }
            }
        }
        if let Some(sel) = ex.selection
            && let Some(i) = cell_of_sel(&layout, sel)
        {
            let r = layout.cells()[i].rect;
            let r = Rect::from_min_size(rect.min + vec2(r.x, r.y), vec2(r.w, r.h)).shrink(1.0);
            painter.rect_stroke(
                r.expand(1.0),
                CornerRadius::same(5),
                Stroke::new(3.0, pal.accent),
                StrokeKind::Outside,
            );
        }
        if let Some(i) = hovered {
            let r = layout.cells()[i].rect;
            let r = Rect::from_min_size(rect.min + vec2(r.x, r.y), vec2(r.w, r.h)).shrink(1.0);
            painter.rect_stroke(
                r,
                CornerRadius::same(4),
                Stroke::new(2.0, pal.text.gamma_multiply(0.85)),
                StrokeKind::Inside,
            );
            egui::Tooltip::for_widget(&resp)
                .at_pointer()
                .show(|ui| tooltip(ui, env, ex, &layout.cells()[i], &layout, &pal));
        }
    }
    if resp.has_focus() {
        painter.rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(2.0, pal.accent),
            StrokeKind::Inside,
        );
    }
    // Menú contextual.
    let target = ex.ctx_target;
    egui::Popup::context_menu(&resp).show(|ui| {
        if let Some(t) = target {
            menu_items(ui, env, ex, t);
        }
    });
    if ex.map.anim.is_some() {
        ctx.request_repaint();
    }
}

/// Celda de nivel 0 desde la que se busca el vecino: la propia, o el padre si es una celda insinuada (nivel 1). Una celda de
/// nivel 1 sin padre (`NO_CELL`) no tiene a quién subir: se queda en sí misma (no se indexa con `u32::MAX`).
fn base_cell(level: u8, parent: u32, cur: usize) -> usize {
    if level == 1 && parent != NO_CELL {
        parent as usize
    } else {
        cur
    }
}

/// Celda vecina en una dirección (flechas del teclado): la más cercana cuyo centro queda al otro lado.
fn neighbor(layout: &Layout, cur: Option<usize>, dx: f32, dy: f32) -> Option<usize> {
    let n0 = layout.stats().level0;
    if n0 == 0 {
        return None;
    }
    let Some(cur) = cur else { return Some(0) };
    // Si la selección es una celda insinuada, se parte de su padre.
    let cur = {
        let c = &layout.cells()[cur];
        base_cell(c.level, c.parent, cur)
    };
    let c = layout.cells()[cur].rect;
    let (cx, cy) = (c.x + c.w / 2.0, c.y + c.h / 2.0);
    let mut best: Option<(f32, usize)> = None;
    for (i, o) in layout.cells()[..n0].iter().enumerate() {
        if i == cur {
            continue;
        }
        let (ox, oy) = (o.rect.x + o.rect.w / 2.0, o.rect.y + o.rect.h / 2.0);
        let (along, cross) = if dx != 0.0 {
            ((ox - cx) * dx, (oy - cy).abs())
        } else {
            ((oy - cy) * dy, (ox - cx).abs())
        };
        if along <= 0.5 {
            continue;
        }
        let score = along + 2.0 * cross;
        if best.is_none_or(|b| score < b.0) {
            best = Some((score, i));
        }
    }
    best.map(|b| b.1).or(Some(cur))
}

/// Tooltip de una celda: nombre, tamaño, porcentaje del padre, composición y pista de uso.
fn tooltip(ui: &mut Ui, env: &mut Env, ex: &Explore, cell: &Cell, layout: &Layout, pal: &Palette) {
    let tr = env.tr;
    let tree = &ex.tree;
    let view = ex.view();
    ui.set_max_width(280.0);
    match cell.kind {
        CellKind::Others(i) => {
            let o = layout.others()[i as usize];
            ui.label(
                egui::RichText::new(tr.ta(
                    "others-title",
                    args!["n" => tr.int(o.count as u64), "size" => tr.size(o.bytes)],
                ))
                .font(theme::font_semibold(14.0)),
            );
            ui.label(
                egui::RichText::new(tr.t("tip-open-group"))
                    .size(12.0)
                    .color(pal.text_muted),
            );
        }
        CellKind::Node(id) => {
            let n = tree.node(id);
            let lower = describe::is_lower_bound(tree, id);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(describe::display_name(tree, id, tr))
                        .font(theme::font_semibold(14.0)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let s = if lower {
                        tr.size_min(view.alloc(id))
                    } else {
                        tr.size(view.alloc(id))
                    };
                    ui.label(egui::RichText::new(s).font(theme::font_semibold(14.0)));
                });
            });
            let parent_alloc = tree
                .parent(id)
                .map_or(view.alloc(id), |p| view.alloc(p))
                .max(1);
            let pct = 100.0 * view.alloc(id) as f64 / parent_alloc as f64;
            let parent_name = tree
                .parent(id)
                .map(|p| describe::display_name(tree, p, tr))
                .unwrap_or_default();
            let mut line = if tree.parent(id).is_some() {
                tr.ta(
                    "tip-pct-of",
                    args!["pct" => tr.pct_fine(pct), "parent" => parent_name],
                )
            } else {
                String::new()
            };
            if n.is_dir() {
                if !line.is_empty() {
                    line.push_str(" · ");
                }
                line.push_str(&tr.ta(
                    "files-count",
                    args!["n" => view.files_below(id), "count" => tr.int(view.files_below(id))],
                ));
            }
            if !line.is_empty() {
                ui.label(egui::RichText::new(line).color(pal.text_muted));
            }
            match describe::special(tree, id) {
                Some(Special::SystemBlock) => {
                    ui.label(
                        egui::RichText::new(tr.t("system-tip"))
                            .size(12.0)
                            .color(pal.text_muted),
                    );
                }
                Some(Special::InfoOnly(k)) => {
                    ui.label(
                        egui::RichText::new(tr.t(info_key(k)))
                            .size(12.0)
                            .color(pal.text_muted),
                    );
                }
                None => {
                    if n.is_dir() && view.alloc(id) > 0 {
                        let h = tree.hist(id);
                        stacked_bar(ui, pal, h);
                        let tp = describe::type_phrase(tr, h);
                        if !tp.is_empty() {
                            ui.label(egui::RichText::new(tp).color(pal.text_muted));
                        }
                        if let Some(ap) = describe::age_phrase(tr, h) {
                            ui.label(egui::RichText::new(ap).color(pal.text_muted));
                        }
                    }
                    if lower {
                        ui.label(
                            egui::RichText::new(tr.t("incomplete-note"))
                                .size(12.0)
                                .color(pal.review),
                        );
                    }
                    if n.flags & flags::CLOUD != 0 {
                        ui.label(
                            egui::RichText::new(tr.t("cloud-note"))
                                .size(12.0)
                                .color(pal.text_muted),
                        );
                    }
                    match ex.protection.target_verdict(&PathKey::new(&tree.path(id))) {
                        Verdict::Denied => {
                            ui.label(
                                egui::RichText::new(tr.t("protected-denied-why"))
                                    .size(12.0)
                                    .color(pal.text_muted),
                            );
                        }
                        Verdict::Untouchable => {
                            ui.label(
                                egui::RichText::new(tr.t("protected-untouchable-why"))
                                    .size(12.0)
                                    .color(pal.text_muted),
                            );
                        }
                        Verdict::ContentsOnly => {
                            ui.label(
                                egui::RichText::new(tr.t("protected-contents-why"))
                                    .size(12.0)
                                    .color(pal.text_muted),
                            );
                        }
                        Verdict::Allowed => {}
                    }
                    if describe::can_enter_view(&view, id) {
                        ui.label(
                            egui::RichText::new(tr.t("tip-enter"))
                                .size(12.0)
                                .color(pal.text_muted),
                        );
                    }
                }
            }
        }
    }
}

/// Clave del texto de "solo informar" de cada archivo.
pub(crate) fn info_key(k: InfoKind) -> &'static str {
    match k {
        InfoKind::Pagefile => "info-pagefile",
        InfoKind::Hiberfil => "info-hiberfil",
        InfoKind::Swapfile => "info-swapfile",
    }
}

/// Barra apilada con el reparto de bytes por tipo.
pub(crate) fn stacked_bar(ui: &mut Ui, pal: &Palette, h: &tesela_model::Hist) {
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width().min(260.0), 8.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(4), pal.track);
    let mut x = rect.left();
    let total: f32 = h.type_pct.iter().map(|&p| p as f32).sum::<f32>().max(1.0);
    let painter = ui.painter_at(rect);
    for t in FileType::ALL {
        let p = h.type_pct[t as usize] as f32;
        if p <= 0.0 {
            continue;
        }
        let w = rect.width() * p / total;
        painter.rect_filled(
            Rect::from_min_size(pos2(x, rect.top()), vec2(w, rect.height())),
            CornerRadius::ZERO,
            pal.type_color(t),
        );
        x += w;
    }
}

/// Elementos del menú contextual de un nodo (Abrir en el Explorador, Copiar ruta, Propiedades y, deshabilitado,
/// Añadir a la cesta). En el modo demostración lo que toca el disco queda deshabilitado.
pub(crate) fn menu_items(ui: &mut Ui, env: &mut Env, ex: &mut Explore, sel: Sel) {
    let tr = env.tr;
    let tree = ex.tree.clone();
    let node = match sel {
        Sel::Node(id) => id,
        Sel::Group(o) => o.parent,
    };
    let is_system =
        matches!(sel, Sel::Node(id) if describe::special(&tree, id) == Some(Special::SystemBlock));
    let real_path = !env.demo && !is_system;
    ui.set_min_width(210.0);
    let state = ex.add_state(sel);
    match sel {
        Sel::Node(id) if describe::can_enter_view(&ex.view(), id) => {
            // Lo protegido no se puede añadir a la cesta: "Ver contenido" ocupa su lugar.
            let key = if matches!(state, AddState::Untouchable | AddState::Denied) {
                "menu-view-contents"
            } else {
                "menu-enter"
            };
            if ui.button(tr.t(key)).clicked() {
                ex.enter(sel);
                ui.close();
            }
        }
        Sel::Group(_) if ui.button(tr.t("menu-open-group")).clicked() => {
            ex.enter(sel);
            ui.close();
        }
        _ => {}
    }
    let unavailable = if env.demo {
        tr.t("demo-unavailable")
    } else {
        tr.t("menu-unavailable-system")
    };
    let b = ui.add_enabled(real_path, egui::Button::new(tr.t("menu-open-explorer")));
    if !real_path {
        b.clone().on_disabled_hover_text(unavailable.clone());
    }
    if b.clicked() {
        let p = tree.path(node);
        ex.reveal(&p, tr, ui.input(|i| i.time));
        ui.close();
    }
    let b = ui.add_enabled(!is_system, egui::Button::new(tr.t("menu-copy-path")));
    if is_system {
        b.clone()
            .on_disabled_hover_text(tr.t("menu-unavailable-system"));
    }
    if b.clicked() {
        let text = ex.path_of(sel);
        ui.ctx().copy_text(text);
        ex.toast = Some((tr.t("path-copied"), ui.input(|i| i.time) + 2.0));
        ui.close();
    }
    let b = ui.add_enabled(real_path, egui::Button::new(tr.t("menu-properties")));
    if !real_path {
        b.clone().on_disabled_hover_text(unavailable);
    }
    if b.clicked() {
        platform::show_properties_async(tree.path(node));
        ui.close();
    }
    ui.separator();
    let now = ui.input(|i| i.time);
    let disabled = |ui: &mut Ui, label: String, why: &str| {
        ui.add_enabled(false, egui::Button::new(label))
            .on_disabled_hover_text(tr.t(why));
    };
    match state {
        AddState::Addable => {
            if ui.button(tr.t("menu-add-basket")).clicked() {
                ex.basket_add(sel, env, now);
                ui.close();
            }
        }
        AddState::ContentsOf => {
            let name = describe::display_name(&tree, node, tr);
            if ui
                .button(tr.ta("menu-add-contents", args!["name" => name]))
                .clicked()
            {
                ex.basket_add(sel, env, now);
                ui.close();
            }
        }
        AddState::InBasket if ex.basket_has_own(sel) => {
            if ui.button(tr.t("menu-remove-basket")).clicked() {
                ex.basket_remove(sel);
                ui.close();
            }
        }
        AddState::InBasket => disabled(ui, tr.t("menu-in-basket"), "menu-in-basket-why"),
        AddState::Untouchable => disabled(ui, tr.t("menu-add-basket"), "protected-untouchable-why"),
        AddState::Denied => disabled(ui, tr.t("menu-add-basket"), "protected-denied-why"),
        AddState::InfoOnly => disabled(ui, tr.t("menu-add-basket"), "menu-unavailable-info"),
        AddState::SystemBlock => disabled(ui, tr.t("menu-add-basket"), "menu-unavailable-system"),
        AddState::CloudOnly => disabled(ui, tr.t("menu-add-basket"), "menu-unavailable-cloud"),
        AddState::Group => disabled(ui, tr.t("menu-add-basket"), "tip-open-group"),
    }
}

/// Para los tests: ¿qué ids de nodo hay entre las celdas de nivel 0?
#[allow(dead_code)]
pub(crate) fn level0_nodes(layout: &Layout) -> Vec<NodeId> {
    layout
        .cells()
        .iter()
        .filter(|c| c.level == 0)
        .filter_map(|c| match c.kind {
            CellKind::Node(id) => Some(id),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::{self, Variant};
    use tesela_layout::Layout;

    fn layout_of(tree: &Tree, root: Root, size: (u32, u32)) -> Layout {
        Layout::compute(
            tree,
            LayoutKey {
                root,
                size,
                depth: Depth::Two,
                version: 1,
                params: Params::default(),
            },
        )
    }

    /// GM-m7: una celda insinuada (nivel 1) sin padre (`NO_CELL`) no se indexa con `u32::MAX`.
    #[test]
    fn la_celda_base_del_vecino_respeta_el_padre_ausente() {
        assert_eq!(base_cell(0, NO_CELL, 4), 4);
        assert_eq!(base_cell(1, 2, 4), 2, "la insinuada parte de su padre");
        assert_eq!(
            base_cell(1, NO_CELL, 4),
            4,
            "sin padre se queda en sí misma"
        );
    }

    fn style_of(l: &Layout, st: &[CellStyle], id: NodeId) -> CellStyle {
        st[l.cell_of_node(id).expect("celda de nivel 0")]
    }

    #[test]
    fn los_colores_siguen_el_plan_tipo_dominante_mixto_y_grises_con_significado() {
        let t = demo::tree(Variant::Complete);
        let pal = Palette::of(false);
        // Raíz: Users (mixto), el bloque del sistema, pagefile.
        let l = layout_of(&t, Root::node(0), (1000, 700));
        let st = styles_for(&l, &t, &pal);
        let users = t.child_by_name(0, b"Users").unwrap();
        assert_eq!(
            style_of(&l, &st, users).fill,
            pal.mixed(),
            "carpeta sin tipo dominante = Mixto"
        );
        let pf = t.child_by_name(0, b"pagefile.sys").unwrap();
        let s = style_of(&l, &st, pf);
        assert_eq!(s.fill, pal.info_only());
        assert_eq!(s.special, Some(Special::InfoOnly(InfoKind::Pagefile)));
        let sys = t.children(0).last().unwrap();
        let s = style_of(&l, &st, sys);
        assert_eq!(s.fill, pal.system_block());
        assert_eq!(s.special, Some(Special::SystemBlock));
        // Dentro de la carpeta de usuario: vídeo (azul), imágenes (naranja), OneDrive (nube) y "Otros" (gris).
        let me = t.child_by_name(users, demo::USER.as_bytes()).unwrap();
        let l = layout_of(&t, Root::node(me), (1000, 700));
        let st = styles_for(&l, &t, &pal);
        let vid = t.child_by_name(me, "Vídeos".as_bytes()).unwrap();
        assert_eq!(style_of(&l, &st, vid).fill, pal.type_color(FileType::Video));
        let img = t.child_by_name(me, "Imágenes".as_bytes()).unwrap();
        assert_eq!(style_of(&l, &st, img).fill, pal.type_color(FileType::Image));
        let od = t.child_by_name(me, b"OneDrive").unwrap();
        assert!(
            style_of(&l, &st, od).cloud,
            "OneDrive lleva el icono de la nube"
        );
        let others = l
            .cells()
            .iter()
            .position(|c| matches!(c.kind, CellKind::Others(_)))
            .unwrap();
        assert_eq!(st[others].fill, pal.others());
        // Las celdas insinuadas son un aclarado del color de su padre.
        let child = l
            .cells()
            .iter()
            .position(|c| c.level == 1 && c.parent as usize == l.cell_of_node(vid).unwrap())
            .unwrap();
        assert!(
            theme::luminance(st[child].fill) > theme::luminance(pal.type_color(FileType::Video))
        );
    }

    #[test]
    fn lo_incompleto_se_marca_y_el_mesh_lleva_sus_rayas() {
        let pal = Palette::of(false);
        let stopped = demo::tree(Variant::Stopped);
        let l = layout_of(&stopped, Root::node(0), (1000, 700));
        let st = styles_for(&l, &stopped, &pal);
        let win = stopped.child_by_name(0, b"Windows").unwrap();
        assert!(
            style_of(&l, &st, win).lower_bound,
            "Windows tiene algo incompleto por debajo"
        );
        let users = stopped.child_by_name(0, b"Users").unwrap();
        assert!(!style_of(&l, &st, users).lower_bound);
        // Las rayas diagonales hacen que el mesh tenga más vértices que el mismo árbol sin ellas: se compara con
        // el mesh de un árbol completo de las mismas celdas de nivel 0 (el de la demostración completa no las lleva).
        let with = build_mesh(&l, &stopped, &st, &pal, 1.0, pos2(0.0, 0.0));
        let mut no_hatch = st.clone();
        for s in &mut no_hatch {
            s.lower_bound = false;
        }
        let without = build_mesh(&l, &stopped, &no_hatch, &pal, 1.0, pos2(0.0, 0.0));
        assert!(
            with.vertices.len() > without.vertices.len(),
            "{} frente a {}",
            with.vertices.len(),
            without.vertices.len()
        );
    }

    #[test]
    fn la_franja_de_los_tres_tipos_solo_va_en_mixto_de_48_px_o_mas() {
        let pal = Palette::of(false);
        let t = demo::tree(Variant::Complete);
        let l = layout_of(&t, Root::node(0), (1000, 700));
        let st = styles_for(&l, &t, &pal);
        let a = build_mesh(&l, &t, &st, &pal, 1.0, pos2(0.0, 0.0));
        // Mismo layout en un viewport pequeño: las celdas de menos de 48 px no llevan franja.
        let small = layout_of(&t, Root::node(0), (180, 120));
        let sst = styles_for(&small, &t, &pal);
        let b = build_mesh(&small, &t, &sst, &pal, 1.0, pos2(0.0, 0.0));
        assert!(a.vertices.len() > b.vertices.len());
        // Con una franja, el mesh tiene vértices del color de alguno de los tres tipos principales de Users.
        let users = t.child_by_name(0, b"Users").unwrap();
        let top = describe::top_types(t.hist(users), 3);
        assert_eq!(top.len(), 3);
        assert!(t.hist(users).dominant_type().is_none());
        let want = pal.type_color(top[0].0);
        assert!(a.vertices.iter().any(|v| v.color.to_array()[3] > 0
            && v.color.r() == want.r()
            && v.color.g() == want.g()
            && v.color.b() == want.b()));
    }

    #[test]
    fn el_mesh_cabe_en_el_presupuesto_incluso_con_15k_celdas() {
        // El peor caso: ~15 000 celdas. Cada celda grande lleva esquinas redondeadas; las diminutas, rectángulos planos.
        let pal = Palette::of(false);
        let t = demo::tree(Variant::Stress);
        let l = layout_of(&t, Root::node(0), (2400, 1500));
        assert_eq!(l.cells().len(), 15_000, "el tope de celdas se respeta");
        let st = styles_for(&l, &t, &pal);
        let mesh = build_mesh(&l, &t, &st, &pal, 1.0, pos2(0.0, 0.0));
        let per_cell = mesh.vertices.len() as f32 / l.cells().len() as f32;
        assert!(per_cell < 14.0, "{per_cell} vértices por celda");
        assert!(
            mesh.indices
                .iter()
                .all(|&i| (i as usize) < mesh.vertices.len())
        );
    }

    #[test]
    fn el_zoom_transforma_el_mesh_sin_cambiar_su_topologia() {
        let pal = Palette::of(true);
        let t = demo::tree(Variant::Complete);
        let l = layout_of(&t, Root::node(0), (600, 400));
        let st = styles_for(&l, &t, &pal);
        let m = build_mesh(&l, &t, &st, &pal, 1.0, pos2(0.0, 0.0));
        let from = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0));
        let to = Rect::from_min_size(pos2(100.0, 50.0), vec2(300.0, 200.0));
        let z = transformed(&m, from, to, 0.5);
        assert_eq!(z.indices, m.indices);
        assert_eq!(z.vertices.len(), m.vertices.len());
        for (a, b) in m.vertices.iter().zip(&z.vertices) {
            assert!((b.pos.x - (100.0 + a.pos.x * 0.5)).abs() < 1e-3);
            assert!((b.pos.y - (50.0 + a.pos.y * 0.5)).abs() < 1e-3);
            assert!(b.color.a() <= a.color.a());
        }
    }
}
