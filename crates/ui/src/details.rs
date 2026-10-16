//! Panel de detalles (PLAN.md §2.3): en disco frente a tamaño lógico, archivos, porcentaje del padre, desglose por
//! tipo, frase de antigüedad, avisos de la clase del nodo (bloque del sistema, "solo informar", nube, incompleto) y
//! acciones. Sin selección muestra la carpeta actual.

use crate::app::Env;
use crate::args;
use crate::describe::{self, Special};
use crate::explore::{AddState, Explore, Sel};
use crate::icons::Icon;
use crate::map::{info_key, stacked_bar};
use crate::platform;
use crate::theme::{self, Palette};
use crate::widgets::{self, ButtonStyle};
use egui::{RichText, Ui};
use tesela_model::{NodeId, flags};

fn stat(ui: &mut Ui, pal: &Palette, label: &str, value: String, big: bool) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(RichText::new(label).size(12.0).color(pal.text_muted));
        let t = RichText::new(value).color(pal.text);
        ui.label(if big {
            t.font(theme::font_semibold(18.0))
        } else {
            t.size(15.0)
        });
    });
}

fn note(ui: &mut Ui, env: &mut Env, icon: Icon, text: String, color: egui::Color32) {
    ui.horizontal_top(|ui| {
        env.icons.show(ui, icon, 16.0, color);
        ui.add(egui::Label::new(RichText::new(text).size(13.0).color(env.pal.text_muted)).wrap());
    });
}

/// Pinta el panel.
pub fn show(ui: &mut Ui, env: &mut Env, ex: &mut Explore) {
    let pal = env.pal;
    let tree = ex.tree.clone();
    let overlay = ex.overlay.clone();
    let view = tesela_model::View::new(&tree, overlay.as_deref());
    let sel = ex.selection.unwrap_or(Sel::Node(ex.root.node));
    let is_current = ex.selection.is_none();
    // Barra SÓLIDA y siempre visible cuando el contenido no cabe (ventana pequeña): nada queda cortado sin aviso.
    ui.style_mut().spacing.scroll = egui::style::ScrollStyle::solid();
    let out = egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            widgets::section_label(
                ui,
                &pal,
                &env.tr.t(if is_current {
                    "details-folder"
                } else {
                    "details-selection"
                }),
            );
            let (id, group) = match sel {
                Sel::Node(id) => (id, None),
                Sel::Group(o) => (o.parent, Some(o)),
            };
            let node = *tree.node(id);
            let special = if group.is_none() {
                describe::special(&tree, id)
            } else {
                None
            };
            let lower = group.is_none() && describe::is_lower_bound(&tree, id);

            // Título y ruta.
            let title = match group {
                Some(o) => env.tr.ta(
                    "others-title",
                    args!["n" => env.tr.int(o.count as u64), "size" => env.tr.size(o.bytes)],
                ),
                None => {
                    let n = describe::display_name(&tree, id, env.tr);
                    if id == 0 {
                        n.trim_end_matches('\\').to_string()
                    } else {
                        n
                    }
                }
            };
            ui.label(
                RichText::new(&title)
                    .font(theme::font_semibold(20.0))
                    .color(pal.text),
            );
            if special != Some(Special::SystemBlock) {
                ui.label(
                    RichText::new(ex.path_of(sel))
                        .family(egui::FontFamily::Monospace)
                        .size(12.0)
                        .color(pal.text_muted),
                );
            }
            if ex.info.approximate {
                let tip = env.tr.t("approx-tip");
                widgets::chip(
                    ui,
                    env,
                    Some(Icon::Info),
                    &env.tr.t("approx-badge"),
                    pal.review,
                    pal.accent_soft,
                )
                .on_hover_text(tip);
            }

            // Cifras.
            let bytes = group.map_or(view.alloc(id), |o| o.bytes);
            let alloc_txt = if lower {
                env.tr.size_min(bytes)
            } else {
                env.tr.size(bytes)
            };
            let logical_txt = if group.is_some() || special.is_some() && view.logical(id) == 0 {
                "—".to_string()
            } else if lower {
                env.tr.size_min(view.logical(id))
            } else {
                env.tr.size(view.logical(id))
            };
            let files_txt = match (group, node.is_dir()) {
                (None, true) => {
                    let f = env.tr.int(view.files_below(id));
                    if lower { format!("≥ {f}") } else { f }
                }
                _ => "—".to_string(),
            };
            let (parent_alloc, parent_name) = match tree.parent(id) {
                Some(p) if group.is_none() => {
                    (view.alloc(p), describe::display_name(&tree, p, env.tr))
                }
                Some(p) => (view.alloc(p), describe::display_name(&tree, p, env.tr)),
                None => (0, String::new()),
            };
            let share = if parent_alloc > 0 {
                100.0 * bytes as f64 / parent_alloc as f64
            } else {
                100.0
            };
            let share_txt = if tree.parent(id).is_some() {
                env.tr.pct_fine(share)
            } else {
                env.tr.pct(100.0)
            };
            let parent_label = if tree.parent(id).is_some() {
                env.tr.ta("details-of", args!["name" => parent_name])
            } else {
                env.tr.t("details-of-disk")
            };
            ui.columns(2, |cols| {
                stat(
                    &mut cols[0],
                    &pal,
                    &env.tr.t("details-on-disk"),
                    alloc_txt,
                    true,
                );
                stat(
                    &mut cols[1],
                    &pal,
                    &env.tr.t("details-size"),
                    logical_txt,
                    false,
                );
            });
            ui.columns(2, |cols| {
                stat(
                    &mut cols[0],
                    &pal,
                    &env.tr.t("details-files"),
                    files_txt,
                    false,
                );
                stat(&mut cols[1], &pal, &parent_label, share_txt, false);
            });

            // Composición y antigüedad (solo carpetas con datos).
            if group.is_none() && node.is_dir() && view.alloc(id) > 0 && special.is_none() {
                let h = tree.hist(id);
                stacked_bar(ui, &pal, h);
                let tp = describe::type_phrase(env.tr, h);
                if !tp.is_empty() {
                    ui.label(RichText::new(tp).size(13.0).color(pal.text_muted));
                }
                if let Some(ap) = describe::age_phrase(env.tr, h) {
                    ui.label(RichText::new(ap).size(13.0).color(pal.text_muted));
                }
            }

            // Avisos por clase.
            match special {
                Some(Special::SystemBlock) => {
                    note(ui, env, Icon::Info, env.tr.t("system-tip"), pal.text_muted);
                    ui.add(egui::Label::new(
                        RichText::new(env.tr.t("system-admin-link"))
                            .size(13.0)
                            .color(pal.text_muted),
                    ))
                    .on_hover_text(env.tr.t("next-version"));
                }
                Some(Special::InfoOnly(k)) => {
                    widgets::chip(
                        ui,
                        env,
                        Some(Icon::Warning),
                        &env.tr.t("risk-info-only"),
                        pal.danger,
                        pal.accent_soft,
                    );
                    note(ui, env, Icon::Warning, env.tr.t(info_key(k)), pal.danger);
                }
                None => {}
            }
            if lower {
                note(
                    ui,
                    env,
                    Icon::Warning,
                    env.tr.t("incomplete-note"),
                    pal.review,
                );
            }
            if group.is_none() && node.flags & flags::CLOUD != 0 {
                note(ui, env, Icon::Cloud, env.tr.t("cloud-note"), pal.accent);
            }
            // Protección y cesta.
            let state = ex.add_state(sel);
            match state {
                AddState::Denied => {
                    note(
                        ui,
                        env,
                        Icon::LockClosed,
                        env.tr.t("protected-denied-why"),
                        pal.text_muted,
                    );
                }
                AddState::Untouchable => {
                    note(
                        ui,
                        env,
                        Icon::LockClosed,
                        env.tr.t("protected-untouchable-why"),
                        pal.text_muted,
                    );
                }
                AddState::ContentsOf => {
                    note(
                        ui,
                        env,
                        Icon::Info,
                        env.tr.t("protected-contents-why"),
                        pal.text_muted,
                    );
                }
                AddState::CloudOnly => {
                    note(
                        ui,
                        env,
                        Icon::Cloud,
                        env.tr.t("menu-unavailable-cloud"),
                        pal.text_muted,
                    );
                }
                AddState::InBasket => {
                    widgets::chip(
                        ui,
                        env,
                        Some(Icon::Cart),
                        &env.tr.t("details-in-basket"),
                        pal.accent,
                        pal.accent_soft,
                    );
                }
                _ => {}
            }
            if group.is_some() {
                note(
                    ui,
                    env,
                    Icon::Info,
                    env.tr.t("tip-open-group"),
                    pal.text_muted,
                );
            }

            // Acciones.
            ui.add_space(4.0);
            let (label, enabled, hint) = match state {
                AddState::Addable => (env.tr.t("menu-add-basket"), true, None),
                AddState::ContentsOf => (env.tr.t("menu-add-contents-short"), true, None),
                AddState::InBasket if ex.basket_has_own(sel) => {
                    (env.tr.t("menu-remove-basket"), true, None)
                }
                AddState::InBasket => (env.tr.t("menu-in-basket"), false, None),
                AddState::Untouchable => (
                    env.tr.t("menu-add-basket"),
                    false,
                    Some("protected-untouchable-why"),
                ),
                AddState::Denied => (
                    env.tr.t("menu-add-basket"),
                    false,
                    Some("protected-denied-why"),
                ),
                AddState::InfoOnly => (
                    env.tr.t("menu-add-basket"),
                    false,
                    Some("menu-unavailable-info"),
                ),
                AddState::SystemBlock => (
                    env.tr.t("menu-add-basket"),
                    false,
                    Some("menu-unavailable-system"),
                ),
                AddState::CloudOnly => (
                    env.tr.t("menu-add-basket"),
                    false,
                    Some("menu-unavailable-cloud"),
                ),
                AddState::Group => (env.tr.t("menu-add-basket"), false, Some("tip-open-group")),
            };
            let r = widgets::button(
                ui,
                env,
                ButtonStyle::Secondary,
                Some(Icon::Cart),
                &label,
                enabled,
            );
            if let Some(h) = hint {
                r.clone().on_hover_text(env.tr.t(h));
            }
            if r.clicked() {
                let now = ui.input(|i| i.time);
                if state == AddState::InBasket {
                    ex.basket_remove(sel);
                } else {
                    ex.basket_add(sel, env, now);
                }
            }
            let is_system = special == Some(Special::SystemBlock);
            let real = !env.demo && !is_system;
            ui.horizontal_wrapped(|ui| {
                let r = widgets::button(
                    ui,
                    env,
                    ButtonStyle::Secondary,
                    Some(Icon::Open),
                    &env.tr.t("menu-open-explorer"),
                    real,
                );
                let r = if !real {
                    r.on_hover_text(if env.demo {
                        env.tr.t("demo-unavailable")
                    } else {
                        env.tr.t("menu-unavailable-system")
                    })
                } else {
                    r
                };
                if r.clicked() {
                    ex.reveal(&tree.path(id), env.tr, ui.input(|i| i.time));
                }
                let r = widgets::button(
                    ui,
                    env,
                    ButtonStyle::Secondary,
                    Some(Icon::Copy),
                    &env.tr.t("menu-copy-path"),
                    !is_system,
                );
                if is_system {
                    r.clone().on_hover_text(env.tr.t("menu-unavailable-system"));
                }
                if r.clicked() {
                    ui.ctx().copy_text(ex.path_of(sel));
                    ex.toast = Some((env.tr.t("path-copied"), ui.input(|i| i.time) + 2.0));
                }
                let r = widgets::button(
                    ui,
                    env,
                    ButtonStyle::Secondary,
                    None,
                    &env.tr.t("menu-properties"),
                    real,
                );
                if !real {
                    r.on_hover_text(if env.demo {
                        env.tr.t("demo-unavailable")
                    } else {
                        env.tr.t("menu-unavailable-system")
                    });
                } else if r.clicked() {
                    platform::show_properties_async(tree.path(id));
                }
            });

            // Lo más grande aquí.
            if group.is_none() && describe::can_enter_view(&view, id) {
                ui.add_space(6.0);
                widgets::section_label(ui, &pal, &env.tr.t("details-biggest"));
                let kids = tesela_layout::Children::of_view(&view, id);
                for c in kids.iter_from(0).take(5) {
                    row_biggest(ui, env, &view, c);
                }
            }
        });
    ex.details_overflow = out.content_size.y > out.inner_rect.height() + 0.5;
}

fn row_biggest(ui: &mut Ui, env: &Env, view: &tesela_model::View, c: NodeId) {
    let pal = env.pal;
    let tree = view.tree();
    let size = if describe::is_lower_bound(tree, c) {
        env.tr.size_min(view.alloc(c))
    } else {
        env.tr.size(view.alloc(c))
    };
    ui.horizontal(|ui| {
        ui.add(
            egui::Label::new(
                RichText::new(describe::display_name(tree, c, env.tr))
                    .size(13.0)
                    .color(pal.text),
            )
            .truncate(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(size).size(13.0).color(pal.text));
        });
    });
}
