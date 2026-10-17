//! Selección múltiple: marcar varios elementos de la carpeta que se está viendo y añadirlos JUNTOS a la cesta.
//!
//! * Lo marcado vive en [`Marks`] y es independiente de `Explore::selection` (el elemento "activo" de los detalles).
//! * **Solo se marcan hijos de la carpeta actual** (o de la agrupación "Otros" abierta): si el clic o la tecla cae en un
//!   descendiente (una celda insinuada del mapa), se marca su ascendiente de esta carpeta. Entre hermanos nunca hay
//!   anidación, así que un tamaño no se puede sumar dos veces. Lo único que puede solaparse es la cesta, y de eso se
//!   encarga `add_state` (`InBasket`): lo que ya está en ella se excluye y se explica.
//! * **Nunca hay una marca invisible**: se vacía al cambiar de carpeta (entrar, subir, migas, abrir "Otros") y, al
//!   cambiar la búsqueda, solo se conservan las marcas que siguen entre los aciertos (así "Añadir N" no añade nada que la
//!   lista no enseñe). Reescanear o volver a Inicio sueltan la exploración entera, y una limpieza solo retira lo que
//!   desapareció de verdad. Si navegar suelta marcas, un aviso breve dice cuántas.
//! * Añadir comprueba cada elemento con `add_state` y lo da de alta por lotes (`Basket::add_batch`), que repite por
//!   elemento las comprobaciones de `add_path` y `add_contents_of`: ninguna protección se salta. Para una sola ruta, el
//!   menú y el panel de detalles usan `Explore::basket_add_one`.

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
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tesela_cleaner::{AddOutcome, BatchKind};
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
    /// Las marcas de antes de una ráfaga de Mayús+flechas: lo marcado es eso más el rango, así el rango también encoge.
    run_base: Option<HashSet<NodeId>>,
    /// Qué es cada marcado respecto a la cesta (`add_state`), válido mientras no cambien el árbol ni la cesta: marcar o
    /// desmarcar uno solo clasifica ese, no los demás.
    cls_key: (u64, u64),
    cls: HashMap<NodeId, AddState>,
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
        self.run_base = None;
        if !self.set.is_empty() {
            self.set.clear();
            self.rev += 1;
        }
        self.anchor = None;
    }

    /// Marca `id` si no lo estaba y lo desmarca si lo estaba. Pasa a ser el ancla de los rangos.
    pub(crate) fn toggle(&mut self, id: NodeId) {
        self.run_base = None;
        if !self.set.remove(&id) {
            self.set.insert(id);
        }
        self.anchor = Some(id);
        self.rev += 1;
    }

    pub(crate) fn add(&mut self, ids: impl IntoIterator<Item = NodeId>) {
        self.run_base = None;
        self.set.extend(ids);
        self.rev += 1;
    }

    pub(crate) fn remove_all(&mut self, ids: &[NodeId]) {
        self.run_base = None;
        for id in ids {
            self.set.remove(id);
        }
        self.rev += 1;
    }

    /// Marca lo que no estaba marcado entre `ids` y desmarca lo que sí.
    pub(crate) fn invert(&mut self, ids: &[NodeId]) {
        self.run_base = None;
        for &id in ids {
            if !self.set.remove(&id) {
                self.set.insert(id);
            }
        }
        self.rev += 1;
    }

    /// Conserva solo lo que cumple `keep` (tras una limpieza: lo que ya no existe deja de estar marcado).
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(NodeId) -> bool) {
        self.run_base = None;
        let before = self.set.len();
        self.set.retain(|&id| keep(id));
        if self.set.len() != before {
            self.rev += 1;
        }
    }

    /// El elemento activo se movió sin Mayús: es el ancla nueva y acaba la ráfaga de Mayús+flechas.
    pub(crate) fn plain_move(&mut self, id: NodeId) {
        self.anchor = Some(id);
        self.run_base = None;
    }

    /// Mayús+flechas: lo marcado pasa a ser lo de antes de la ráfaga más `range` (el rango entre el ancla y el extremo).
    pub(crate) fn set_run(&mut self, range: &[NodeId]) {
        let base = match self.run_base.take() {
            Some(b) => b,
            None => self.set.clone(),
        };
        self.set = base.iter().copied().chain(range.iter().copied()).collect();
        self.run_base = Some(base);
        self.rev += 1;
    }
}

/// ¿Debe alternar la marca de una fila un `clicked()` SIN puntero? Sí si es el clic por defecto de un lector de pantalla
/// (no hay teclas) o la primera pulsación de Espacio; no con Intro (es "entrar") ni con la autorrepetición de Espacio.
pub(crate) fn keyboard_click_toggles(i: &egui::InputState) -> bool {
    let (mut first, mut repeat, mut enter) = (false, false, false);
    for e in &i.events {
        if let egui::Event::Key {
            key,
            pressed: true,
            repeat: r,
            ..
        } = e
        {
            match key {
                egui::Key::Enter => enter = true,
                egui::Key::Space if *r => repeat = true,
                egui::Key::Space => first = true,
                _ => {}
            }
        }
    }
    !enter && (first || !repeat)
}

/// ¿Se pulsó Espacio (la pulsación, no la autorrepetición de mantenerlo)?
pub(crate) fn space_pressed_once(i: &egui::InputState) -> bool {
    i.events.iter().any(|e| {
        matches!(
            e,
            egui::Event::Key {
                key: egui::Key::Space,
                pressed: true,
                repeat: false,
                ..
            }
        )
    })
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
    /// De ellos, las carpetas conocidas de las que se añade el contenido y no la carpeta.
    pub contents_of: HashSet<NodeId>,
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

    /// Cuántos de los que se añaden son carpetas conocidas (se añade su contenido).
    pub(crate) fn contents(&self) -> usize {
        self.contents_of.len()
    }
}

/// Las dos líneas de la barra: el recuento con el tamaño y, si hace falta, lo que no se puede añadir y por qué.
pub(crate) fn summary_lines(tr: &Tr, s: &Summary) -> (String, Option<String>) {
    let n = s.marked as u64;
    let count = tr.int(n);
    let first = if s.add.is_empty() {
        tr.ta("sel-count-only", args!["n" => n, "count" => count])
    } else if s.cannot() > 0 {
        // Con algo excluido, el tamaño es solo el de lo que se añadiría.
        let size = if s.lower {
            tr.size_min(s.bytes)
        } else {
            tr.size(s.bytes)
        };
        tr.ta(
            "sel-count-partial",
            args!["n" => n, "count" => count, "size" => size],
        )
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
    if s.contents() > 0 {
        let c = s.contents() as u64;
        parts.push(tr.ta("sel-contents", args!["n" => c, "count" => tr.int(c)]));
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

    /// ¿Se puede alternar la marca de `id`? Tiene que colgar de lo que se ve y, con una búsqueda activa, o ser un acierto o
    /// estar ya marcado (desmarcar siempre se puede): así no se marca nunca lo que la lista no enseña.
    pub(crate) fn can_toggle_mark(&mut self, id: NodeId) -> bool {
        let Some(c) = self.child_of_root(id) else {
            return false;
        };
        self.multi.contains(c) || self.search_set().is_none_or(|hits| hits.contains(&c))
    }

    /// Marca o desmarca un elemento (una agrupación "Otros" marca o desmarca todos sus elementos).
    pub(crate) fn marks_toggle(&mut self, sel: Sel) {
        match sel {
            Sel::Node(id) => {
                if self.can_toggle_mark(id)
                    && let Some(c) = self.child_of_root(id)
                {
                    self.multi.toggle(c);
                }
            }
            Sel::Group(o) => {
                let mut members = self.others_members(o);
                // Con búsqueda, solo los elementos del grupo que son aciertos (nunca se marca lo que no se ve).
                if let Some(hits) = self.search_matches() {
                    let hits: HashSet<NodeId> = hits.iter().copied().collect();
                    members.retain(|id| hits.contains(id));
                }
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

    /// Mayús+flechas: lo marcado es lo de antes de la ráfaga más el rango entre el ancla y `target` (también encoge).
    pub(crate) fn marks_shift_extend(&mut self, order: &[NodeId], target: NodeId) {
        let anchor = self
            .multi
            .anchor
            .filter(|a| order.contains(a))
            .unwrap_or(target);
        let range = range_between(order, anchor, target).to_vec();
        self.multi.set_run(&range);
        self.multi.anchor = Some(anchor);
    }

    /// Al cambiar la búsqueda solo se conservan las marcas que siguen entre los aciertos: así lo que "Añadir N" añade es
    /// siempre algo que se ve. (Si se borra la búsqueda no vuelve nada: las marcas ya quitadas se quedan quitadas.)
    pub(crate) fn sync_marks_with_search(&mut self) {
        let q = crate::describe::fold(self.search.trim());
        if q == self.marks_search {
            return;
        }
        self.marks_search.clone_from(&q);
        if q.is_empty() || self.multi.is_empty() {
            return;
        }
        if let Some(hits) = self.search_matches() {
            let hits: HashSet<NodeId> = hits.iter().copied().collect();
            self.multi.retain(|id| hits.contains(&id));
        }
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
    /// Sin caché: lo usa Añadir (cuenta el estado de ahora mismo) y las pruebas.
    pub(crate) fn compute_summary(&self) -> Summary {
        self.summarize(&mut HashMap::new())
    }

    fn summarize(&self, cls: &mut HashMap<NodeId, AddState>) -> Summary {
        let view = self.view();
        let mut s = Summary::default();
        for id in self.multi.sorted() {
            s.marked += 1;
            let state = *cls
                .entry(id)
                .or_insert_with(|| self.add_state(Sel::Node(id)));
            match state {
                AddState::Addable | AddState::ContentsOf => {
                    if state == AddState::ContentsOf {
                        s.contents_of.insert(id);
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

    /// El resumen de lo marcado, recalculado solo si cambió la selección, el árbol o la cesta (versiones, sin formatear la
    /// cesta). La clasificación de cada marcado se conserva mientras no cambien el árbol ni la cesta: marcar o
    /// desmarcar uno no vuelve a clasificar los demás.
    pub(crate) fn marks_summary(&mut self) -> Arc<Summary> {
        if self.multi.is_empty() {
            return Arc::default();
        }
        let key = (self.version, self.flow.basket.rev());
        let stamp = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (self.multi.rev, key).hash(&mut h);
            h.finish()
        };
        if let Some((k, s)) = &self.multi.cache
            && *k == stamp
        {
            return s.clone();
        }
        let mut cls = std::mem::take(&mut self.multi.cls);
        if self.multi.cls_key != key {
            cls.clear();
            self.multi.cls_key = key;
        }
        let s = Arc::new(self.summarize(&mut cls));
        self.multi.cls = cls;
        self.multi.cache = Some((stamp, s.clone()));
        s
    }

    /// Añade a la cesta lo marcado que se puede añadir, por lotes, y vacía la selección. Devuelve (los que entraron, los
    /// marcados). Cada elemento pasa por `add_state` y por las comprobaciones de `Basket` (ruta de unidad local,
    /// protección, ya cubierta), las mismas que el icono de la cesta.
    pub(crate) fn marks_add_core(&mut self) -> (usize, usize) {
        // Se recalcula en el momento (no con la caché): lo que cuenta es el estado de ahora.
        let s = self.compute_summary();
        let items: Vec<(std::path::PathBuf, BatchKind)> = s
            .add
            .iter()
            .map(|&id| {
                let kind = if s.contents_of.contains(&id) {
                    BatchKind::ContentsOf
                } else {
                    BatchKind::Path
                };
                (self.tree.path(id), kind)
            })
            .collect();
        let prot = self.protection.clone();
        let out = self.flow.basket.add_batch(items, &prot);
        let added = out
            .iter()
            .filter(|o| matches!(o, AddOutcome::Added { .. }))
            .count();
        self.multi.clear();
        (added, s.marked)
    }

    /// "Añadir N a la cesta" de la barra: como [`Explore::marks_add_core`], con el aviso. No hace nada mientras hay una
    /// limpieza en curso. Devuelve cuántos entraron.
    pub(crate) fn marks_add_to_basket(&mut self, tr: &Tr, now: f64) -> usize {
        if self.flow.busy() {
            return 0;
        }
        let (added, marked) = self.marks_add_core();
        let mut msg = tr.ta(
            "sel-added",
            args!["n" => added as u64, "count" => tr.int(added as u64)],
        );
        let skipped = marked - added;
        if skipped > 0 {
            msg.push_str(" · ");
            msg.push_str(&tr.ta(
                "sel-added-skipped",
                args!["n" => skipped as u64, "count" => tr.int(skipped as u64)],
            ));
        }
        self.toast = Some((msg, now + 3.0));
        added
    }

    /// La barra de selección (bajo el mapa o la lista): el interruptor "Seleccionar" y, con algo marcado, el recuento, lo
    /// que no se puede añadir y los botones. Ocupa siempre el mismo alto: así el mapa no se recoloca al marcar el primero.
    /// Los controles se crean en orden de lectura (interruptor, Añadir, Desmarcar) para que Tab los recorra así.
    pub(crate) fn selection_bar(&mut self, ui: &mut Ui, env: &mut Env) {
        let pal = env.pal;
        let tr = env.tr;
        let summary = self.marks_summary();
        let has = !self.multi.is_empty();
        let busy = self.flow.busy();
        let mut do_add = false;
        let mut do_clear = false;
        let mut refocus = false;
        let full = ui.available_rect_before_wrap();
        // La barra ocupa la columna central: según su ancho se acorta el texto de los controles (el nombre accesible no
        // cambia). Los botones pierden el texto por debajo de 600 px; el interruptor lo enseña en cuanto cabe junto al
        // espacio que ocupan los botones (unos 350 px con texto, 90 sin él) y unos 210 px de texto del resumen.
        let button_text = full.width() >= 600.0;
        let toggle_text = full.width() - if button_text { 352.0 } else { 92.0 } >= 338.0;
        let clear = tr.t("sel-clear");
        let can = !summary.add.is_empty() && !busy;
        let add_label = if summary.add.is_empty() {
            tr.t("sel-add-none")
        } else {
            let n = summary.add.len() as u64;
            tr.ta("sel-add", args!["n" => n, "count" => tr.int(n)])
        };
        // Ancho de la zona de botones (se mide igual que `widgets::button`) para repartir el hueco antes de dibujar.
        let right_w = if !has {
            0.0
        } else if button_text {
            text_button_w(ui, &clear, false) + text_button_w(ui, &add_label, true) + 8.0 + 2.0
        } else {
            36.0 + 36.0 + 8.0 + 2.0
        };
        let left_rect = if has {
            Rect::from_min_max(
                full.min,
                pos2((full.right() - right_w - 12.0).max(full.min.x), full.max.y),
            )
        } else {
            full
        };
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(left_rect)
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(left_rect));
                ui.spacing_mut().item_spacing.x = 10.0;
                self.mode_toggle(ui, env, toggle_text);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    if has {
                        let (first, second) = summary_lines(tr, &summary);
                        // `vertical` coloca el bloque arriba del todo: se centra a ojo (una o dos líneas) en la barra.
                        ui.add_space(if second.is_some() { 5.0 } else { 13.0 });
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
                        l.on_hover_text(&full_text);
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
                        ui.add_space(13.0);
                        let l = ui.add(
                            egui::Label::new(
                                egui::RichText::new(&text).size(12.0).color(pal.text_muted),
                            )
                            .truncate(),
                        );
                        l.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &text));
                        // La pista se corta en ventanas estrechas: completa, a la vista del ratón.
                        l.on_hover_text(format!("{text} · {}", tr.t("sel-keys-tip")));
                    }
                });
            },
        );
        if has {
            let right_rect = Rect::from_min_max(pos2(full.right() - right_w, full.top()), full.max);
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(right_rect)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let r = if button_text {
                        widgets::button(
                            ui,
                            env,
                            ButtonStyle::Secondary,
                            Some(Icon::Cart),
                            &add_label,
                            can,
                        )
                    } else {
                        widgets::icon_button(ui, env, Icon::Cart, &add_label, can)
                    };
                    if !can {
                        // Sin nada que añadir, o con una limpieza en curso: la pista dice cuál de las dos.
                        let why = if busy {
                            "sel-add-busy-tip"
                        } else {
                            "sel-add-none-tip"
                        };
                        r.clone().on_hover_text(tr.t(why));
                    }
                    if r.clicked() {
                        do_add = true;
                        refocus |= r.interact_pointer_pos().is_none();
                    }
                    let r = if button_text {
                        widgets::button(ui, env, ButtonStyle::Ghost, None, &clear, true)
                    } else {
                        widgets::icon_button(ui, env, Icon::Dismiss, &clear, true)
                    };
                    if r.clicked() {
                        do_clear = true;
                        refocus |= r.interact_pointer_pos().is_none();
                    }
                },
            );
        }
        ui.advance_cursor_after_rect(full);
        if do_clear {
            self.multi.clear();
        }
        if do_add {
            let now = ui.input(|i| i.time);
            self.marks_add_to_basket(tr, now);
        }
        // Con teclado, los botones desaparecen al pulsarlos: el foco pasa al interruptor en vez de perderse.
        if refocus {
            ui.memory_mut(|m| m.request_focus(egui::Id::new(TOGGLE_ID)));
        }
    }

    /// El interruptor "Seleccionar" (modo selección): una casilla con su texto, alcanzable con Tab y accesible como casilla.
    fn mode_toggle(&mut self, ui: &mut Ui, env: &mut Env, with_text: bool) {
        let pal = env.pal;
        let label = env.tr.t("sel-toggle");
        let on = self.multi.mode;
        let galley = ui.painter().layout_no_wrap(
            label.clone(),
            theme::font_semibold(14.0),
            Color32::PLACEHOLDER,
        );
        // Sin texto, un cuadrado de 34 px con solo la casilla (el nombre accesible y la pista siguen diciendo "Seleccionar").
        let w = if with_text {
            12.0 + 16.0 + 8.0 + galley.size().x + 14.0
        } else {
            34.0
        };
        let (rect, _) = ui.allocate_exact_size(vec2(w, 34.0), Sense::hover());
        let resp = ui.interact(rect, egui::Id::new(TOGGLE_ID), Sense::click());
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
            let cx = if with_text {
                rect.left() + 20.0
            } else {
                rect.center().x
            };
            paint_checkbox(
                ui.painter(),
                env,
                Rect::from_center_size(pos2(cx, rect.center().y), vec2(16.0, 16.0)),
                on,
            );
            if with_text {
                ui.painter().galley(
                    pos2(
                        rect.left() + 12.0 + 16.0 + 8.0,
                        rect.center().y - galley.size().y / 2.0,
                    ),
                    galley,
                    pal.text,
                );
            }
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
        let tip = format!(
            "{label} · {} {}",
            env.tr.t("sel-toggle-tip"),
            env.tr.t("sel-keys-tip")
        );
        resp.on_hover_text(tip);
    }
}

/// Identificador del interruptor "Seleccionar" (para devolverle el foco tras Añadir o Desmarcar con el teclado).
const TOGGLE_ID: &str = "tesela-sel-toggle";

/// Ancho de un botón de texto de `widgets::button` (con icono de 18 px y su hueco, o sin él).
fn text_button_w(ui: &Ui, text: &str, icon: bool) -> f32 {
    let g = ui.painter().layout_no_wrap(
        text.to_string(),
        theme::font_semibold(14.0),
        Color32::PLACEHOLDER,
    );
    (24.0 + if icon { 26.0 } else { 0.0 } + g.size().x).max(40.0)
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
            contents_of: HashSet::from([2]),
            bytes: 3 << 30,
            lower: false,
            in_basket: 1,
            protected: 1,
            info: 0,
            cloud: 0,
        };
        let (a, b) = summary_lines(&tr, &s);
        assert_eq!(a, "4 seleccionados · se añadirían 3,0 GB");
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
        assert_eq!(a, "4 selected · 3.0 GB would be added");
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

    fn key_events(keys: &[(egui::Key, bool)]) -> egui::InputState {
        let mut i = egui::InputState::default();
        for &(key, repeat) in keys {
            i.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat,
                modifiers: egui::Modifiers::NONE,
            });
        }
        i
    }

    #[test]
    fn un_clic_sin_puntero_alterna_salvo_con_intro_o_la_autorrepeticion_de_espacio() {
        use egui::Key::{Enter, Space};
        // Sin teclas: el clic por defecto de un lector de pantalla.
        assert!(keyboard_click_toggles(&key_events(&[])));
        // La primera pulsación de Espacio sí; la autorrepetición sola, no.
        assert!(keyboard_click_toggles(&key_events(&[(Space, false)])));
        assert!(keyboard_click_toggles(&key_events(&[
            (Space, false),
            (Space, true)
        ])));
        assert!(!keyboard_click_toggles(&key_events(&[(Space, true)])));
        // Intro nunca alterna.
        assert!(!keyboard_click_toggles(&key_events(&[(Enter, false)])));
        assert!(space_pressed_once(&key_events(&[(Space, false)])));
        assert!(!space_pressed_once(&key_events(&[(Space, true)])));
        assert!(!space_pressed_once(&key_events(&[(Enter, false)])));
    }

    #[test]
    fn una_rafaga_de_mayus_flechas_amplia_y_encoge_sobre_lo_marcado_antes() {
        let order: Vec<NodeId> = (1..=8).collect();
        let mut m = Marks::default();
        m.toggle(7); // ya estaba marcado antes de la ráfaga
        m.anchor = Some(2);
        let run = |m: &mut Marks, to: NodeId| {
            let r = range_between(&order, 2, to).to_vec();
            m.set_run(&r);
        };
        run(&mut m, 3);
        assert_eq!(m.sorted(), vec![2, 3, 7]);
        run(&mut m, 5);
        assert_eq!(m.sorted(), vec![2, 3, 4, 5, 7]);
        run(&mut m, 4);
        assert_eq!(
            m.sorted(),
            vec![2, 3, 4, 7],
            "encoge y conserva lo de antes"
        );
        // Cualquier otra acción acaba la ráfaga: la siguiente parte de lo que hay.
        m.toggle(8);
        run(&mut m, 3);
        assert_eq!(m.sorted(), vec![2, 3, 4, 7, 8]);
    }
}
