//! Selección múltiple: marcar varios elementos de la carpeta que se está viendo y añadirlos JUNTOS a la cesta.
//!
//! * Lo marcado vive en [`Marks`] y es independiente de `Explore::selection` (el elemento "activo" de los detalles).
//! * **Solo se marcan hijos de la carpeta actual** (o de la agrupación "Otros" abierta): si el clic o la tecla cae en un
//!   descendiente (una celda insinuada del mapa), se marca su ascendiente de esta carpeta. Entre hermanos nunca hay
//!   anidación, así que un tamaño no se puede sumar dos veces. Lo único que puede solaparse es la cesta, y de eso se
//!   encarga `add_state` (`InBasket`): lo que ya está en ella se excluye y se explica.
//! * Se vacía al cambiar de carpeta (entrar, subir, migas, abrir "Otros"): lo marcado habla de ESTA carpeta, y una marca
//!   invisible haría que "Añadir" tocase elementos que ya no se ven. Reescanear o volver a Inicio sueltan la exploración
//!   entera, y una limpieza solo retira lo que desapareció de verdad.
//! * Añadir usa el mismo camino que el menú y el panel de detalles (`Explore::basket_add_one`): ninguna protección se salta.

use crate::app::Env;
use crate::args;
use crate::explore::{AddState, Explore, Sel};
use crate::i18n::Tr;
use crate::icons::Icon;
use crate::theme;
use crate::widgets::{self, ButtonStyle};
use egui::{
    Align, Color32, CornerRadius, Layout, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder,
    WidgetInfo, WidgetType, pos2, vec2,
};
use std::collections::HashSet;
use std::sync::Arc;
use tesela_layout::Others;
use tesela_model::NodeId;

/// Alto de la barra de selección (dos líneas de texto).
pub(crate) const BAR_H: f32 = 44.0;
/// Ancho que ocupa la columna de casillas de la lista.
pub(crate) const CHECK_W: f32 = 26.0;

/// Lo marcado y el modo "Seleccionar".
#[derive(Default)]
pub(crate) struct Marks {
    set: HashSet<NodeId>,
    /// Último elemento tocado (el extremo fijo de un rango con Mayús).
    pub(crate) anchor: Option<NodeId>,
    /// Modo "Seleccionar": pulsar un elemento lo marca o desmarca, y la lista enseña sus casillas.
    pub(crate) mode: bool,
    /// Cambia con cada modificación (clave de la caché del resumen).
    rev: u64,
    cache: Option<(u64, Arc<Summary>)>,
}

impl Marks {
    pub(crate) fn len(&self) -> usize {
        self.set.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    pub(crate) fn contains(&self, id: NodeId) -> bool {
        self.set.contains(&id)
    }

    /// Los marcados, en orden de identificador.
    pub(crate) fn sorted(&self) -> Vec<NodeId> {
        let mut v: Vec<NodeId> = self.set.iter().copied().collect();
        v.sort_unstable();
        v
    }

    /// Vacía lo marcado (el modo "Seleccionar" se queda como está).
    pub(crate) fn clear(&mut self) {
        if !self.set.is_empty() {
            self.set.clear();
            self.rev += 1;
        }
        self.anchor = None;
    }

    /// Marca `id` si no lo estaba y lo desmarca si lo estaba. Pasa a ser el ancla de los rangos.
    pub(crate) fn toggle(&mut self, id: NodeId) {
        if !self.set.remove(&id) {
            self.set.insert(id);
        }
        self.anchor = Some(id);
        self.rev += 1;
    }

    pub(crate) fn add(&mut self, ids: impl IntoIterator<Item = NodeId>) {
        self.set.extend(ids);
        self.rev += 1;
    }

    pub(crate) fn remove_all(&mut self, ids: &[NodeId]) {
        for id in ids {
            self.set.remove(id);
        }
        self.rev += 1;
    }

    /// Marca lo que no estaba marcado entre `ids` y desmarca lo que sí.
    pub(crate) fn invert(&mut self, ids: &[NodeId]) {
        for &id in ids {
            if !self.set.remove(&id) {
                self.set.insert(id);
            }
        }
        self.rev += 1;
    }

    /// Conserva solo lo que cumple `keep` (tras una limpieza: lo que ya no existe deja de estar marcado).
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(NodeId) -> bool) {
        let before = self.set.len();
        self.set.retain(|&id| keep(id));
        if self.set.len() != before {
            self.rev += 1;
        }
    }
}

/// Elementos de `order` entre `a` y `b`, ambos incluidos y en cualquier sentido. Vacío si alguno no está en `order`.
pub(crate) fn range_between(order: &[NodeId], a: NodeId, b: NodeId) -> &[NodeId] {
    let (Some(ia), Some(ib)) = (
        order.iter().position(|&x| x == a),
        order.iter().position(|&x| x == b),
    ) else {
        return &[];
    };
    &order[ia.min(ib)..=ia.max(ib)]
}

/// Qué pasaría con lo marcado si se añadiese a la cesta.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Summary {
    /// Cuántos elementos hay marcados.
    pub marked: usize,
    /// Los que SÍ se pueden añadir (en orden de identificador).
    pub add: Vec<NodeId>,
    /// De ellos, cuántos son carpetas conocidas de las que se añade el contenido y no la carpeta.
    pub contents: usize,
    /// Lo que ocupan los que se añaden (cada hermano una sola vez).
    pub bytes: u64,
    /// Alguno de los que se añaden tiene una cifra incompleta (mínimo).
    pub lower: bool,
    pub in_basket: usize,
    pub protected: usize,
    pub info: usize,
    pub cloud: usize,
}

impl Summary {
    /// Cuántos de los marcados NO se pueden añadir.
    pub(crate) fn cannot(&self) -> usize {
        self.in_basket + self.protected + self.info + self.cloud
    }
}

/// Las dos líneas de la barra: el recuento con el tamaño y, si hace falta, lo que no se puede añadir y por qué.
pub(crate) fn summary_lines(tr: &Tr, s: &Summary) -> (String, Option<String>) {
    let n = s.marked as u64;
    let count = tr.int(n);
    let first = if s.add.is_empty() {
        tr.ta("sel-count-only", args!["n" => n, "count" => count])
    } else {
        let size = if s.lower {
            tr.size_min(s.bytes)
        } else {
            tr.size(s.bytes)
        };
        tr.ta(
            "sel-count",
            args!["n" => n, "count" => count, "size" => size],
        )
    };
    let mut parts: Vec<String> = Vec::new();
    let cannot = s.cannot();
    if cannot > 0 {
        let why: Vec<String> = [
            ("sel-why-protected", s.protected),
            ("sel-why-in-basket", s.in_basket),
            ("sel-why-info", s.info),
            ("sel-why-cloud", s.cloud),
        ]
        .iter()
        .filter(|(_, k)| *k > 0)
        .map(|(key, k)| tr.ta(key, args!["n" => *k as u64, "count" => tr.int(*k as u64)]))
        .collect();
        parts.push(tr.ta(
            "sel-cannot",
            args!["n" => cannot as u64, "count" => tr.int(cannot as u64), "detail" => why.join(", ")],
        ));
    }
    if s.contents > 0 {
        parts.push(tr.ta(
            "sel-contents",
            args!["n" => s.contents as u64, "count" => tr.int(s.contents as u64)],
        ));
    }
    let second = (!parts.is_empty()).then(|| parts.join(" · "));
    (first, second)
}

impl Explore {
    /// Los elementos que la carpeta muestra ahora, en el orden de la lista: los aciertos si hay búsqueda y, si no, todos
    /// los hijos (o los de la agrupación "Otros" abierta).
    pub(crate) fn visible_order(&mut self) -> Vec<NodeId> {
        match self.search_matches() {
            Some(h) => h.to_vec(),
            None => self.shown_children().iter().collect(),
        }
    }

    /// Los elementos que agrupa una celda "Otros".
    pub(crate) fn others_members(&self, o: Others) -> Vec<NodeId> {
        let kids = tesela_layout::Children::of_view(&self.view(), o.parent);
        let end = ((o.start + o.count) as usize).min(kids.len());
        ((o.start as usize).min(end)..end)
            .map(|i| kids.get(i))
            .collect()
    }

    /// ¿Están marcados todos los elementos de esa agrupación "Otros"?
    pub(crate) fn others_all_marked(&self, o: Others) -> bool {
        let m = self.others_members(o);
        !m.is_empty() && m.iter().all(|&id| self.multi.contains(id))
    }

    /// El hijo de la carpeta actual que es `id` o lo contiene (una celda insinuada del mapa o una selección más profunda
    /// se marcan por su ascendiente de esta carpeta). `None` si `id` no cuelga de lo que se está viendo.
    pub(crate) fn child_of_root(&self, id: NodeId) -> Option<NodeId> {
        let mut cur = id;
        loop {
            let p = self.tree.parent(cur)?;
            if p == self.root.node {
                break;
            }
            cur = p;
        }
        if self.root.is_group() && !self.shown_children().iter().any(|c| c == cur) {
            return None;
        }
        Some(cur)
    }

    /// Marca o desmarca un elemento (una agrupación "Otros" marca o desmarca todos sus elementos).
    pub(crate) fn marks_toggle(&mut self, sel: Sel) {
        match sel {
            Sel::Node(id) => {
                if let Some(c) = self.child_of_root(id) {
                    self.multi.toggle(c);
                }
            }
            Sel::Group(o) => {
                let members = self.others_members(o);
                if members.is_empty() {
                    return;
                }
                if members.iter().all(|&id| self.multi.contains(id)) {
                    self.multi.remove_all(&members);
                } else {
                    self.multi.add(members);
                }
            }
        }
    }

    /// Marca el rango entre el ancla y `target` (en el orden `order` de la lista), sin quitar lo que ya estaba marcado.
    pub(crate) fn marks_range_in(&mut self, order: &[NodeId], target: NodeId) {
        let anchor = self
            .multi
            .anchor
            .filter(|a| order.contains(a))
            .unwrap_or(target);
        let range = range_between(order, anchor, target).to_vec();
        self.multi.add(range);
        self.multi.anchor = Some(anchor);
    }

    /// "Seleccionar todo aquí": todo lo que se ve (los aciertos si hay búsqueda).
    pub(crate) fn marks_all_here(&mut self) {
        let ids = self.visible_order();
        self.multi.add(ids);
    }

    /// "Invertir selección" sobre lo que se ve: lo marcado se desmarca y lo demás se marca.
    pub(crate) fn marks_invert_here(&mut self) {
        let ids = self.visible_order();
        self.multi.invert(&ids);
    }

    /// Qué pasaría con lo marcado al añadirlo, clasificado con las mismas reglas que el icono de la cesta (`add_state`).
    pub(crate) fn compute_summary(&self) -> Summary {
        let view = self.view();
        let mut s = Summary::default();
        for id in self.multi.sorted() {
            s.marked += 1;
            let state = self.add_state(Sel::Node(id));
            match state {
                AddState::Addable | AddState::ContentsOf => {
                    if state == AddState::ContentsOf {
                        s.contents += 1;
                    }
                    s.add.push(id);
                    s.bytes += view.alloc(id);
                    s.lower |= crate::describe::is_lower_bound(&self.tree, id);
                }
                AddState::InBasket => s.in_basket += 1,
                AddState::Untouchable | AddState::Denied => s.protected += 1,
                AddState::InfoOnly | AddState::SystemBlock => s.info += 1,
                AddState::CloudOnly => s.cloud += 1,
                AddState::Group => {}
            }
        }
        s
    }

    /// El resumen de lo marcado, calculado una vez por cambio de la selección, del árbol o de la cesta.
    pub(crate) fn marks_summary(&mut self) -> Arc<Summary> {
        use std::hash::{Hash, Hasher};
        if self.multi.is_empty() {
            return Arc::default();
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for s in self.flow.basket.sources() {
            format!("{s:?}").hash(&mut h);
        }
        (self.multi.rev, self.version).hash(&mut h);
        let stamp = h.finish();
        if let Some((k, s)) = &self.multi.cache
            && *k == stamp
        {
            return s.clone();
        }
        let s = Arc::new(self.compute_summary());
        self.multi.cache = Some((stamp, s.clone()));
        s
    }

    /// Añade a la cesta lo marcado que se puede añadir, uno a uno por el mismo camino que el icono de la cesta, y vacía
    /// la selección. Devuelve cuántos entraron.
    pub(crate) fn marks_add_to_basket(&mut self, env: &Env, now: f64) -> usize {
        // Se recalcula en el momento (no con la caché): lo que cuenta es el estado de ahora.
        let s = self.compute_summary();
        let mut added = 0usize;
        for &id in &s.add {
            if self.basket_add_one(Sel::Node(id)) {
                added += 1;
            }
        }
        self.multi.clear();
        let tr = env.tr;
        let mut msg = tr.ta(
            "sel-added",
            args!["n" => added as u64, "count" => tr.int(added as u64)],
        );
        let skipped = s.marked - added;
        if skipped > 0 {
            msg.push_str(" · ");
            msg.push_str(&tr.ta(
                "sel-added-skipped",
                args!["count" => tr.int(skipped as u64)],
            ));
        }
        self.toast = Some((msg, now + 3.0));
        added
    }

    /// La barra de selección (bajo el mapa o la lista): el interruptor "Seleccionar" y, con algo marcado, el recuento, lo
    /// que no se puede añadir y los botones. Ocupa siempre el mismo alto: así el mapa no se recoloca al marcar el primero.
    pub(crate) fn selection_bar(&mut self, ui: &mut Ui, env: &mut Env) {
        let pal = env.pal;
        let tr = env.tr;
        let summary = self.marks_summary();
        let has = !self.multi.is_empty();
        let mut do_add = false;
        let mut do_clear = false;
        let full = ui.available_rect_before_wrap();
        let mut left_rect = full;
        if has {
            let right = ui.scope_builder(
                UiBuilder::new()
                    .max_rect(full)
                    .layout(Layout::right_to_left(Align::Center)),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let clear = tr.t("sel-clear");
                    if widgets::button(ui, env, ButtonStyle::Ghost, None, &clear, true).clicked() {
                        do_clear = true;
                    }
                    let can = !summary.add.is_empty();
                    let label = if can {
                        let n = summary.add.len() as u64;
                        tr.ta("sel-add", args!["n" => n, "count" => tr.int(n)])
                    } else {
                        tr.t("sel-add-none")
                    };
                    let r = widgets::button(
                        ui,
                        env,
                        ButtonStyle::Secondary,
                        Some(Icon::Cart),
                        &label,
                        can,
                    );
                    if !can {
                        r.clone().on_hover_text(tr.t("sel-add-none-tip"));
                    }
                    if r.clicked() {
                        do_add = true;
                    }
                },
            );
            left_rect = Rect::from_min_max(
                full.min,
                pos2(
                    (right.response.rect.left() - 12.0).max(full.min.x),
                    full.max.y,
                ),
            );
        }
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(left_rect)
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(left_rect));
                ui.spacing_mut().item_spacing.x = 10.0;
                self.mode_toggle(ui, env);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    if has {
                        let (first, second) = summary_lines(tr, &summary);
                        let full_text = match &second {
                            Some(s) => format!("{first} · {s}"),
                            None => first.clone(),
                        };
                        let l = ui.add(
                            egui::Label::new(
                                egui::RichText::new(&first)
                                    .font(theme::font_semibold(14.0))
                                    .color(pal.text),
                            )
                            .truncate(),
                        );
                        l.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &first));
                        if let Some(sec) = second {
                            let l2 = ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&sec).size(12.0).color(pal.text_muted),
                                )
                                .truncate(),
                            );
                            l2.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &sec));
                            l2.on_hover_text(full_text);
                        }
                    } else {
                        let key = if self.multi.mode {
                            "sel-hint-mode"
                        } else {
                            "sel-hint"
                        };
                        let text = tr.t(key);
                        let l = ui.add(
                            egui::Label::new(
                                egui::RichText::new(&text).size(12.0).color(pal.text_muted),
                            )
                            .truncate(),
                        );
                        l.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &text));
                    }
                });
            },
        );
        ui.advance_cursor_after_rect(full);
        if do_clear {
            self.multi.clear();
        }
        if do_add {
            let now = ui.input(|i| i.time);
            self.marks_add_to_basket(env, now);
        }
    }

    /// El interruptor "Seleccionar" (modo selección): una casilla con su texto, alcanzable con Tab y accesible como casilla.
    fn mode_toggle(&mut self, ui: &mut Ui, env: &mut Env) {
        let pal = env.pal;
        let label = env.tr.t("sel-toggle");
        let on = self.multi.mode;
        let galley = ui.painter().layout_no_wrap(
            label.clone(),
            theme::font_semibold(14.0),
            Color32::PLACEHOLDER,
        );
        let w = 12.0 + 16.0 + 8.0 + galley.size().x + 14.0;
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 34.0), Sense::click());
        resp.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, true, on, label.clone()));
        if ui.is_rect_visible(rect) {
            let fill = if on || resp.hovered() {
                pal.accent_soft
            } else {
                pal.surface
            };
            ui.painter().rect(
                rect,
                CornerRadius::same(8),
                fill,
                Stroke::new(1.0, if on { pal.accent } else { pal.border }),
                StrokeKind::Inside,
            );
            paint_checkbox(
                ui.painter(),
                env,
                Rect::from_center_size(pos2(rect.left() + 20.0, rect.center().y), vec2(16.0, 16.0)),
                on,
            );
            ui.painter().galley(
                pos2(
                    rect.left() + 12.0 + 16.0 + 8.0,
                    rect.center().y - galley.size().y / 2.0,
                ),
                galley,
                pal.text,
            );
            if resp.has_focus() {
                widgets::focus_ring(ui.painter(), &pal, rect.expand(1.0), 9);
            }
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.clicked() {
            self.multi.mode = !on;
        }
        resp.on_hover_text(env.tr.t("sel-toggle-tip"));
    }
}

/// Casilla de 16 px: marcada con el relleno de acento y una marca blanca; sin marcar, solo el borde.
pub(crate) fn paint_checkbox(painter: &egui::Painter, env: &mut Env, rect: Rect, on: bool) {
    let pal = env.pal;
    if on {
        painter.rect_filled(rect, CornerRadius::same(4), pal.accent);
        env.icons
            .paint(painter, rect.shrink(2.0), Icon::Checkmark, Color32::WHITE);
    } else {
        painter.rect_stroke(
            rect.shrink(0.75),
            CornerRadius::same(4),
            Stroke::new(1.5, pal.text_muted),
            StrokeKind::Inside,
        );
    }
}

/// Insignia redonda con la marca (esquina de una celda del mapa seleccionada).
pub(crate) fn paint_check_badge(painter: &egui::Painter, env: &mut Env, center: egui::Pos2) {
    let pal = env.pal;
    painter.circle_filled(center, 10.0, pal.accent);
    env.icons.paint(
        painter,
        Rect::from_center_size(center, vec2(13.0, 13.0)),
        Icon::Checkmark,
        Color32::WHITE,
    );
}

/// Velo de acento de una celda del mapa seleccionada: se distingue del contorno de la selección activa (3 px por fuera),
/// del foco y de la insignia del carrito.
pub(crate) fn mark_tint(pal: &theme::Palette) -> Color32 {
    pal.accent.gamma_multiply(0.30)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alternar_marca_y_desmarca_y_fija_el_ancla() {
        let mut m = Marks::default();
        m.toggle(7);
        m.toggle(3);
        assert_eq!(m.sorted(), vec![3, 7]);
        assert_eq!(m.anchor, Some(3));
        m.toggle(7);
        assert_eq!(m.sorted(), vec![3]);
        m.clear();
        assert!(m.is_empty() && m.anchor.is_none());
    }

    #[test]
    fn invertir_marca_lo_no_marcado_y_viceversa() {
        let mut m = Marks::default();
        let all: Vec<NodeId> = (1..=5).collect();
        m.add(all.iter().copied());
        m.toggle(4);
        assert_eq!(m.sorted(), vec![1, 2, 3, 5]);
        m.invert(&all);
        assert_eq!(m.sorted(), vec![4]);
    }

    #[test]
    fn el_rango_incluye_los_dos_extremos_en_cualquier_sentido() {
        let order: Vec<NodeId> = vec![10, 20, 30, 40, 50];
        assert_eq!(range_between(&order, 20, 40), &[20, 30, 40]);
        assert_eq!(range_between(&order, 40, 20), &[20, 30, 40]);
        assert_eq!(range_between(&order, 30, 30), &[30]);
        assert!(range_between(&order, 20, 99).is_empty());
    }

    #[test]
    fn el_resumen_dice_que_no_se_puede_añadir_y_por_que() {
        let tr = Tr::new(crate::Lang::Es);
        let s = Summary {
            marked: 4,
            add: vec![1, 2],
            contents: 1,
            bytes: 3 << 30,
            lower: false,
            in_basket: 1,
            protected: 1,
            info: 0,
            cloud: 0,
        };
        let (a, b) = summary_lines(&tr, &s);
        assert_eq!(a, "4 seleccionados · 3,0 GB");
        assert_eq!(
            b.as_deref(),
            Some(
                "2 no se pueden añadir: 1 protegido, 1 ya en la cesta · 1 carpeta conocida: se añade su contenido"
            )
        );
        // Nada que añadir: sin tamaño.
        let none = Summary {
            marked: 1,
            protected: 1,
            ..Summary::default()
        };
        let (a, b) = summary_lines(&tr, &none);
        assert_eq!(a, "1 seleccionado");
        assert_eq!(b.as_deref(), Some("1 no se puede añadir: 1 protegido"));
        let en = Tr::new(crate::Lang::En);
        let (a, b) = summary_lines(&en, &s);
        assert_eq!(a, "4 selected · 3.0 GB");
        assert!(
            b.unwrap()
                .starts_with("2 cannot be added: 1 protected, 1 already in the basket")
        );
        // Sin nada excluido no hay segunda línea.
        let ok = Summary {
            marked: 2,
            add: vec![1, 2],
            bytes: 1 << 20,
            ..Summary::default()
        };
        assert!(summary_lines(&tr, &ok).1.is_none());
    }
}
