//! Vista de lista: la vista accesible, sincronizada con el mapa. Cada fila lleva una etiqueta accesible completa
//! (nombre, "≥" si es un mínimo, tamaño, porcentaje del padre y número de archivos) y comparte la selección con el
//! mapa y los detalles. Con muchas filas solo se pintan las visibles.

use crate::app::Env;
use crate::args;
use crate::describe::{self, Special};
use crate::explore::{Explore, Sel, node_icon};
use crate::icons::Icon;
use crate::map::menu_items;
use crate::multisel::{CHECK_W, paint_checkbox};
use crate::theme;
use crate::widgets;
use egui::{Align2, CornerRadius, FontId, Rect, Sense, Stroke, Ui, pos2, vec2};
use tesela_model::NodeId;

const ROW_H: f32 = 32.0;

/// Columnas de la lista según el ancho de la fila. El NOMBRE manda: las demás columnas se ocultan por orden (primero
/// "archivos", luego la barra y el porcentaje) hasta que el nombre tenga sitio. Todas las posiciones son distancias al
/// borde DERECHO de la fila.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Cols {
    /// Ancho que le queda a la columna del nombre (desde `NAME_LEFT`).
    pub name_w: f32,
    /// Borde derecho de "Tamaño" (distancia al borde derecho de la fila).
    pub size_right: f32,
    /// Inicio de la barra y el porcentaje (distancia al borde derecho), si se muestran.
    pub bar_left: Option<f32>,
    /// Borde derecho de "Archivos" (distancia al borde derecho), si se muestra.
    pub files_right: Option<f32>,
}

/// Dónde empieza el nombre (tras el icono).
pub(crate) const NAME_LEFT: f32 = 36.0;
/// Ancho mínimo que se garantiza al nombre mientras se pueda quitar otra columna.
pub(crate) const NAME_MIN_W: f32 = 150.0;

/// Reparte el ancho `width` de la fila.
pub(crate) fn columns(width: f32) -> Cols {
    // (columna "archivos", barra) → anchura que ocupan las columnas de la derecha.
    const FULL: f32 = 320.0;
    const NO_FILES: f32 = 242.0;
    const SIZE_ONLY: f32 = 120.0;
    let name_w = |right: f32| (width - NAME_LEFT - right).max(0.0);
    if name_w(FULL) >= NAME_MIN_W {
        Cols {
            name_w: name_w(FULL),
            size_right: 110.0,
            bar_left: Some(300.0),
            files_right: Some(12.0),
        }
    } else if name_w(NO_FILES) >= NAME_MIN_W {
        Cols {
            name_w: name_w(NO_FILES),
            size_right: 12.0,
            bar_left: Some(222.0),
            files_right: None,
        }
    } else {
        Cols {
            name_w: name_w(SIZE_ONLY),
            size_right: 12.0,
            bar_left: None,
            files_right: None,
        }
    }
}

enum Items {
    Children(crate::explore::Shown),
    Hits(std::sync::Arc<Vec<NodeId>>),
}

impl Items {
    fn len(&self) -> usize {
        match self {
            Items::Children(c) => c.len(),
            Items::Hits(h) => h.len(),
        }
    }
    fn get(&self, i: usize) -> NodeId {
        match self {
            Items::Children(c) => c.get(i),
            Items::Hits(h) => h[i],
        }
    }
}

/// Pinta la lista de los elementos de la carpeta actual.
pub fn show(ui: &mut Ui, env: &mut Env, ex: &mut Explore) {
    let pal = env.pal;
    let items = match ex.search_matches() {
        Some(h) => Items::Hits(h),
        None => Items::Children(ex.shown_children()),
    };
    let n = items.len();
    let total: u64 = ex.view().alloc(ex.root.node).max(1);
    let tree = ex.tree.clone();
    // Con el modo "Seleccionar" activo o con algo marcado aparece la columna de casillas (y el resto se desplaza).
    let checks = ex.multi.mode || !ex.multi.is_empty();
    let off = if checks { CHECK_W } else { 0.0 };

    // Cabecera de columnas.
    let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    let cols = columns(hr.width() - off);
    let hdr = |text: String, x: f32, anchor: Align2| {
        ui.painter().text(
            pos2(x, hr.center().y),
            anchor,
            text,
            theme::font_semibold(12.0),
            pal.text_muted,
        );
    };
    hdr(
        env.tr.t("list-col-name"),
        hr.left() + NAME_LEFT + off,
        Align2::LEFT_CENTER,
    );
    if let Some(b) = cols.bar_left {
        hdr(
            env.tr.t("list-col-share"),
            hr.right() - b,
            Align2::LEFT_CENTER,
        );
    }
    hdr(
        env.tr.t("list-col-size"),
        hr.right() - cols.size_right,
        Align2::RIGHT_CENTER,
    );
    if let Some(f) = cols.files_right {
        hdr(
            env.tr.t("list-col-files"),
            hr.right() - f,
            Align2::RIGHT_CENTER,
        );
    }
    ui.painter()
        .hline(hr.x_range(), hr.bottom(), Stroke::new(1.0, pal.border));
    if checks && n > 0 {
        // Casilla de la cabecera: marca o desmarca todo lo que se ve (la vía con ratón de "todo menos uno").
        let marked_here = (0..n).filter(|&i| ex.multi.contains(items.get(i))).count();
        let all = marked_here == n;
        let cb = Rect::from_center_size(pos2(hr.left() + 16.0, hr.center().y), vec2(16.0, 16.0));
        let hit = ui.interact(
            cb.expand2(vec2(4.0, 6.0)),
            ui.id().with("casilla-cabecera"),
            Sense::click(),
        );
        let label = env.tr.t("list-select-all-rows");
        hit.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, all, label.clone())
        });
        crate::multisel::paint_checkbox(ui.painter(), env, cb, all);
        if marked_here > 0 && !all {
            // Estado mixto: una raya en la casilla.
            ui.painter().hline(
                cb.shrink2(vec2(4.0, 0.0)).x_range(),
                cb.center().y,
                Stroke::new(2.0, pal.text_muted),
            );
        }
        if hit.has_focus() {
            widgets::focus_ring(ui.painter(), &pal, cb.expand(3.0), 6);
        }
        if hit.clicked() {
            let ids: Vec<NodeId> = (0..n).map(|i| items.get(i)).collect();
            if all {
                ex.multi.remove_all(&ids);
            } else {
                ex.multi.add(ids);
            }
        }
        hit.on_hover_text(label);
    }

    if n == 0 {
        ui.add_space(24.0);
        let msg = if ex.search.trim().is_empty() {
            env.tr.t("list-empty")
        } else {
            env.tr.t("list-no-results")
        };
        ui.label(egui::RichText::new(msg).color(pal.text_muted));
        return;
    }

    let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
    if ex.scroll_to_selection {
        ex.scroll_to_selection = false;
        if let Some(Sel::Node(sel)) = ex.selection
            && let Some(i) = (0..n).find(|&i| items.get(i) == sel)
        {
            scroll = scroll.vertical_scroll_offset((i as f32 * ROW_H - 3.0 * ROW_H).max(0.0));
        }
    }
    let mut entered = false;
    let mut focused: Option<usize> = None;
    let want_focus = ex.list_focus.take();
    scroll.show_rows(ui, ROW_H, n, |ui, range| {
        for i in range.clone() {
            if entered {
                break;
            }
            let id = items.get(i);
            let node = *tree.node(id);
            let node_alloc = ex.view().alloc(id);
            let node_files = ex.view().files_below(id);
            let sel = Sel::Node(id);
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
            let resp = ui.interact(rect, ui.id().with(("fila", id)), Sense::click());
            if want_focus == Some(id) {
                // La fila a la que acaba de llegar el teclado (flecha arriba/abajo): se enfoca y se deja a la vista.
                resp.request_focus();
                ui.scroll_to_rect(rect, None);
            }
            if resp.has_focus() {
                focused = Some(i);
                ex.nav_focus = Some(resp.id);
            }
            if resp.gained_focus() {
                // Con Tab la selección sigue al foco: así Intro abre justo la fila enfocada.
                ex.selection = Some(sel);
                ui.scroll_to_rect(rect, None);
            }
            let selected = ex.selection == Some(sel);
            let marked = ex.multi.contains(id);
            let lower = describe::is_lower_bound(&tree, id);
            let name = describe::display_name(&tree, id, env.tr);
            let size_txt = if lower {
                env.tr.size_min(node_alloc)
            } else {
                env.tr.size(node_alloc)
            };
            let share = 100.0 * node_alloc as f64 / total as f64;
            let files = if node.is_dir() {
                Some(node_files)
            } else {
                None
            };
            let label = {
                let mut s = format!(
                    "{name}, {size_txt}, {}",
                    env.tr
                        .ta("share-of-folder", args!["pct" => env.tr.pct_fine(share)])
                );
                if let Some(f) = files {
                    s.push_str(", ");
                    s.push_str(
                        &env.tr
                            .ta("files-count", args!["n" => f, "count" => env.tr.int(f)]),
                    );
                }
                if describe::special(&tree, id) == Some(Special::SystemBlock) {
                    s.push_str(", ");
                    s.push_str(&env.tr.t("system-tip"));
                }
                if describe::special(&tree, id).is_none() {
                    let key = tesela_cleaner::paths::PathKey::new(&tree.path(id));
                    if ex.protection.target_verdict(&key) != tesela_cleaner::Verdict::Allowed {
                        s.push_str(", ");
                        s.push_str(&env.tr.t("protected-label"));
                    }
                    if ex.flow.basket.covers(&key) {
                        s.push_str(", ");
                        s.push_str(&env.tr.t("details-in-basket"));
                    }
                }
                s
            };
            // Con las casillas visibles la fila es una casilla y su estado ("marcada") lo da `toggled`, no la etiqueta; sin
            // ellas, el de siempre (`toggled` = elemento activo). El elemento activo se expone siempre como `selected`.
            resp.widget_info(|| {
                if checks {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::Checkbox,
                        true,
                        marked,
                        label.clone(),
                    )
                } else {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::SelectableLabel,
                        true,
                        selected,
                        label.clone(),
                    )
                }
            });
            ui.ctx()
                .accesskit_node_builder(resp.id, |node| node.set_selected(selected));
            if ui.is_rect_visible(rect) {
                if selected {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), pal.accent_soft);
                    ui.painter().rect_filled(
                        Rect::from_min_size(rect.min, vec2(3.0, rect.height())),
                        CornerRadius::same(2),
                        pal.accent,
                    );
                } else if marked {
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::same(6),
                        crate::multisel::mark_tint(&pal).gamma_multiply(0.45),
                    );
                } else if resp.hovered() {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), pal.surface_alt);
                }
                if checks {
                    paint_checkbox(
                        ui.painter(),
                        env,
                        Rect::from_center_size(
                            pos2(rect.left() + 16.0, rect.center().y),
                            vec2(16.0, 16.0),
                        ),
                        marked,
                    );
                }
                if resp.has_focus() {
                    widgets::focus_ring(ui.painter(), &pal, rect, 6);
                }
                let cy = rect.center().y;
                let icon = node_icon(&tree, id);
                let icon_col = match describe::special(&tree, id) {
                    Some(_) => pal.text_muted,
                    None if node.is_dir() => pal.accent,
                    None => pal.text_muted,
                };
                env.icons.paint(
                    ui.painter(),
                    Rect::from_center_size(pos2(rect.left() + 20.0 + off, cy), vec2(16.0, 16.0)),
                    icon,
                    icon_col,
                );
                let name_rect = widgets::clipped_text(
                    ui.painter(),
                    pos2(rect.left() + NAME_LEFT + off, cy),
                    Align2::LEFT_CENTER,
                    &name,
                    if selected {
                        theme::font_semibold(14.0)
                    } else {
                        FontId::proportional(14.0)
                    },
                    pal.text,
                    cols.name_w,
                );
                // Candado si está protegido (no se puede añadir a la cesta) y carrito si ya está en ella.
                let (protected, in_basket) = if describe::special(&tree, id).is_none() {
                    let key = tesela_cleaner::paths::PathKey::new(&tree.path(id));
                    (
                        ex.protection.target_verdict(&key) != tesela_cleaner::Verdict::Allowed,
                        ex.flow.basket.covers(&key),
                    )
                } else {
                    (false, false)
                };
                let mut x = name_rect.right() + 8.0;
                for (show, icon, col) in [
                    (protected, Icon::LockClosed, pal.text_muted),
                    (in_basket, Icon::Cart, pal.accent),
                ] {
                    if show && x + 16.0 < rect.right() - cols.size_right - 60.0 {
                        env.icons.paint(
                            ui.painter(),
                            Rect::from_min_size(pos2(x, cy - 8.0), vec2(16.0, 16.0)),
                            icon,
                            col,
                        );
                        x += 22.0;
                    }
                }
                // Barra de cuota y porcentaje (se ocultan si el nombre no tiene sitio).
                if let Some(b) = cols.bar_left {
                    let bar =
                        Rect::from_min_size(pos2(rect.right() - b, cy - 4.0), vec2(90.0, 8.0));
                    ui.painter()
                        .rect_filled(bar, CornerRadius::same(4), pal.track);
                    let fw = (bar.width() * (share / 100.0) as f32).max(if node_alloc > 0 {
                        2.0
                    } else {
                        0.0
                    });
                    let color = if describe::special(&tree, id).is_some() {
                        pal.info_only()
                    } else {
                        pal.accent
                    };
                    ui.painter().rect_filled(
                        Rect::from_min_size(bar.min, vec2(fw.min(bar.width()), 8.0)),
                        CornerRadius::same(4),
                        color,
                    );
                    ui.painter().text(
                        pos2(bar.right() + 8.0, cy),
                        Align2::LEFT_CENTER,
                        env.tr.pct_fine(share),
                        FontId::proportional(12.0),
                        pal.text_muted,
                    );
                }
                ui.painter().text(
                    pos2(rect.right() - cols.size_right, cy),
                    Align2::RIGHT_CENTER,
                    &size_txt,
                    theme::font_semibold(13.0),
                    pal.text,
                );
                if let (Some(f), Some(fr)) = (files, cols.files_right) {
                    ui.painter().text(
                        pos2(rect.right() - fr, cy),
                        Align2::RIGHT_CENTER,
                        env.tr.int(f),
                        FontId::proportional(13.0),
                        pal.text_muted,
                    );
                }
            }
            if resp.clicked() || resp.secondary_clicked() {
                ex.selection = Some(sel);
            }
            if resp.secondary_clicked() {
                ex.ctx_target = Some(sel);
            }
            // Selección múltiple. Con el puntero: Mayús+clic marca el rango desde el ancla, Ctrl+clic (o el modo "Seleccionar",
            // o pulsar la casilla) alterna la fila y un clic normal solo fija el ancla. Con el teclado, Espacio alterna.
            let mods = ui.input(|i| i.modifiers);
            if resp.clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    // Un clic de ratón enfoca la fila: así Espacio y Mayús+flechas funcionan sin pasar por Tab.
                    resp.request_focus();
                    let on_check = checks && p.x < rect.left() + off;
                    if mods.shift {
                        let order: Vec<NodeId> = (0..n).map(|k| items.get(k)).collect();
                        ex.marks_range_in(&order, id);
                    } else if mods.command || ex.multi.mode || on_check {
                        ex.marks_toggle(sel);
                    } else {
                        ex.multi.plain_move(id);
                    }
                } else if ui.input(crate::multisel::keyboard_click_toggles) {
                    // Sin puntero: el clic por defecto de un lector de pantalla o la pulsación de Espacio alternan la marca
                    // (Intro entra en la carpeta y la autorrepetición de Espacio no vuelve a alternar).
                    ex.marks_toggle(sel);
                }
            }
            // Con Ctrl o Mayús un doble clic son dos marcas seguidas, no "entrar".
            if resp.double_clicked() && !(mods.command || mods.shift) {
                entered = ex.enter(sel);
            }
            let target = ex.ctx_target;
            egui::Popup::context_menu(&resp).show(|ui| {
                if let Some(t) = target {
                    menu_items(ui, env, ex, t);
                }
            });
        }
        // Teclado: con una fila enfocada, ↑/↓ (y Re Pág, Av Pág, Inicio, Fin) mueven la selección y el foco.
        let shift = ui.input(|i| i.modifiers.shift);
        if let Some(fi) = focused
            && !entered
            && let Some(to) = ui.ctx().input_mut(|i| key_target(i, fi, n))
            && to != fi
        {
            ex.selection = Some(Sel::Node(items.get(to)));
            ex.list_focus = Some(items.get(to));
            if shift {
                // Mayús+flechas: lo marcado es lo de antes más el rango desde el ancla (la fila de la que se salió, si aún
                // no hay ancla); con la flecha contraria el rango encoge.
                if ex.multi.anchor.is_none() {
                    ex.multi.anchor = Some(items.get(fi));
                }
                let order: Vec<NodeId> = (0..n).map(|k| items.get(k)).collect();
                ex.marks_shift_extend(&order, items.get(to));
            } else {
                ex.multi.plain_move(items.get(to));
            }
            if to < range.start || to >= range.end {
                ex.scroll_to_selection = true;
            }
        }
    });
}

/// Fila a la que lleva la tecla de navegación pulsada (y la consume), o `None` si no se pulsó ninguna.
fn key_target(i: &mut egui::InputState, from: usize, n: usize) -> Option<usize> {
    use egui::{Key, Modifiers};
    let last = n.saturating_sub(1);
    if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
        Some((from + 1).min(last))
    } else if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
        Some(from.saturating_sub(1))
    } else if i.consume_key(Modifiers::NONE, Key::PageDown) {
        Some((from + 10).min(last))
    } else if i.consume_key(Modifiers::NONE, Key::PageUp) {
        Some(from.saturating_sub(10))
    } else if i.consume_key(Modifiers::NONE, Key::Home) {
        Some(0)
    } else if i.consume_key(Modifiers::NONE, Key::End) {
        Some(last)
    } else {
        None
    }
}
