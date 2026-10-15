//! Vista de lista: la vista accesible, sincronizada con el mapa. Cada fila lleva una etiqueta accesible completa
//! (nombre, "≥" si es un mínimo, tamaño, porcentaje del padre y número de archivos) y comparte la selección con el
//! mapa y los detalles. Con muchas filas solo se pintan las visibles.

use crate::app::Env;
use crate::args;
use crate::describe::{self, Special};
use crate::explore::{Explore, Sel, node_icon};
use crate::map::menu_items;
use crate::theme;
use crate::widgets;
use egui::{Align2, CornerRadius, FontId, Rect, Sense, Stroke, Ui, pos2, vec2};
use tesela_model::NodeId;

const ROW_H: f32 = 32.0;

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
    let total: u64 = ex.tree.node(ex.root.node).alloc.max(1);
    let tree = ex.tree.clone();

    // Cabecera de columnas.
    let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
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
        hr.left() + 36.0,
        Align2::LEFT_CENTER,
    );
    hdr(
        env.tr.t("list-col-share"),
        hr.right() - 300.0,
        Align2::LEFT_CENTER,
    );
    hdr(
        env.tr.t("list-col-size"),
        hr.right() - 110.0,
        Align2::RIGHT_CENTER,
    );
    hdr(
        env.tr.t("list-col-files"),
        hr.right() - 12.0,
        Align2::RIGHT_CENTER,
    );
    ui.painter()
        .hline(hr.x_range(), hr.bottom(), Stroke::new(1.0, pal.border));

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
    scroll.show_rows(ui, ROW_H, n, |ui, range| {
        for i in range {
            if entered {
                break;
            }
            let id = items.get(i);
            let node = *tree.node(id);
            let sel = Sel::Node(id);
            let selected = ex.selection == Some(sel);
            let (rect, resp) =
                ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
            let lower = describe::is_lower_bound(&tree, id);
            let name = describe::display_name(&tree, id, env.tr);
            let size_txt = if lower {
                env.tr.size_min(node.alloc)
            } else {
                env.tr.size(node.alloc)
            };
            let share = 100.0 * node.alloc as f64 / total as f64;
            let files = if node.is_dir() {
                Some(node.files_below as u64)
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
                s
            };
            resp.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::SelectableLabel,
                    true,
                    selected,
                    label.clone(),
                )
            });
            if ui.is_rect_visible(rect) {
                if selected {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), pal.accent_soft);
                    ui.painter().rect_filled(
                        Rect::from_min_size(rect.min, vec2(3.0, rect.height())),
                        CornerRadius::same(2),
                        pal.accent,
                    );
                } else if resp.hovered() {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), pal.surface_alt);
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
                    Rect::from_center_size(pos2(rect.left() + 20.0, cy), vec2(16.0, 16.0)),
                    icon,
                    icon_col,
                );
                widgets::clipped_text(
                    ui.painter(),
                    pos2(rect.left() + 36.0, cy),
                    Align2::LEFT_CENTER,
                    &name,
                    if selected {
                        theme::font_semibold(14.0)
                    } else {
                        FontId::proportional(14.0)
                    },
                    pal.text,
                    rect.width() - 36.0 - 320.0,
                );
                // Barra de cuota y porcentaje.
                let bar =
                    Rect::from_min_size(pos2(rect.right() - 300.0, cy - 4.0), vec2(90.0, 8.0));
                ui.painter()
                    .rect_filled(bar, CornerRadius::same(4), pal.track);
                let fw = (bar.width() * (share / 100.0) as f32).max(if node.alloc > 0 {
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
                ui.painter().text(
                    pos2(rect.right() - 110.0, cy),
                    Align2::RIGHT_CENTER,
                    &size_txt,
                    theme::font_semibold(13.0),
                    pal.text,
                );
                if let Some(f) = files {
                    ui.painter().text(
                        pos2(rect.right() - 12.0, cy),
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
            if resp.double_clicked() {
                entered = ex.enter(sel);
            }
            let target = ex.ctx_target;
            egui::Popup::context_menu(&resp).show(|ui| {
                if let Some(t) = target {
                    menu_items(ui, env, ex, t);
                }
            });
        }
    });
}
