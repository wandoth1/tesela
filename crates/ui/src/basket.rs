//! La cesta y la pantalla de Recomendaciones sobre la exploración: qué se puede añadir, la barra de la cesta, su lista
//! ("Ver") y las tarjetas Seguro / Revisar / Solo informar (PLAN.md §2.3, §2.5 y §2.6).

use crate::app::Env;
use crate::args;
use crate::describe::{self, InfoKind, Special};
use crate::explore::{AddState, Explore, Sel, View};
use crate::icons::Icon;
use crate::map::info_key;
use crate::recs::{self, RecRow, Recs};
use crate::theme::{self, Palette};
use crate::treepath::Locator;
use crate::widgets::{self, ButtonStyle};
use egui::{Align, CornerRadius, Layout, Margin, RichText, Stroke, Ui, vec2};
use tesela_cleaner::paths::PathKey;
use tesela_cleaner::{AddOutcome, Browser, CategoryId, Destination, Source, Verdict, rules};
use tesela_model::{NodeId, flags};

/// Ancho que se reserva a la derecha del titular de Recomendaciones para el botón "Limpiar lo seguro (X)" (unos 215 px) y su hueco.
const RECS_BUTTON_RESERVE: f32 = 250.0;

impl Explore {
    /// Clave de comparación de la ruta de un nodo.
    pub(crate) fn node_key(&self, id: NodeId) -> PathKey {
        PathKey::new(&self.tree.path(id))
    }

    /// Qué se puede hacer con `sel` respecto a la cesta.
    pub(crate) fn add_state(&self, sel: Sel) -> AddState {
        let id = match sel {
            Sel::Group(_) => return AddState::Group,
            Sel::Node(id) => id,
        };
        match describe::special(&self.tree, id) {
            Some(Special::SystemBlock) => return AddState::SystemBlock,
            Some(Special::InfoOnly(_)) => return AddState::InfoOnly,
            None => {}
        }
        let n = self.tree.node(id);
        if n.flags & flags::CLOUD != 0 && !n.is_dir() && n.alloc == 0 {
            return AddState::CloudOnly;
        }
        let key = self.node_key(id);
        match self.protection.target_verdict(&key) {
            Verdict::Denied => AddState::Denied,
            Verdict::Untouchable => AddState::Untouchable,
            Verdict::ContentsOnly => {
                if self.flow.basket.covers(&key.join("x")) {
                    AddState::InBasket
                } else {
                    AddState::ContentsOf
                }
            }
            Verdict::Allowed => {
                if self.flow.basket.covers(&key) {
                    AddState::InBasket
                } else {
                    AddState::Addable
                }
            }
        }
    }

    /// Añade `sel` a la cesta (un archivo o carpeta, o el contenido de una carpeta conocida).
    pub(crate) fn basket_add(&mut self, sel: Sel, env: &Env, now: f64) {
        let Sel::Node(id) = sel else { return };
        let path = self.tree.path(id);
        let prot = self.protection.clone();
        let out = match self.add_state(sel) {
            AddState::Addable => self.flow.basket.add_path(path, &prot),
            AddState::ContentsOf => self.flow.basket.add_contents_of(path, &prot),
            _ => return,
        };
        if matches!(out, AddOutcome::Added { .. }) {
            self.toast = Some((env.tr.t("basket-added"), now + 2.0));
        }
    }

    /// Quita `sel` de la cesta (solo si está como entrada propia).
    pub(crate) fn basket_remove(&mut self, sel: Sel) {
        let Sel::Node(id) = sel else { return };
        let path = self.tree.path(id);
        self.flow.basket.remove(&Source::Path(path.clone()));
        self.flow.basket.remove(&Source::ContentsOf(path));
    }

    /// ¿Está `sel` en la cesta como entrada propia (se puede quitar)?
    pub(crate) fn basket_has_own(&self, sel: Sel) -> bool {
        let Sel::Node(id) = sel else { return false };
        let path = self.tree.path(id);
        self.flow.basket.sources().any(|s| match s {
            Source::Path(p) | Source::ContentsOf(p) => PathKey::new(p) == PathKey::new(&path),
            _ => false,
        })
    }

    /// Recalcula las recomendaciones si cambió el árbol (o el overlay).
    pub(crate) fn ensure_recs(&mut self, env: &Env) {
        if self.recs.as_ref().is_some_and(|(v, _)| *v == self.version) {
            return;
        }
        let view = self.view();
        let r = recs::compute(
            &self.tree,
            &view,
            env.backend.as_ref(),
            &self.browsers_open,
            self.now_unix,
        );
        self.recs = Some((self.version, r));
    }

    /// Vuelve a mirar qué navegadores están abiertos y recalcula ("Volver a comprobar").
    pub(crate) fn recheck_browsers(&mut self, env: &Env) {
        self.browsers_open = env.backend.running_browsers();
        self.recs = None;
        self.ensure_recs(env);
    }

    /// Tamaño estimado de la cesta con el árbol (la cifra definitiva la mide el `cleaner` al confirmar).
    pub(crate) fn basket_estimate(&mut self, env: &Env) -> (usize, u64) {
        self.ensure_recs(env);
        let recs = self.recs.as_ref().map(|r| r.1.clone()).unwrap_or_default();
        let tree = self.tree.clone();
        let view = self.view();
        let mut loc = Locator::new(&tree);
        let mut bytes = 0u64;
        for s in self.flow.basket.sources() {
            bytes += match s {
                Source::Category(id) => recs.row(*id).map_or(0, |r| r.bytes),
                Source::Path(p) | Source::ContentsOf(p) => loc
                    .locate(p)
                    .filter(|n| view.is_visible(*n))
                    .map_or(0, |n| view.alloc(n)),
                Source::EmptyBin => recs.recycle.map_or(0, |r| r.1),
            };
        }
        (self.flow.basket.len(), bytes)
    }

    /// Empieza la limpieza de lo que hay en la cesta (aprobación y confirmación).
    pub(crate) fn start_clean(&mut self, ctx: &egui::Context, env: &Env) {
        let vol = self.volume_ctx();
        self.flow.start(ctx, env.backend, vol, false);
    }

    pub(crate) fn volume_ctx(&self) -> crate::clean::VolumeCtx {
        crate::clean::VolumeCtx {
            drive: self.info.req.drive.as_ref().map(|d| d.letter),
            volume: if self.info.full_volume && self.info.volume_scope {
                self.info.volume
            } else {
                None
            },
        }
    }

    /// Barra de la cesta (PLAN.md §2.3): resumen, "Ver", "Vaciar cesta" y el botón principal.
    pub(crate) fn basket_bar(&mut self, ui: &mut Ui, env: &mut Env) {
        let pal = env.pal;
        let (n, bytes) = self.basket_estimate(env);
        let busy = self.flow.busy();
        let ctx = ui.ctx().clone();
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            env.icons.show(ui, Icon::Cart, 22.0, if n > 0 { pal.accent } else { pal.text_muted });
            let text = env.tr.ta(
                "basket-summary",
                args!["n" => n as u64, "count" => env.tr.int(n as u64), "size" => env.tr.size(bytes)],
            );
            let l = ui.label(RichText::new(&text).color(if n > 0 { pal.text } else { pal.text_muted }));
            l.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text.clone()));
            if env.dry_run {
                ui.label(RichText::new(env.tr.t("dry-run-banner")).size(12.0).color(pal.review));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = env.tr.ta("basket-clean", args!["size" => env.tr.size(bytes)]);
                let r = widgets::button(ui, env, ButtonStyle::Primary, None, &label, n > 0 && !busy);
                if n == 0 {
                    r.clone().on_hover_text(env.tr.t("basket-empty-hint"));
                }
                if r.clicked() {
                    self.start_clean(&ctx, env);
                }
                let label = env.tr.t("basket-empty");
                if widgets::button(ui, env, ButtonStyle::Ghost, None, &label, n > 0 && !busy).clicked() {
                    self.flow.basket.clear();
                }
                let label = env.tr.t("basket-view");
                if widgets::button(ui, env, ButtonStyle::Secondary, None, &label, n > 0).clicked() {
                    self.basket_open = !self.basket_open;
                }
            });
        });
    }

    /// La lista de la cesta agrupada por destino, con las frases de PLAN.md §2.5.
    pub(crate) fn basket_window(&mut self, ctx: &egui::Context, env: &mut Env) {
        if !self.basket_open {
            return;
        }
        if self.flow.basket.is_empty() {
            self.basket_open = false;
            return;
        }
        let pal = env.pal;
        self.ensure_recs(env);
        let recs = self.recs.as_ref().map(|r| r.1.clone()).unwrap_or_default();
        let tree = self.tree.clone();
        let view = self.view();
        let mut loc = Locator::new(&tree);
        let mut remove: Option<Source> = None;
        let mut open = true;
        let title = env.tr.t("basket-window-title");
        egui::Window::new(&title)
            .id(egui::Id::new("tesela-basket-window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(520.0)
            .show(ctx, |ui| {
                let mut group =
                    |ui: &mut Ui,
                     env: &mut Env,
                     icon: Icon,
                     head: &str,
                     phrase: &str,
                     items: Vec<(Source, String, String, u64)>| {
                        if items.is_empty() {
                            return;
                        }
                        ui.horizontal(|ui| {
                            env.icons.show(ui, icon, 18.0, pal.text);
                            ui.label(
                                RichText::new(head)
                                    .font(theme::font_semibold(15.0))
                                    .color(pal.text),
                            );
                        });
                        ui.label(RichText::new(phrase).size(13.0).color(pal.text_muted));
                        ui.add_space(4.0);
                        for (src, name, sub, bytes) in items {
                            ui.horizontal(|ui| {
                                ui.set_min_height(28.0);
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    ui.label(RichText::new(&name).color(pal.text));
                                    if !sub.is_empty() {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&sub)
                                                    .size(11.0)
                                                    .color(pal.text_muted),
                                            )
                                            .truncate(),
                                        );
                                    }
                                });
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let label = env
                                        .tr
                                        .ta("basket-remove-item", args!["name" => name.clone()]);
                                    if widgets::icon_button(ui, env, Icon::Dismiss, &label, true)
                                        .clicked()
                                    {
                                        remove = Some(src.clone());
                                    }
                                    ui.label(RichText::new(env.tr.size(bytes)).color(pal.text));
                                });
                            });
                        }
                        ui.add_space(10.0);
                    };
                let mut cats: Vec<(Source, String, String, u64)> = Vec::new();
                let mut user: Vec<(Source, String, String, u64)> = Vec::new();
                let mut bin: Vec<(Source, String, String, u64)> = Vec::new();
                for s in self.flow.basket.sources() {
                    match s {
                        Source::Category(id) => {
                            let b = recs.row(*id).map_or(0, |r| r.bytes);
                            let row = (s.clone(), category_name(env, *id), String::new(), b);
                            // Una categoría con destino Papelera (informes de errores, volcados) va con lo que se puede recuperar.
                            if category_is_recycled(*id) {
                                user.push(row);
                            } else {
                                cats.push(row);
                            }
                        }
                        Source::Path(p) | Source::ContentsOf(p) => {
                            let b = loc
                                .locate(p)
                                .filter(|n| view.is_visible(*n))
                                .map_or(0, |n| view.alloc(n));
                            let base = p.file_name().map_or_else(
                                || p.display().to_string(),
                                |n| n.to_string_lossy().into_owned(),
                            );
                            let name = if matches!(s, Source::ContentsOf(_)) {
                                env.tr.ta("basket-contents-of", args!["name" => base])
                            } else {
                                base
                            };
                            user.push((s.clone(), name, p.display().to_string(), b));
                        }
                        Source::EmptyBin => {
                            bin.push((
                                s.clone(),
                                env.tr.t("known-recycle-bin"),
                                String::new(),
                                recs.recycle.map_or(0, |r| r.1),
                            ));
                        }
                    }
                }
                let (h1, p1) = (
                    env.tr.t("confirm-group-permanent"),
                    env.tr.t("basket-phrase-permanent"),
                );
                group(ui, env, Icon::DeleteOff, &h1, &p1, cats);
                let (h2, p2) = (
                    env.tr.t("confirm-group-recycle"),
                    env.tr.t("basket-phrase-recycle"),
                );
                group(ui, env, Icon::Delete, &h2, &p2, user);
                let (h3, p3) = (env.tr.t("confirm-group-bin"), env.tr.t("basket-phrase-bin"));
                group(ui, env, Icon::BinRecycle, &h3, &p3, bin);
            });
        if let Some(s) = remove {
            self.flow.basket.remove(&s);
        }
        self.basket_open = open;
    }
}

/// ¿Va esta categoría a la Papelera (informes de errores, volcados) en vez de borrarse y recrearse sola?
pub(crate) fn category_is_recycled(id: CategoryId) -> bool {
    rules().get(id).destination == Some(Destination::Recycle)
}

/// Nombre de una categoría en la cesta y en las recomendaciones.
pub(crate) fn category_name(env: &Env, id: CategoryId) -> String {
    env.tr.t(match id {
        CategoryId::UserTemp => "known-user-temp",
        CategoryId::WindowsTemp => "known-windows-temp",
        CategoryId::ChromeCache => "known-chrome-cache",
        CategoryId::EdgeCache => "known-edge-cache",
        CategoryId::FirefoxCache => "known-firefox-cache",
        CategoryId::RecycleBin => "known-recycle-bin",
        CategoryId::Downloads => "known-downloads",
        CategoryId::WerReports => "known-wer-reports",
        CategoryId::CrashDumps => "known-crash-dumps",
        CategoryId::Pagefile => "info-name-pagefile",
        CategoryId::Hiberfil => "info-name-hiberfil",
        CategoryId::Swapfile => "info-name-swapfile",
    })
}

// ---------------------------------------------------------------------------------------------------------------------
// Recomendaciones
// ---------------------------------------------------------------------------------------------------------------------

fn card_frame(pal: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(pal.surface)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(16, 14))
}

fn tag(
    ui: &mut Ui,
    env: &mut Env,
    icon: Icon,
    text: &str,
    fg: egui::Color32,
    bg: egui::Color32,
    hint: &str,
) {
    let pal = env.pal;
    ui.horizontal(|ui| {
        widgets::chip(ui, env, Some(icon), text, fg, bg);
        ui.label(RichText::new(hint).size(13.0).color(pal.text_muted));
    });
    ui.add_space(4.0);
}

impl Explore {
    /// La pantalla de Recomendaciones (PLAN.md §2.3 y la maqueta aprobada).
    pub(crate) fn recs_ui(&mut self, ui: &mut Ui, env: &mut Env) {
        let pal = env.pal;
        self.ensure_recs(env);
        let recs: Recs = self.recs.as_ref().map(|r| r.1.clone()).unwrap_or_default();
        // "Ya marcado": la primera vez, lo seguro que se puede limpiar ahora entra en la cesta.
        if !self.recs_preselected {
            self.recs_preselected = true;
            for id in recs.preselected() {
                self.flow.basket.set_category(id, true);
            }
        }
        let ctx = ui.ctx().clone();
        let safe = recs.safe_total();
        let review = recs.review_total();
        let busy = self.flow.busy();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        // R-1: el titular (largo: "Hasta X en candidatos de bajo riesgo...") se limita al ancho que deja el botón
                        // principal y pasa a varias líneas; con la ventana mínima (960 px) ocupaba su sitio y lo pisaba.
                        ui.set_max_width((ui.available_width() - RECS_BUTTON_RESERVE).max(240.0));
                        let title = if safe > 0 {
                            env.tr
                                .ta("recs-title-safe", args!["size" => env.tr.size(safe)])
                        } else {
                            env.tr.t("recs-title-none")
                        };
                        let l = ui.label(
                            RichText::new(&title)
                                .font(theme::font_semibold(28.0))
                                .color(pal.text),
                        );
                        l.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::Label, true, title.clone())
                        });
                        if review > 0 {
                            ui.label(
                                RichText::new(env.tr.ta(
                                    "recs-subtitle-review",
                                    args!["size" => env.tr.size(review)],
                                ))
                                .color(pal.text_muted),
                            );
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let label = env
                            .tr
                            .ta("recs-clean-safe", args!["size" => env.tr.size(safe)]);
                        let r = widgets::button(
                            ui,
                            env,
                            ButtonStyle::Primary,
                            Some(Icon::ShieldCheckmark),
                            &label,
                            safe > 0 && !busy,
                        );
                        if r.clicked() {
                            for id in recs.preselected() {
                                self.flow.basket.set_category(id, true);
                            }
                            self.start_clean(&ctx, env);
                        }
                    });
                });
                ui.add_space(14.0);
                if recs.rows.is_empty()
                    && recs.unavailable.is_empty()
                    && recs.info.is_empty()
                    && recs.downloads.is_none()
                    && recs.recycle.is_none()
                {
                    ui.label(RichText::new(env.tr.t("recs-none")).color(pal.text_muted));
                    return;
                }
                let wide = ui.available_width() >= 760.0;
                let left = |ui: &mut Ui, env: &mut Env, this: &mut Explore| {
                    this.recs_safe_column(ui, env, &recs)
                };
                let right = |ui: &mut Ui, env: &mut Env, this: &mut Explore| {
                    this.recs_review_column(ui, env, &recs)
                };
                if wide {
                    let w = (ui.available_width() - 20.0) / 2.0;
                    ui.horizontal_top(|ui| {
                        ui.allocate_ui_with_layout(
                            vec2(w, 0.0),
                            Layout::top_down(Align::Min),
                            |ui| left(ui, env, self),
                        );
                        ui.add_space(20.0);
                        ui.allocate_ui_with_layout(
                            vec2(w, 0.0),
                            Layout::top_down(Align::Min),
                            |ui| right(ui, env, self),
                        );
                    });
                } else {
                    left(ui, env, self);
                    ui.add_space(14.0);
                    right(ui, env, self);
                }
            });
    }

    fn recs_safe_column(&mut self, ui: &mut Ui, env: &mut Env, recs: &Recs) {
        let pal = env.pal;
        let hint = env.tr.t("recs-safe-hint");
        tag(
            ui,
            env,
            Icon::ShieldCheckmark,
            &env.tr.t("risk-safe-tag"),
            pal.safe,
            theme::mix(pal.safe, pal.bg, 0.85),
            &hint,
        );
        for id in [CategoryId::UserTemp, CategoryId::WindowsTemp] {
            if let Some(row) = recs.row(id) {
                self.safe_card(ui, env, &[row], id);
            }
        }
        let browsers: Vec<&RecRow> = [
            CategoryId::ChromeCache,
            CategoryId::EdgeCache,
            CategoryId::FirefoxCache,
        ]
        .into_iter()
        .filter_map(|c| recs.row(c))
        .collect();
        if !browsers.is_empty() {
            self.safe_card(ui, env, &browsers, CategoryId::ChromeCache);
        }
        // H4: lo que existe pero Tesela no ofrece, con su motivo, en vez de desaparecer en silencio.
        if !recs.unavailable.is_empty() {
            card_frame(&pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new(env.tr.t("recs-unavailable-title"))
                        .font(theme::font_semibold(15.0))
                        .color(pal.text),
                );
                for u in &recs.unavailable {
                    let name = if u.id.is_browser_cache() {
                        browser_label(env, u.id)
                    } else {
                        category_name(env, u.id)
                    };
                    let why = env.tr.t(match u.why {
                        recs::Unavailable::Link => "recs-unavailable-link",
                        recs::Unavailable::Changed => "recs-unavailable-changed",
                        recs::Unavailable::Outside => "recs-unavailable-outside",
                    });
                    let line = env.tr.ta(
                        "recs-unavailable-line",
                        args!["name" => name, "detail" => why],
                    );
                    ui.add(
                        egui::Label::new(RichText::new(line).size(13.0).color(pal.text_muted))
                            .wrap(),
                    );
                }
            });
            ui.add_space(10.0);
        }
    }

    /// Una tarjeta "Seguro". `rows` tiene una fila (temporales) o las de los navegadores (agrupadas en una tarjeta).
    fn safe_card(&mut self, ui: &mut Ui, env: &mut Env, rows: &[&RecRow], lead: CategoryId) {
        let pal = env.pal;
        let grouped = rows.len() > 1 || rows.first().is_some_and(|r| r.id.is_browser_cache());
        let name = if grouped {
            env.tr.t("rule-browsers-name")
        } else {
            category_name(env, lead)
        };
        let why = env.tr.t(if grouped {
            "rule-browsers-why"
        } else if lead == CategoryId::UserTemp {
            "rule-user-temp-why"
        } else {
            "rule-windows-temp-why"
        });
        let usable: Vec<CategoryId> = rows.iter().filter(|r| r.usable()).map(|r| r.id).collect();
        let total: u64 = rows.iter().filter(|r| r.usable()).map(|r| r.bytes).sum();
        let mut checked = !usable.is_empty()
            && usable
                .iter()
                .all(|id| self.flow.basket.contains_category(*id));
        let before = checked;
        let blocked: Vec<&&RecRow> = rows.iter().filter(|r| r.blocked_by.is_some()).collect();
        let mut recheck = false;
        card_frame(&pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                let cb = ui.add_enabled(!usable.is_empty(), egui::Checkbox::without_text(&mut checked));
                let label = env.tr.ta("recs-include", args!["name" => name.clone()]);
                cb.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, !usable.is_empty(), checked, label.clone()));
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&name).font(theme::font_semibold(15.0)).color(pal.text));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new(env.tr.size(total)).font(theme::font_semibold(15.0)).color(pal.text));
                        });
                    });
                    ui.add(egui::Label::new(RichText::new(&why).size(13.0).color(pal.text_muted)).wrap());
                    if grouped {
                        ui.add_space(4.0);
                        for r in rows.iter().filter(|r| r.usable()) {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(browser_label(env, r.id)).size(13.0).color(pal.text));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(RichText::new(env.tr.size(r.bytes)).size(13.0).color(pal.text));
                                });
                            });
                        }
                        for r in &blocked {
                            if let Some(b) = r.blocked_by {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        RichText::new(env.tr.ta(
                                            "recs-blocked",
                                            args!["name" => b.name(), "size" => env.tr.size(r.bytes)],
                                        ))
                                        .size(13.0)
                                        .color(pal.review),
                                    );
                                    if ui.link(env.tr.t("recs-recheck")).clicked() {
                                        recheck = true;
                                    }
                                });
                            }
                        }
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        env.icons.show(ui, Icon::DeleteOff, 13.0, pal.text);
                        ui.label(RichText::new(env.tr.t("dest-permanent")).size(12.0).color(pal.text));
                    });
                });
            });
        });
        ui.add_space(10.0);
        if checked != before {
            for id in &usable {
                self.flow.basket.set_category(*id, checked);
            }
        }
        if recheck {
            self.recheck_browsers(env);
        }
    }

    fn recs_review_column(&mut self, ui: &mut Ui, env: &mut Env, recs: &Recs) {
        let pal = env.pal;
        let hint = env.tr.t("recs-review-hint");
        tag(
            ui,
            env,
            Icon::Eye,
            &env.tr.t("risk-review-tag"),
            pal.review,
            theme::mix(pal.review, pal.bg, 0.85),
            &hint,
        );
        let ctx = ui.ctx().clone();
        let mut go_to: Option<NodeId> = None;
        let mut empty = false;
        card_frame(&pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some((node, bytes)) = recs.downloads {
                review_row(
                    ui,
                    env,
                    &env.tr.t("known-downloads"),
                    &env.tr.t("rule-downloads-why"),
                    bytes,
                    &env.tr.t("recs-view"),
                    true,
                    || go_to = Some(node),
                );
            }
            if let Some((_, bytes)) = recs.recycle {
                if recs.downloads.is_some() {
                    ui.separator();
                }
                let busy = self.flow.busy();
                review_row(
                    ui,
                    env,
                    &env.tr.t("known-recycle-bin"),
                    &env.tr.t("rule-recycle-bin-why"),
                    bytes,
                    &env.tr.t("recs-empty-bin"),
                    !busy && bytes > 0,
                    || empty = true,
                );
            }
            // Informes de errores y volcados de bloqueo: Revisar, con casilla (nunca marcada de salida) y a la Papelera.
            let mut any = recs.downloads.is_some() || recs.recycle.is_some();
            for row in recs.diagnostics() {
                if any {
                    ui.separator();
                }
                any = true;
                self.review_category_row(ui, env, row.id, row.bytes);
            }
            if !any {
                ui.label(RichText::new(env.tr.t("recs-review-none")).color(pal.text_muted));
            }
        });
        if let Some(n) = go_to {
            self.view = View::Map;
            self.go_to(n);
        }
        if empty {
            self.flow.basket.set_empty_bin(true);
            self.start_clean(&ctx, env);
        }
        ui.add_space(14.0);
        let hint = env.tr.t("recs-info-hint");
        tag(
            ui,
            env,
            Icon::Warning,
            &env.tr.t("risk-info-only"),
            pal.danger,
            theme::mix(pal.danger, pal.bg, 0.88),
            &hint,
        );
        card_frame(&pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if recs.info.is_empty() {
                ui.label(RichText::new(env.tr.t("recs-info-none")).color(pal.text_muted));
            }
            for (i, it) in recs.info.iter().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                let name = env.tr.t(match it.kind {
                    InfoKind::Pagefile => "info-name-pagefile",
                    InfoKind::Hiberfil => "info-name-hiberfil",
                    InfoKind::Swapfile => "info-name-swapfile",
                });
                review_row(
                    ui,
                    env,
                    &name,
                    &env.tr.t(info_key(it.kind)),
                    it.bytes,
                    "",
                    false,
                    || {},
                );
            }
        });
    }
}

impl Explore {
    /// Una categoría de "Revisar" con casilla (informes de errores y volcados de bloqueo): nombre, qué es y qué se pierde,
    /// tamaño estimado y destino (la Papelera). Marcarla la mete en la cesta; nunca viene marcada.
    fn review_category_row(&mut self, ui: &mut Ui, env: &mut Env, id: CategoryId, bytes: u64) {
        let pal = env.pal;
        let name = category_name(env, id);
        let why = env.tr.t(match id {
            CategoryId::WerReports => "rule-wer-reports-why",
            _ => "rule-crash-dumps-why",
        });
        let mut checked = self.flow.basket.contains_category(id);
        let before = checked;
        ui.horizontal_top(|ui| {
            let cb = ui.add_enabled(bytes > 0, egui::Checkbox::without_text(&mut checked));
            let label = env.tr.ta("recs-include", args!["name" => name.clone()]);
            cb.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Checkbox,
                    bytes > 0,
                    checked,
                    label.clone(),
                )
            });
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(&name)
                            .font(theme::font_semibold(15.0))
                            .color(pal.text),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(env.tr.size(bytes))
                                .font(theme::font_semibold(15.0))
                                .color(pal.text),
                        );
                    });
                });
                ui.add(
                    egui::Label::new(RichText::new(&why).size(13.0).color(pal.text_muted)).wrap(),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    env.icons.show(ui, Icon::Delete, 13.0, pal.text);
                    ui.label(
                        RichText::new(env.tr.t("dest-recycle-diag"))
                            .size(12.0)
                            .color(pal.text),
                    );
                });
            });
        });
        if checked != before {
            self.flow.basket.set_category(id, checked);
        }
    }
}

fn browser_label(env: &Env, id: CategoryId) -> String {
    Browser::from_category(id).map_or_else(String::new, |b| {
        env.tr.ta("recs-browser-cache", args!["name" => b.name()])
    })
}

/// Una fila de las tarjetas "Revisar" y "Solo informar": nombre, explicación, tamaño y, si lo hay, un botón.
#[allow(clippy::too_many_arguments)]
fn review_row(
    ui: &mut Ui,
    env: &mut Env,
    name: &str,
    why: &str,
    bytes: u64,
    button: &str,
    enabled: bool,
    mut on_click: impl FnMut(),
) {
    let pal = env.pal;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_max_width((ui.available_width() - 190.0).max(120.0));
            ui.label(
                RichText::new(name)
                    .font(theme::font_semibold(15.0))
                    .color(pal.text),
            );
            ui.add(egui::Label::new(RichText::new(why).size(13.0).color(pal.text_muted)).wrap());
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if !button.is_empty()
                && widgets::button(ui, env, ButtonStyle::Secondary, None, button, enabled).clicked()
            {
                on_click();
            }
            ui.label(
                RichText::new(env.tr.size(bytes))
                    .font(theme::font_semibold(15.0))
                    .color(pal.text),
            );
        });
    });
}
