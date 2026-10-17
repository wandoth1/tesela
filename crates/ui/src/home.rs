//! Pantalla de inicio: tarjetas por unidad con anillo de uso y "Analizar una carpeta…" (PLAN.md §2.2).

use crate::app::{Action, Env, LangPref, ScanRequest};
use crate::args;
use crate::icons::Icon;
use crate::platform::{DriveInfo, DriveKind};
use crate::theme::{self, Palette, ThemePref};
use crate::widgets::{self, ButtonStyle};
use egui::{
    Align, Color32, CornerRadius, Layout, Pos2, Rect, RichText, Sense, Shape, Stroke, StrokeKind,
    Ui, Vec2, vec2,
};

/// Anillo de uso: pista y arco del `frac` (0..=1) empezando arriba.
pub fn ring(
    painter: &egui::Painter,
    center: Pos2,
    radius: f32,
    width: f32,
    frac: f32,
    color: Color32,
    track: Color32,
) {
    painter.circle_stroke(center, radius, Stroke::new(width, track));
    let frac = frac.clamp(0.0, 1.0);
    if frac <= 0.0 {
        return;
    }
    let n = ((frac * 96.0).ceil() as usize).max(2);
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let a =
                -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * frac * i as f32 / n as f32;
            center + radius * vec2(a.cos(), a.sin())
        })
        .collect();
    painter.add(Shape::line(pts, Stroke::new(width, color)));
}

/// Nombre de una unidad para mostrar: "Sistema (C:)". Sin etiqueta, "Disco local (C:)".
pub fn drive_title(env: &Env, d: &DriveInfo) -> String {
    drive_title_plain(env.tr, d)
}

/// Como [`drive_title`], sin necesitar un `Env`.
pub fn drive_title_plain(tr: &crate::i18n::Tr, d: &DriveInfo) -> String {
    let label = if d.label.is_empty() {
        tr.t(match d.kind {
            DriveKind::Fixed => "drive-local-disk",
            DriveKind::Removable => "drive-removable-disk",
            DriveKind::Network => "drive-network-disk",
        })
    } else {
        d.label.clone()
    };
    format!("{label} ({}:)", d.letter)
}

/// Ancho máximo del menú de ajustes (el texto de la comprobación de actualizaciones se parte en líneas).
const UPDATE_MENU_WIDTH: f32 = 340.0;

/// Altura de la franja superior del inicio (la del botón de ajustes y la del aviso de versión nueva).
const TOP_BAND: f32 = 36.0;

/// Botón de ajustes con su menú (tema, idioma y comprobación de actualizaciones).
pub fn settings_button(ui: &mut Ui, env: &mut Env) {
    let label = env.tr.t("settings");
    let resp = widgets::icon_button(ui, env, Icon::Settings, &label, true);
    let (tr, settings, history, update, pal) =
        (env.tr, env.settings, env.history, env.update, env.pal);
    let mut acts: Vec<Action> = Vec::new();
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(200.0);
        // El texto explicativo de abajo se parte en líneas en vez de ensanchar el menú.
        ui.set_max_width(UPDATE_MENU_WIDTH);
        ui.label(RichText::new(tr.t("settings-theme")).font(theme::font_semibold(12.0)));
        for (pref, key) in [
            (ThemePref::System, "theme-system"),
            (ThemePref::Light, "theme-light"),
            (ThemePref::Dark, "theme-dark"),
        ] {
            if ui
                .selectable_label(settings.theme == pref, tr.t(key))
                .clicked()
            {
                acts.push(Action::SetTheme(pref));
            }
        }
        ui.separator();
        ui.label(RichText::new(tr.t("settings-language")).font(theme::font_semibold(12.0)));
        for (pref, key) in [
            (LangPref::System, "lang-system"),
            (LangPref::Es, "lang-es"),
            (LangPref::En, "lang-en"),
        ] {
            if ui
                .selectable_label(settings.language == pref, tr.t(key))
                .clicked()
            {
                acts.push(Action::SetLanguage(pref));
            }
        }
        // La comprobación de actualizaciones: opcional y desactivada por defecto. Se explica ANTES de activarla.
        if update.available {
            ui.separator();
            let mut check = settings.update.check;
            if ui
                .checkbox(&mut check, tr.t("settings-update-check"))
                .changed()
            {
                acts.push(Action::SetCheckUpdates(check));
            }
            ui.label(
                RichText::new(tr.t("settings-update-explain"))
                    .size(12.0)
                    .color(pal.text_muted),
            );
            if update.failed {
                ui.label(
                    RichText::new(tr.t("update-failed"))
                        .size(12.0)
                        .color(pal.text_muted),
                );
            }
        }
        ui.separator();
        // El historial guarda solo resúmenes (fecha, unidad, usado, liberado, enviado a la Papelera): se puede borrar.
        let has_history = !history.is_empty();
        let clear = ui.add_enabled(
            has_history,
            egui::Button::new(tr.t("settings-clear-history")),
        );
        if clear.clicked() {
            acts.push(Action::ClearHistory);
            ui.close();
        }
        ui.separator();
        if ui.button(tr.t("about")).clicked() {
            acts.push(Action::ShowAbout);
            ui.close();
        }
    });
    env.actions.extend(acts);
}

fn card(ui: &mut Ui, env: &mut Env, d: &DriveInfo) {
    let pal: Palette = env.pal;
    let used = d.used_percent();
    let almost_full = used >= 90;
    let stroke = if almost_full {
        Stroke::new(2.0, pal.review)
    } else {
        Stroke::new(1.0, pal.border)
    };
    let title = drive_title(env, d);
    let frame = egui::Frame::new()
        .fill(pal.surface)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(24, 24));
    frame.show(ui, |ui| {
        ui.set_width(252.0);
        ui.set_min_height(292.0);
        ui.vertical_centered(|ui| {
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(120.0), Sense::hover());
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Label,
                    true,
                    env.tr.ta("drive-used", args!["pct" => used as u64]),
                )
            });
            let color = if almost_full { pal.review } else { pal.accent };
            ring(
                ui.painter(),
                rect.center(),
                48.0,
                12.0,
                used as f32 / 100.0,
                color,
                pal.track,
            );
            ui.painter().text(
                rect.center() - vec2(0.0, 8.0),
                egui::Align2::CENTER_CENTER,
                env.tr.pct(used as f64),
                theme::font_semibold(26.0),
                pal.text,
            );
            ui.painter().text(
                rect.center() + vec2(0.0, 16.0),
                egui::Align2::CENTER_CENTER,
                env.tr.t("drive-used-word"),
                egui::FontId::proportional(12.0),
                pal.text_muted,
            );
            ui.add_space(12.0);
            ui.label(
                RichText::new(&title)
                    .font(theme::font_semibold(17.0))
                    .color(pal.text),
            );
            ui.label(
                RichText::new(env.tr.ta(
                    "drive-free-of",
                    args!["free" => env.tr.size(d.free), "total" => env.tr.size(d.total)],
                ))
                .color(pal.text_muted),
            );
            // Con una cuota de disco, lo que el usuario puede usar es menos que lo libre del volumen: nota aparte.
            if !d.volume_scope {
                // Hay cuota y Windows no da la capacidad del volumen: las cifras de arriba son las de tu cuenta.
                ui.label(
                    RichText::new(env.tr.t("drive-quota-scope"))
                        .size(12.0)
                        .color(pal.text_muted),
                );
            } else if d.available < d.free {
                ui.label(
                    RichText::new(
                        env.tr
                            .ta("drive-quota", args!["avail" => env.tr.size(d.available)]),
                    )
                    .size(12.0)
                    .color(pal.text_muted),
                );
            }
            // La última limpieza de esta unidad (solo si hay historial).
            if let Some(h) = env.history.iter().rev().find(|e| e.drive != 0 && e.drive as char == d.letter) {
                let secs = crate::app::unix_now().saturating_sub(h.unix);
                ui.label(
                    RichText::new(env.tr.ta(
                        "home-last-clean",
                        args!["ago" => crate::describe::ago_phrase(env.tr, secs), "size" => env.tr.size(h.freed_now)],
                    ))
                    .size(12.0)
                    .color(pal.text_muted),
                );
            }
            ui.add_space(2.0);
            if almost_full {
                widgets::chip(
                    ui,
                    env,
                    None,
                    &env.tr.t("drive-almost-full"),
                    pal.review,
                    pal.accent_soft,
                );
            } else if d.kind != DriveKind::Fixed {
                ui.label(
                    RichText::new(env.tr.t(match d.kind {
                        DriveKind::Removable => "drive-removable-note",
                        _ => "drive-network-note",
                    }))
                    .size(12.0)
                    .color(pal.text_muted),
                );
            } else {
                ui.add_space(24.0);
            }
            ui.add_space(8.0);
            let style = if almost_full {
                ButtonStyle::Primary
            } else {
                ButtonStyle::Secondary
            };
            let label = env.tr.ta("analyze-drive", args!["name" => title.clone()]);
            let r = ui
                .scope(|ui| {
                    ui.set_width(252.0);
                    wide_button(ui, env, style, &label, &env.tr.t("analyze"))
                })
                .inner;
            if r.clicked() {
                env.actions.push(Action::StartScan(ScanRequest {
                    root: d.root(),
                    label: title.clone(),
                    drive: Some(d.clone()),
                }));
            }
        });
    });
}

/// Botón de ancho completo con el texto visible `text` y la etiqueta accesible `label` (más descriptiva).
fn wide_button(
    ui: &mut Ui,
    env: &mut Env,
    style: ButtonStyle,
    label: &str,
    text: &str,
) -> egui::Response {
    let pal = env.pal;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    let (fill, fg, stroke) = match style {
        ButtonStyle::Primary => (
            pal.accent,
            if pal.dark { pal.ink } else { Color32::WHITE },
            Stroke::NONE,
        ),
        _ => (pal.surface, pal.text, Stroke::new(1.0, pal.border)),
    };
    let fill = if resp.hovered() {
        if style == ButtonStyle::Primary {
            theme::mix(fill, pal.ink, 0.12)
        } else {
            pal.accent_soft
        }
    } else {
        fill
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(8),
        fill,
        stroke,
        StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        theme::font_semibold(15.0),
        fg,
    );
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(1.0),
            CornerRadius::same(9),
            Stroke::new(2.0, pal.accent),
            StrokeKind::Outside,
        );
    }
    resp
}

/// El aviso discreto de una versión nueva: "Hay una versión nueva de Tesela: v0.1.5 · Ver novedades · Descartar". Ocupa la
/// franja superior, a la izquierda del botón de ajustes. NO descarga nada: "Ver novedades" abre la página de la versión en
/// el navegador, y "Descartar" lo oculta para esa versión.
fn update_notice(ui: &mut Ui, env: &mut Env, v: tesela_update::Version) {
    let (tr, pal) = (env.tr, env.pal);
    // A la derecha queda el botón de ajustes.
    let rect = Rect::from_min_size(
        ui.max_rect().left_top(),
        vec2((ui.max_rect().width() - TOP_BAND - 16.0).max(0.0), TOP_BAND),
    );
    let mut acts: Vec<Action> = Vec::new();
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.visuals_mut().hyperlink_color = pal.accent;
            ui.label(
                RichText::new(tr.ta("update-available", args!["v" => v.tag()]))
                    .size(13.0)
                    .color(pal.text_muted),
            );
            ui.label(RichText::new("·").size(13.0).color(pal.text_muted));
            let view = ui
                .link(RichText::new(tr.t("update-view")).size(13.0))
                .on_hover_text(tr.t("update-view-hover"));
            if view.clicked() {
                acts.push(Action::OpenRelease(v));
            }
            ui.label(RichText::new("·").size(13.0).color(pal.text_muted));
            if ui
                .link(RichText::new(tr.t("update-dismiss")).size(13.0))
                .clicked()
            {
                acts.push(Action::DismissUpdate(v));
            }
        },
    );
    env.actions.extend(acts);
}

/// Pinta la pantalla de inicio.
pub fn show(ui: &mut Ui, env: &mut Env, notice: Option<&str>, dialog_open: bool) {
    let pal = env.pal;
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(pal.bg)
                .inner_margin(egui::Margin::symmetric(64, 32)),
        )
        .show(ui, |ui| {
            // El aviso de versión nueva, arriba a la izquierda (primero: ocupa la misma franja que el botón de ajustes, así
            // no mueve nada si aparece).
            if let Some(v) = env.update.notice.filter(|_| !env.demo) {
                update_notice(ui, env, v);
            }
            // Ajustes arriba a la derecha.
            let top_right = ui.max_rect().right_top() + vec2(-8.0, 0.0);
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(Rect::from_min_size(
                    top_right - vec2(TOP_BAND, 0.0),
                    vec2(TOP_BAND, TOP_BAND),
                )),
                |ui| settings_button(ui, env),
            );
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(env.tr.t("home-title"))
                        .font(theme::font_semibold(32.0))
                        .color(pal.text),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(env.tr.t("home-subtitle"))
                        .size(15.0)
                        .color(pal.text_muted),
                );
                ui.add_space(28.0);
                if let Some(n) = notice {
                    ui.label(RichText::new(n).color(pal.danger));
                    ui.add_space(8.0);
                }
                if env.drives_loading {
                    ui.horizontal(|ui| {
                        widgets::busy(ui, env.animate, 16.0, pal.accent);
                        ui.label(
                            RichText::new(env.tr.t("home-loading-drives")).color(pal.text_muted),
                        );
                    });
                } else if env.drives.is_empty() {
                    ui.label(RichText::new(env.tr.t("home-no-drives")).color(pal.text_muted));
                }
                let drives = env.drives.to_vec();
                // Las tarjetas se centran y saltan de línea si no caben.
                ui.with_layout(Layout::top_down(Align::Center), |ui| {
                    let per_row = ((ui.available_width() + 24.0) / (302.0 + 24.0))
                        .floor()
                        .max(1.0) as usize;
                    for chunk in drives.chunks(per_row) {
                        ui.horizontal(|ui| {
                            let total_w =
                                chunk.len() as f32 * 302.0 + (chunk.len() as f32 - 1.0) * 24.0;
                            ui.add_space(((ui.available_width() - total_w) / 2.0).max(0.0));
                            ui.spacing_mut().item_spacing.x = 24.0;
                            for d in chunk {
                                card(ui, env, d);
                            }
                        });
                        ui.add_space(24.0);
                    }
                });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let w = 190.0 + 24.0;
                    ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                    let r = widgets::button(
                        ui,
                        env,
                        ButtonStyle::Secondary,
                        Some(Icon::FolderOpen),
                        &env.tr.t("analyze-folder"),
                        !dialog_open,
                    );
                    if r.clicked() {
                        env.actions.push(Action::PickFolder);
                    }
                });
            });
            ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                ui.label(
                    RichText::new(env.tr.t("home-privacy"))
                        .size(12.0)
                        .color(pal.text_muted),
                );
                if env.demo {
                    ui.label(
                        RichText::new(env.tr.t("demo-banner"))
                            .size(12.0)
                            .color(pal.review),
                    );
                }
            });
        });
}
