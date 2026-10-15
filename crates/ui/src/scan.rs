//! Pantalla de escaneo (PLAN.md §2.4): progreso real, filas de las rutas conocidas con su tamaño a medida que la cola
//! prioritaria las completa, barras vivas por carpeta de primer nivel y "Parar y ver".
//!
//! La clasificación de riesgo y "Limpiar lo seguro" son del incremento 3: las filas son informativas y el botón está
//! deshabilitado con la pista "Disponible en la próxima versión".
//!
//! El modo demostración usa un escaneo SIMULADO sobre el árbol sintético (misma pantalla, sin tocar el disco).

use crate::app::{Action, Env, ScanRequest};
use crate::args;
use crate::demo::{self, DEMO_KNOWN_ROWS};
use crate::explore::{Explore, ScanInfo};
use crate::icons::Icon;
use crate::theme::{self, Palette};
use crate::widgets::{self, ButtonStyle};
use egui::{CornerRadius, Margin, RichText, Ui, vec2};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tesela_model::{FileType, Tree};
use tesela_scanner::{KnownKind, Progress, ScanHandle, ScanOptions};

/// Cómo acabó (o sigue) un escaneo.
pub enum ScanOutcome {
    Done(Box<Explore>),
    Failed(String),
    Cancelled,
}

/// Duración del escaneo simulado de la demostración.
const DEMO_SECS: f32 = 6.0;

struct Real {
    handle: Option<ScanHandle>,
    progress: Arc<Progress>,
    /// Clase de cada ruta prioritaria (mismo orden que `progress.priority_targets()`).
    kinds: Vec<Option<KnownKind>>,
}

struct Demo {
    tree: Arc<Tree>,
    /// (nombre, bytes finales) de las carpetas de primer nivel, de mayor a menor.
    top: Vec<(String, u64)>,
    files: u64,
    /// Fracción fija (capturas y pruebas); `None` = avanza con el reloj.
    frozen: Option<f32>,
    stop: bool,
}

enum Kind {
    Real(Real),
    Demo(Demo),
}

/// Escaneo en marcha.
pub struct ScanScreen {
    req: ScanRequest,
    started: Instant,
    /// Pedido el descarte (Cancelar): al terminar se vuelve al inicio sin mostrar el mapa.
    discard: bool,
    animate: bool,
    kind: Kind,
}

/// Nombre canónico en minúsculas para comparar rutas (formas largas, sin `\\?\`).
fn canon_key(p: &Path) -> Option<String> {
    let c = std::fs::canonicalize(p).ok()?;
    Some(dunce::simplified(&c).to_string_lossy().to_lowercase())
}

impl ScanScreen {
    /// Escaneo real de `req.root`.
    pub fn real(
        _ctx: &egui::Context,
        req: ScanRequest,
        animate: bool,
    ) -> Result<ScanScreen, String> {
        let handle =
            tesela_scanner::start(ScanOptions::new(&req.root)).map_err(|e| e.to_string())?;
        // Las rutas prioritarias salen canónicas; se emparejan con las candidatas (también canónicas) para nombrarlas.
        let drive_root = req.drive.as_ref().map(|d| d.root());
        let labeled =
            tesela_scanner::known_paths_labeled(drive_root.as_deref().or(Some(req.root.as_path())));
        let mut by_path: HashMap<String, KnownKind> = HashMap::new();
        for (kind, p) in labeled {
            if let Some(k) = canon_key(&p) {
                by_path.entry(k).or_insert(kind);
            }
        }
        let progress = handle.progress().clone();
        let kinds = progress
            .priority_targets()
            .iter()
            .map(|t| canon_key(&t.path).and_then(|k| by_path.get(&k).copied()))
            .collect();
        Ok(ScanScreen {
            req,
            started: Instant::now(),
            discard: false,
            animate,
            kind: Kind::Real(Real {
                handle: Some(handle),
                progress,
                kinds,
            }),
        })
    }

    /// Escaneo simulado sobre el árbol sintético. `frozen` fija la fracción (capturas y pruebas).
    pub fn demo(req: ScanRequest, frozen: Option<f32>, _stopped: bool) -> ScanScreen {
        let tree = Arc::new(demo::tree(demo::Variant::Complete));
        let mut top: Vec<(String, u64)> = tree
            .children(0)
            .filter(|&c| tree.node(c).flags & tesela_model::flags::SYSTEM_INFO == 0)
            .map(|c| (tree.name_lossy(c).into_owned(), tree.node(c).alloc))
            .collect();
        top.sort_by_key(|t| std::cmp::Reverse(t.1));
        let files = tree.stats().files;
        ScanScreen {
            req,
            started: Instant::now(),
            discard: false,
            animate: true,
            kind: Kind::Demo(Demo {
                tree,
                top,
                files,
                frozen,
                stop: false,
            }),
        }
    }

    /// Nombre de lo que se escanea.
    pub fn label(&self) -> &str {
        &self.req.label
    }

    /// Pide cancelar (los hilos terminan solos y el resultado se descarta).
    pub fn cancel(&mut self) {
        self.discard = true;
        if let Kind::Real(r) = &self.kind
            && let Some(h) = &r.handle
        {
            h.cancel();
        }
    }

    fn demo_fraction(&self, d: &Demo) -> f32 {
        d.frozen
            .unwrap_or_else(|| (self.started.elapsed().as_secs_f32() / DEMO_SECS).min(1.0))
            .clamp(0.0, 1.0)
    }

    /// Avanza el escaneo: si terminó, devuelve el resultado.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<ScanOutcome> {
        let started = self.started;
        match &mut self.kind {
            Kind::Real(r) => {
                if r.handle.as_ref().is_some_and(ScanHandle::is_finished) {
                    let handle = r.handle.take()?;
                    return Some(match handle.wait() {
                        _ if self.discard => ScanOutcome::Cancelled,
                        Ok(res) => ScanOutcome::Done(Box::new(Explore::from_scan(
                            self.req.clone(),
                            res,
                            self.animate,
                            started.elapsed(),
                        ))),
                        Err(e) => ScanOutcome::Failed(e.to_string()),
                    });
                }
                ctx.request_repaint_after(Duration::from_millis(50));
                None
            }
            Kind::Demo(d) => {
                let done = d.frozen.is_none()
                    && (d.stop || self.started.elapsed().as_secs_f32() >= DEMO_SECS);
                if done && !self.discard {
                    let variant = if d.stop {
                        demo::Variant::Stopped
                    } else {
                        demo::Variant::Complete
                    };
                    return Some(ScanOutcome::Done(Box::new(Explore::demo(
                        self.req.clone(),
                        variant,
                        self.animate,
                    ))));
                }
                if done {
                    return Some(ScanOutcome::Cancelled);
                }
                if d.frozen.is_none() {
                    ctx.request_repaint_after(Duration::from_millis(33));
                }
                None
            }
        }
    }

    fn stop(&mut self) {
        match &mut self.kind {
            Kind::Real(r) => {
                if let Some(h) = &r.handle {
                    h.cancel();
                }
            }
            Kind::Demo(d) => d.stop = true,
        }
    }
}

/// Una fila de "sitios conocidos".
struct Row {
    name: String,
    bytes: u64,
    done: bool,
}

fn kind_key(k: Option<KnownKind>) -> &'static str {
    match k {
        Some(KnownKind::UserTemp) => "known-user-temp",
        Some(KnownKind::WindowsTemp) => "known-windows-temp",
        Some(KnownKind::ChromeCache) => "known-chrome-cache",
        Some(KnownKind::EdgeCache) => "known-edge-cache",
        Some(KnownKind::FirefoxCache) => "known-firefox-cache",
        Some(KnownKind::Downloads) => "known-downloads",
        Some(KnownKind::RecycleBin) => "known-recycle-bin",
        None => "known-other",
    }
}

/// Valores que la pantalla enseña (de un escaneo real o simulado).
struct View {
    files: u64,
    bytes: u64,
    total: Option<u64>,
    errors: u64,
    rows: Vec<Row>,
    /// (nombre, bytes, terminado) de las carpetas de primer nivel, de mayor a menor.
    bars: Vec<(String, u64, bool)>,
    finished: bool,
    fraction: Option<f32>,
}

fn view_of(s: &ScanScreen, env: &Env) -> View {
    let total = s
        .req
        .drive
        .as_ref()
        .map(|d| d.total.saturating_sub(d.free))
        .filter(|t| *t > 0);
    match &s.kind {
        Kind::Real(r) => {
            let p = &r.progress;
            let load = std::sync::atomic::Ordering::Relaxed;
            let bytes = p.alloc.load(load);
            let rows = p
                .priority_targets()
                .iter()
                .zip(&r.kinds)
                .map(|(t, k)| Row {
                    name: env.tr.t(kind_key(*k)),
                    bytes: t.alloc.load(load),
                    done: t.is_done(),
                })
                .collect();
            let mut bars: Vec<(String, u64, bool)> = p
                .top_levels()
                .iter()
                .map(|t| {
                    let name = if t.name.is_empty() {
                        env.tr.t("scan-root-files")
                    } else {
                        t.name.to_string_lossy().into_owned()
                    };
                    (name, t.alloc.load(load), false)
                })
                .filter(|b| b.1 > 0)
                .collect();
            bars.sort_by_key(|b| std::cmp::Reverse(b.1));
            View {
                files: p.files.load(load),
                bytes,
                total,
                errors: p.errors.load(load),
                rows,
                bars,
                finished: p.finished.load(load),
                fraction: total.map(|t| (bytes as f32 / t as f32).clamp(0.0, 0.99)),
            }
        }
        Kind::Demo(d) => {
            let f = s.demo_fraction(d);
            let rows = DEMO_KNOWN_ROWS
                .iter()
                .map(|(key, bytes, end)| Row {
                    name: env.tr.t(key),
                    bytes: (*bytes as f32 * (f / end).min(1.0)) as u64,
                    done: f >= *end,
                })
                .collect();
            // Cada carpeta avanza a su ritmo: las más grandes tardan más en completarse.
            let bars = d
                .top
                .iter()
                .enumerate()
                .map(|(i, (n, b))| {
                    let speed = 1.0 + 0.35 * i as f32;
                    let k = (f * speed).min(1.0);
                    (n.clone(), (*b as f32 * k) as u64, k >= 1.0)
                })
                .collect();
            let used = d.tree.node(0).alloc;
            View {
                files: (d.files as f32 * f) as u64,
                bytes: (used as f32 * f) as u64,
                total: total.or(Some(used)),
                errors: 0,
                rows,
                bars,
                finished: f >= 1.0,
                fraction: Some(f),
            }
        }
    }
}

fn card_frame(pal: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(pal.surface)
        .stroke(egui::Stroke::new(1.0, pal.border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(28, 24))
}

/// Pinta la pantalla de escaneo.
pub fn show(ui: &mut Ui, env: &mut Env, s: &mut ScanScreen) {
    let pal = env.pal;
    let v = view_of(s, env);
    let elapsed = s.started.elapsed().as_secs();
    let mut stop = false;
    let mut cancel = false;
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(pal.bg)
                .inner_margin(Margin::symmetric(40, 28)),
        )
        .show(ui, |ui| {
            // Título, cifras y botones.
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(env.tr.t("scan-title"))
                        .font(theme::font_semibold(26.0))
                        .color(pal.text),
                );
                ui.add_space(8.0);
                let counts = match v.total {
                    Some(t) => env.tr.ta(
                        "scan-counts-of",
                        args![
                            "files" => env.tr.int(v.files),
                            "size" => env.tr.size(v.bytes),
                            "total" => env.tr.size(t),
                            "time" => format!("{}:{:02}", elapsed / 60, elapsed % 60)
                        ],
                    ),
                    None => env.tr.ta(
                        "scan-counts",
                        args![
                            "files" => env.tr.int(v.files),
                            "size" => env.tr.size(v.bytes),
                            "time" => format!("{}:{:02}", elapsed / 60, elapsed % 60)
                        ],
                    ),
                };
                ui.label(RichText::new(counts).color(pal.text_muted));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::button(ui, env, ButtonStyle::Ghost, None, &env.tr.t("cancel"), true)
                        .clicked()
                    {
                        cancel = true;
                    }
                    if widgets::button(
                        ui,
                        env,
                        ButtonStyle::Secondary,
                        Some(Icon::Stop),
                        &env.tr.t("scan-stop"),
                        !v.finished,
                    )
                    .clicked()
                    {
                        stop = true;
                    }
                });
            });
            ui.add_space(6.0);
            // Barra de progreso general.
            let frac = v.fraction;
            let (bar_rect, bar_resp) =
                ui.allocate_exact_size(vec2(ui.available_width(), 8.0), egui::Sense::hover());
            bar_resp.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::ProgressIndicator,
                    true,
                    env.tr.ta(
                        "scan-progress-label",
                        args!["pct" => (frac.unwrap_or(0.0) * 100.0) as u64],
                    ),
                )
            });
            ui.painter()
                .rect_filled(bar_rect, CornerRadius::same(4), pal.track);
            match frac {
                Some(f) => {
                    let r =
                        egui::Rect::from_min_size(bar_rect.min, vec2(bar_rect.width() * f, 8.0));
                    ui.painter()
                        .rect_filled(r, CornerRadius::same(4), pal.accent);
                }
                None => {
                    // Sin total conocido (una carpeta): un tramo que va y viene.
                    let t = ui.input(|i| i.time) as f32;
                    let w = bar_rect.width() * 0.25;
                    let x =
                        bar_rect.left() + (bar_rect.width() - w) * (0.5 + 0.5 * (t * 2.0).sin());
                    let r = egui::Rect::from_min_size(egui::pos2(x, bar_rect.top()), vec2(w, 8.0));
                    ui.painter()
                        .rect_filled(r, CornerRadius::same(4), pal.accent);
                    ui.ctx().request_repaint_after(Duration::from_millis(33));
                }
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new(s.req.root.display().to_string())
                    .family(egui::FontFamily::Monospace)
                    .size(12.0)
                    .color(pal.text_muted),
            );
            ui.add_space(14.0);

            // Dos tarjetas: sitios conocidos (izquierda) y el mapa que se llena (derecha).
            let avail = ui.available_rect_before_wrap();
            let right_w = 460.0f32.min(avail.width() * 0.45);
            let left_rect = egui::Rect::from_min_max(
                avail.min,
                egui::pos2(avail.right() - right_w - 24.0, avail.bottom()),
            );
            let right_rect = egui::Rect::from_min_max(
                egui::pos2(avail.right() - right_w, avail.top()),
                avail.max,
            );

            ui.scope_builder(egui::UiBuilder::new().max_rect(left_rect), |ui| {
                card_frame(&pal).show(ui, |ui| {
                    ui.set_min_size(vec2(left_rect.width() - 58.0, left_rect.height() - 50.0));
                    known_places(ui, env, &v);
                });
            });
            ui.scope_builder(egui::UiBuilder::new().max_rect(right_rect), |ui| {
                card_frame(&pal).show(ui, |ui| {
                    ui.set_min_size(vec2(right_rect.width() - 58.0, right_rect.height() - 50.0));
                    live_map(ui, env, &v);
                });
            });
        });
    if stop {
        s.stop();
    }
    if cancel {
        env.actions.push(Action::GoHome);
    }
}

fn known_places(ui: &mut Ui, env: &mut Env, v: &View) {
    let pal = env.pal;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(
                RichText::new(env.tr.t("scan-known-title"))
                    .size(13.0)
                    .color(pal.text_muted),
            );
            let sum: u64 = v.rows.iter().map(|r| r.bytes).sum();
            ui.label(
                RichText::new(
                    env.tr
                        .ta("scan-known-sum", args!["size" => env.tr.size(sum)]),
                )
                .font(theme::font_semibold(30.0))
                .color(pal.text),
            );
            ui.label(
                RichText::new(env.tr.t("scan-known-note"))
                    .size(13.0)
                    .color(pal.text_muted),
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
            // Inactivo: la limpieza es del incremento 3.
            let _ = widgets::button(
                ui,
                env,
                ButtonStyle::Primary,
                Some(Icon::ShieldCheckmark),
                &env.tr.t("scan-clean-safe"),
                false,
            );
        });
    });
    ui.add_space(14.0);
    let done = v.rows.iter().filter(|r| r.done).count();
    if v.rows.is_empty() {
        ui.label(RichText::new(env.tr.t("scan-no-known")).color(pal.text_muted));
        return;
    }
    ui.horizontal(|ui| {
        if done < v.rows.len() {
            ui.add(egui::Spinner::new().size(16.0));
            ui.label(
                RichText::new(env.tr.ta(
                    "scan-checking",
                    args!["done" => done, "total" => v.rows.len()],
                ))
                .size(13.0)
                .color(pal.text_muted),
            );
        } else {
            ui.label(
                RichText::new(env.tr.t("scan-checked"))
                    .size(13.0)
                    .color(pal.text_muted),
            );
        }
    });
    ui.add_space(6.0);
    let top = ui.cursor().top();
    ui.painter().hline(
        ui.max_rect().x_range(),
        top,
        egui::Stroke::new(1.0, pal.border),
    );
    for r in &v.rows {
        ui.horizontal(|ui| {
            ui.set_min_height(34.0);
            if r.done {
                env.icons.show(ui, Icon::Checkmark, 18.0, pal.safe);
            } else {
                ui.add(egui::Spinner::new().size(16.0));
            }
            ui.label(RichText::new(&r.name).size(15.0).color(pal.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (txt, col) = if r.done || r.bytes > 0 {
                    (env.tr.size(r.bytes), pal.text)
                } else {
                    (env.tr.t("scan-searching"), pal.text_muted)
                };
                ui.label(
                    RichText::new(txt)
                        .font(theme::font_semibold(15.0))
                        .color(col),
                );
            });
        });
        let y = ui.cursor().top();
        ui.painter().hline(
            ui.max_rect().x_range(),
            y,
            egui::Stroke::new(1.0, pal.border),
        );
    }
    if v.errors > 0 {
        ui.add_space(10.0);
        ui.label(
            RichText::new(env.tr.ta("scan-errors", args!["n" => env.tr.int(v.errors)]))
                .size(13.0)
                .color(pal.text_muted),
        );
    }
}

fn live_map(ui: &mut Ui, env: &mut Env, v: &View) {
    let pal = env.pal;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(env.tr.t("scan-map-title"))
                .font(theme::font_semibold(16.0))
                .color(pal.text),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(env.tr.t("scan-counting"))
                    .size(12.0)
                    .color(pal.text_muted),
            );
        });
    });
    ui.add_space(8.0);
    let max = v.bars.first().map_or(1, |b| b.1).max(1);
    let bar_color = pal.type_color(FileType::Program);
    for (name, bytes, done) in v.bars.iter().take(8) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(name).color(pal.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let s = if *done || v.finished {
                    env.tr.size(*bytes)
                } else {
                    env.tr.size_min(*bytes)
                };
                ui.label(RichText::new(s).color(pal.text_muted));
            });
        });
        widgets::progress_bar(
            ui,
            &pal,
            *bytes as f32 / max as f32,
            bar_color,
            14.0,
            !(*done || v.finished),
            None,
        );
        ui.add_space(4.0);
    }
    ui.add_space(8.0);
    ui.label(
        RichText::new(env.tr.t("scan-fast-mode"))
            .size(13.0)
            .color(pal.text_muted),
    )
    .on_hover_text(env.tr.t("next-version"));
}

/// Información del escaneo que se conserva en la pantalla de exploración.
pub(crate) fn info_from_summary(
    req: &ScanRequest,
    summary: &tesela_scanner::ScanSummary,
    files: u64,
    elapsed: Duration,
) -> ScanInfo {
    ScanInfo {
        req: req.clone(),
        display_root: summary.root.display().to_string(),
        full_volume: summary.full_volume,
        volume: summary.volume.map(|v| (v.total, v.free)),
        cancelled: summary.cancelled,
        approximate: summary.api == tesela_scanner::ApiUsed::FindFallback,
        dir_errors: summary.errors(),
        files,
        elapsed,
        finished_at: Instant::now(),
    }
}
