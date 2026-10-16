//! El flujo de limpieza de la interfaz: cesta -> aprobación -> confirmación -> ejecución -> resumen (PLAN.md §2.5).
//!
//! Es un componente independiente de la exploración: lo usan la pantalla de escaneo ("Limpiar lo seguro") y la ventana
//! principal. Todo lo que toca el disco va por el [`Backend`] (el `cleaner` real, o el simulado de `--demo`), en hilos
//! propios; la interfaz solo lee atómicos y canales. NADA se borra sin pasar por la confirmación, que enseña el total y el
//! destino de cada cosa.

use crate::app::Env;
use crate::args;
use crate::icons::Icon;
use crate::theme::{self, Palette};
use crate::widgets::{self, ButtonStyle};
use crossbeam_channel::{Receiver, TryRecvError};
use egui::{Align, CornerRadius, Layout, Margin, RichText, Stroke, Ui, vec2};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tesela_cleaner::log::{HistoryEntry, history_append};
use tesela_cleaner::{
    ApproveOptions, Backend, Basket, Browser, CategoryId, ExecError, ExecOptions, Fit, LogWriter,
    Phase, Plan, PlanItem, Progress, RejectReason, Removed, Report, StopReason, UnfitReason,
    Verdict,
};

/// Contexto del volumen que se está limpiando (para el historial y las barras antes/ahora del resumen).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct VolumeCtx {
    /// Letra de la unidad (si se analizó una unidad entera).
    pub drive: Option<char>,
    /// (capacidad, libre) del volumen al terminar el análisis, con ámbito de volumen.
    pub volume: Option<(u64, u64)>,
}

/// Eventos del flujo que la pantalla dueña tiene que atender.
#[derive(Debug)]
pub(crate) enum FlowEvent {
    /// Lo que desapareció de verdad: la exploración actualiza el árbol con esto (overlay), sin reescanear.
    Removed(Vec<Removed>),
}

struct Confirm {
    plan: Arc<Plan>,
    /// La casilla del grupo ámbar ("se eliminará definitivamente porque no cabe").
    allow_unfit: bool,
    /// "Ver la lista completa".
    show_all: bool,
    /// Se vuelve a confirmar porque algo cambió.
    reconfirm: bool,
    focused: bool,
}

struct Running {
    progress: Arc<Progress>,
    rx: Receiver<Result<Report, ExecError>>,
    dry: bool,
    allow_unfit: bool,
    cancelled: bool,
    started: Instant,
    /// Espacio usado del volumen al empezar (para el historial).
    used_before: Option<u64>,
    /// El usuario intentó cerrar la ventana mientras se ejecutaba: se le dice que espere o cancele.
    close_blocked: bool,
}

struct Summary {
    report: Report,
    vol: VolumeCtx,
    /// Navegadores abiertos con una caché sin limpiar y cuánto se liberaría al cerrarlos.
    blocked: Vec<(Browser, u64)>,
}

enum Stage {
    Idle,
    Approving {
        rx: Receiver<Plan>,
        cancel: Arc<AtomicBool>,
        reconfirm: bool,
    },
    Confirm(Box<Confirm>),
    Running(Box<Running>),
    Summary(Box<Summary>),
}

/// Aviso breve (toast) pendiente de pintar.
pub(crate) enum Note {
    NothingToClean,
    /// No se pudo crear el registro local: sin registro no se limpia (M4). No se ha tocado nada.
    NoLog,
    /// Lo que cabe en la Papelera cambió desde la confirmación: hay que revisar y confirmar otra vez.
    Reconfirm,
    /// La aplicación se ejecuta como administrador: la limpieza real no se hace (E1). No se ha tocado nada.
    Elevated,
}

impl Drop for CleanFlow {
    /// Si la pantalla se cierra mientras se aprueba la cesta o se ejecuta, se cancela (entre entradas: lo que ya empezó
    /// termina bien y nada más se toca).
    fn drop(&mut self) {
        match &self.stage {
            Stage::Approving { cancel, .. } => cancel.store(true, Ordering::SeqCst),
            Stage::Running(r) => r.progress.cancel(),
            _ => {}
        }
    }
}

/// El flujo de limpieza.
pub(crate) struct CleanFlow {
    pub basket: Basket,
    stage: Stage,
    events: Vec<FlowEvent>,
    note: Option<Note>,
    /// Contexto del volumen limpiado (se recuerda de `start` a `finish`).
    vol: VolumeCtx,
    /// Pedido de "Volver a comprobar" desde el resumen (la exploración recalcula las recomendaciones).
    pub recheck: bool,
    /// Cachés de navegadores abiertos que NO se limpiaron y cuánto habría que liberar si se cerraran (lo pone la pantalla
    /// dueña a partir de las recomendaciones; el resumen lo enseña como "Cierra X para liberar hasta N más").
    pub blocked_hint: Vec<(Browser, u64)>,
}

impl Default for CleanFlow {
    fn default() -> CleanFlow {
        CleanFlow {
            basket: Basket::new(),
            stage: Stage::Idle,
            events: Vec::new(),
            note: None,
            vol: VolumeCtx::default(),
            recheck: false,
            blocked_hint: Vec::new(),
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// ¿Cuánto ocupa lo que se libera "de verdad" (definitivo + vaciar la Papelera + lo que no cabe si se confirmó)?
pub(crate) fn free_bytes(plan: &Plan, allow_unfit: bool) -> u64 {
    plan.permanent_bytes()
        + plan.bin.as_ref().map_or(0, |b| b.bytes_hint)
        + if allow_unfit { plan.unfit_bytes() } else { 0 }
}

impl CleanFlow {
    /// ¿Hay una ventana modal abierta (confirmación o ejecución)? Las teclas de la exploración no actúan.
    pub fn modal_open(&self) -> bool {
        matches!(self.stage, Stage::Confirm(_) | Stage::Running(_))
    }

    /// ¿Se está mostrando el resumen (ocupa toda la ventana)?
    pub fn in_summary(&self) -> bool {
        matches!(self.stage, Stage::Summary(_))
    }

    /// ¿Se está ejecutando la limpieza (borrando o enviando a la Papelera)? Cerrar la ventana entonces la dejaría a medias.
    pub fn is_running(&self) -> bool {
        matches!(self.stage, Stage::Running(_))
    }

    /// Anota que se intentó cerrar la ventana durante la ejecución (la ventana de progreso lo explica).
    pub fn block_close(&mut self) {
        if let Stage::Running(r) = &mut self.stage {
            r.close_blocked = true;
        }
    }

    /// ¿Hay algo en marcha (aprobando, confirmando o ejecutando)?
    pub fn busy(&self) -> bool {
        !matches!(self.stage, Stage::Idle | Stage::Summary(_))
    }

    /// Cierra el resumen (y cualquier otra cosa en curso que no esté ejecutando).
    pub fn close_summary(&mut self) {
        if matches!(self.stage, Stage::Summary(_)) {
            self.stage = Stage::Idle;
        }
    }

    /// Eventos pendientes para la pantalla dueña.
    pub fn take_events(&mut self) -> Vec<FlowEvent> {
        std::mem::take(&mut self.events)
    }

    /// Para capturas y pruebas con el respaldo SIMULADO: aprueba la cesta al instante y abre la confirmación.
    pub fn open_confirm_now(&mut self, backend: &Arc<dyn Backend>, vol: VolumeCtx) {
        let plan = backend.approve(
            &self.basket,
            &ApproveOptions::default(),
            &Arc::new(AtomicBool::new(false)),
        );
        self.vol = vol;
        self.stage = Stage::Confirm(Box::new(Confirm {
            plan: Arc::new(plan),
            allow_unfit: false,
            show_all: false,
            reconfirm: false,
            focused: true,
        }));
    }

    /// Para capturas y pruebas con el respaldo SIMULADO: aprueba y ejecuta al instante y deja el resumen.
    pub fn run_now(&mut self, backend: &Arc<dyn Backend>, vol: VolumeCtx, dry: bool) {
        let plan = backend.approve(
            &self.basket,
            &ApproveOptions::default(),
            &Arc::new(AtomicBool::new(false)),
        );
        self.vol = vol;
        let opts = ExecOptions {
            dry_run: dry,
            allow_unfit_permanent: false,
            log: None,
        };
        match backend.execute(&plan, &opts, &Progress::new()) {
            Ok(report) => self.finish(
                backend,
                report,
                dry,
                vol.volume.map(|(t, f)| t.saturating_sub(f)),
            ),
            // El error no se traga: se enseña con el mismo aviso que en el flujo normal (H6.7).
            Err(ExecError::LogRequired) => self.note = Some(Note::NoLog),
            Err(ExecError::NeedsReconfirm(_)) => self.note = Some(Note::Reconfirm),
            Err(ExecError::Elevated) => self.note = Some(Note::Elevated),
        }
    }

    /// Empieza la aprobación de la cesta (en un hilo); al terminar se abre la confirmación.
    pub fn start(
        &mut self,
        ctx: &egui::Context,
        backend: &Arc<dyn Backend>,
        vol: VolumeCtx,
        reconfirm: bool,
    ) {
        if self.busy() || self.basket.is_empty() {
            return;
        }
        self.vol = vol;
        let (tx, rx) = crossbeam_channel::bounded(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let (b, basket, c, ctx2) = (
            backend.clone(),
            self.basket.clone(),
            cancel.clone(),
            ctx.clone(),
        );
        std::thread::spawn(move || {
            let plan = b.approve(&basket, &ApproveOptions::default(), &c);
            let _ = tx.send(plan);
            ctx2.request_repaint();
        });
        self.stage = Stage::Approving {
            rx,
            cancel,
            reconfirm,
        };
    }

    /// Avanza el flujo (recoge resultados de los hilos). Devuelve el aviso a enseñar, si lo hay.
    pub fn poll(&mut self, ctx: &egui::Context, backend: &Arc<dyn Backend>, dry: bool) {
        match &mut self.stage {
            Stage::Approving { rx, reconfirm, .. } => match rx.try_recv() {
                Ok(plan) => {
                    let reconfirm = *reconfirm;
                    if plan.is_empty() && plan.rejected.is_empty() {
                        self.note = Some(Note::NothingToClean);
                        self.stage = Stage::Idle;
                    } else {
                        self.stage = Stage::Confirm(Box::new(Confirm {
                            plan: Arc::new(plan),
                            allow_unfit: false,
                            show_all: false,
                            reconfirm,
                            focused: false,
                        }));
                    }
                }
                Err(TryRecvError::Disconnected) => self.stage = Stage::Idle,
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                }
            },
            Stage::Running(run) => match run.rx.try_recv() {
                Ok(Ok(report)) => {
                    let (dry_run, allow_unfit) = (run.dry, run.allow_unfit);
                    let used_before = run.used_before;
                    let _ = (allow_unfit, dry);
                    self.finish(backend, report, dry_run, used_before);
                }
                Ok(Err(ExecError::LogRequired)) => {
                    // El ejecutor exige un registro utilizable: no se ha tocado nada.
                    self.stage = Stage::Idle;
                    self.note = Some(Note::NoLog);
                }
                Ok(Err(ExecError::Elevated)) => {
                    // Elevado: no se ha tocado nada (el `cleaner` lo rechaza antes de empezar).
                    self.stage = Stage::Idle;
                    self.note = Some(Note::Elevated);
                }
                Ok(Err(ExecError::NeedsReconfirm(_))) => {
                    // Lo que cabe en la Papelera cambió desde la confirmación: se vuelve a aprobar y a confirmar.
                    self.stage = Stage::Idle;
                    let vol = self.vol;
                    self.start(ctx, backend, vol, true);
                }
                Err(TryRecvError::Disconnected) => self.stage = Stage::Idle,
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(80));
                }
            },
            _ => {}
        }
    }

    fn finish(
        &mut self,
        backend: &Arc<dyn Backend>,
        report: Report,
        dry: bool,
        used_before: Option<u64>,
    ) {
        if !dry {
            let removed: Vec<Removed> = report
                .items
                .iter()
                .flat_map(|i| i.removed.clone())
                .collect();
            if !removed.is_empty() {
                self.events.push(FlowEvent::Removed(removed));
            }
            if let Some(dir) = backend.data_dir() {
                let e = HistoryEntry {
                    unix: unix_now(),
                    drive: self.vol.drive.map_or(0, |c| c as u8),
                    used: used_before.unwrap_or(0),
                    freed_now: report.freed_now.unwrap_or(0).max(0) as u64,
                    to_bin: report.recycled_bytes,
                };
                let _ = history_append(&dir, &e);
            }
            // Todo lo que estaba en la cesta se intentó: la cesta queda vacía (lo que falló se puede volver a añadir).
            self.basket.clear();
        }
        // Un navegador abierto: lo que quedó sin limpiar de su caché ("Cierra X para liberar hasta N más").
        let open = backend.running_browsers();
        let blocked: Vec<(Browser, u64)> = self
            .blocked_hint
            .iter()
            .filter(|(b, bytes)| open.contains(b) && *bytes > 0)
            .copied()
            .collect();
        self.stage = Stage::Summary(Box::new(Summary {
            report,
            vol: self.vol,
            blocked,
        }));
    }

    // ---------------------------------------------------------------------------------------------------------------
    // Ventanas modales: confirmación y ejecución
    // ---------------------------------------------------------------------------------------------------------------

    /// Pinta la confirmación o el progreso, si toca, y atiende sus botones.
    pub fn show_modals(
        &mut self,
        ctx: &egui::Context,
        env: &mut Env,
        vol: VolumeCtx,
        used_before: Option<u64>,
    ) {
        let backend = env.backend.clone();
        let mut go: Option<(bool, bool)> = None; // (dry, allow_unfit)
        let mut cancel_confirm = false;
        let mut cancel_run = false;
        match &mut self.stage {
            Stage::Confirm(c) => confirm_ui(ctx, env, c, &mut go, &mut cancel_confirm),
            Stage::Running(r) => running_ui(ctx, env, r, &mut cancel_run),
            _ => {}
        }
        if let Some(note) = &self.note {
            let (key, secs) = match note {
                Note::NothingToClean => ("confirm-empty", 4.0),
                // Más larga: explica por qué no pasó nada.
                Note::NoLog => ("confirm-no-log", 12.0),
                Note::Reconfirm => ("confirm-reconfirm", 8.0),
                // Larga: dice qué hacer.
                Note::Elevated => ("confirm-elevated", 12.0),
            };
            let msg = env.tr.t(key);
            toast(ctx, &msg);
            // El aviso dura unos segundos.
            let until = ctx.data_mut(|d| {
                *d.get_temp_mut_or_insert_with(egui::Id::new("tesela-note-until"), || {
                    ctx.input(|i| i.time) + secs
                })
            });
            if ctx.input(|i| i.time) > until {
                self.note = None;
                ctx.data_mut(|d| d.remove::<f64>(egui::Id::new("tesela-note-until")));
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
        }
        if cancel_confirm {
            self.stage = Stage::Idle;
        }
        if cancel_run && let Stage::Running(r) = &mut self.stage {
            r.progress.cancel();
            r.cancelled = true;
        }
        if let Some((dry, allow_unfit)) = go
            && let Stage::Confirm(c) = &self.stage
        {
            let plan = c.plan.clone();
            // E1: elevado no se limpia (también lo rechaza el `cleaner`; aquí se avisa antes de crear el registro).
            if blocked_by_elevation(backend.is_elevated(), dry, backend.is_demo()) {
                self.note = Some(Note::Elevated);
                ctx.request_repaint();
                return;
            }
            // SIN REGISTRO NO HAY ACCIÓN DESTRUCTIVA (M4): si el registro local no se puede crear, no se ejecuta nada y se
            // dice por qué. La simulación (y la demostración, que nunca toca el disco) puede seguir sin él.
            let log = backend
                .data_dir()
                .and_then(|d| LogWriter::create(&d, dry).ok())
                .map(Arc::new);
            if abort_without_log(log.is_some(), dry, backend.is_demo()) {
                self.note = Some(Note::NoLog);
                ctx.request_repaint();
                return;
            }
            self.vol = vol;
            let progress = Progress::new();
            let (tx, rx) = crossbeam_channel::bounded(1);
            let opts = ExecOptions {
                dry_run: dry,
                allow_unfit_permanent: allow_unfit,
                log,
            };
            let (b, p, pr, ctx2) = (backend.clone(), plan.clone(), progress.clone(), ctx.clone());
            std::thread::spawn(move || {
                let r = b.execute(&p, &opts, &pr);
                let _ = tx.send(r);
                ctx2.request_repaint();
            });
            self.stage = Stage::Running(Box::new(Running {
                progress,
                rx,
                dry,
                allow_unfit,
                cancelled: false,
                started: Instant::now(),
                used_before,
                close_blocked: false,
            }));
        }
    }
}

/// ¿Hay que abortar la limpieza por falta de registro (M4)? Sin registro no hay acción destructiva: solo la simulación
/// (que no toca nada) y la demostración (que nunca toca el disco) pueden seguir sin él.
fn abort_without_log(has_log: bool, dry: bool, is_demo: bool) -> bool {
    !has_log && !dry && !is_demo
}

/// ¿Hay que negarse a limpiar porque el proceso está elevado (E1)? Solo la limpieza REAL: la simulación y la demostración
/// (que nunca toca el disco) siguen, y el análisis no pasa por aquí.
fn blocked_by_elevation(elevated: bool, dry: bool, is_demo: bool) -> bool {
    elevated && !dry && !is_demo
}

fn toast(ctx: &egui::Context, msg: &str) {
    egui::Area::new(egui::Id::new("tesela-flow-toast"))
        .anchor(egui::Align2::CENTER_BOTTOM, vec2(0.0, -110.0))
        .order(egui::Order::Tooltip)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(msg);
            });
        });
}

/// Nombre para mostrar de un elemento del plan.
fn item_name(env: &Env, it: &PlanItem) -> String {
    match it.category {
        Some(CategoryId::UserTemp) => env.tr.t("known-user-temp"),
        Some(CategoryId::WindowsTemp) => env.tr.t("known-windows-temp"),
        Some(CategoryId::ChromeCache) => env.tr.t("known-chrome-cache"),
        Some(CategoryId::EdgeCache) => env.tr.t("known-edge-cache"),
        Some(CategoryId::FirefoxCache) => env.tr.t("known-firefox-cache"),
        Some(CategoryId::Downloads) => env.tr.t("known-downloads"),
        Some(CategoryId::RecycleBin) => env.tr.t("known-recycle-bin"),
        _ => it.path.file_name().map_or_else(
            || it.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
    }
}

/// ¿Alguno no cabe porque la Papelera de su unidad no tiene tamaño máximo configurado?
fn has_capacity_unknown(fits: impl Iterator<Item = Fit>) -> bool {
    fits.into_iter()
        .any(|f| f == Fit::Unfit(UnfitReason::CapacityUnknown))
}

fn unfit_reason_key(r: UnfitReason) -> &'static str {
    match r {
        UnfitReason::NoBin => "unfit-no-bin",
        UnfitReason::NukeOnDelete => "unfit-nuke",
        UnfitReason::TooBig => "unfit-too-big",
        UnfitReason::PathTooLong => "unfit-path-long",
        UnfitReason::Policy => "unfit-policy",
        UnfitReason::Unknown => "unfit-unknown",
        UnfitReason::CapacityUnknown => "unfit-capacity-unknown",
    }
}

/// `hover`: la ruta completa, que sale al pasar el ratón por el nombre (distingue elementos con el mismo nombre).
fn row(
    ui: &mut Ui,
    pal: &Palette,
    left: &str,
    chip: Option<&str>,
    size: &str,
    muted: Option<&str>,
    hover: Option<&str>,
) {
    ui.horizontal(|ui| {
        ui.set_min_height(26.0);
        let name = ui.label(RichText::new(left).color(pal.text));
        if let Some(h) = hover {
            name.on_hover_text(h);
        }
        if let Some(m) = muted {
            let l = ui.label(RichText::new(m).size(12.0).color(pal.text_muted));
            if let Some(h) = hover {
                l.on_hover_text(h);
            }
        }
        if let Some(c) = chip {
            ui.label(
                RichText::new(c)
                    .size(12.0)
                    .font(theme::font_semibold(12.0))
                    .color(pal.review),
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(size).color(pal.text));
        });
    });
    let y = ui.cursor().top();
    ui.painter().hline(
        ui.max_rect().x_range(),
        y,
        Stroke::new(1.0, pal.border.gamma_multiply(0.6)),
    );
}

fn group_header(ui: &mut Ui, env: &mut Env, icon: Icon, title: &str, size: &str) {
    let pal = env.pal;
    ui.horizontal(|ui| {
        env.icons.show(ui, icon, 18.0, pal.text);
        ui.label(
            RichText::new(title)
                .font(theme::font_semibold(15.0))
                .color(pal.text),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(size)
                    .font(theme::font_semibold(15.0))
                    .color(pal.text),
            );
        });
    });
}

fn note_line(ui: &mut Ui, pal: &Palette, text: &str, warn: bool) {
    ui.add(
        egui::Label::new(RichText::new(text).size(13.0).color(if warn {
            pal.review
        } else {
            pal.text_muted
        }))
        .wrap(),
    );
}

/// Etiqueta del botón principal según lo que haya.
fn go_label(env: &Env, plan: &Plan, allow_unfit: bool, dry: bool) -> String {
    let free = free_bytes(plan, allow_unfit);
    let bin = plan.recycle_bytes();
    if dry {
        return env.tr.t("confirm-go-dry");
    }
    let has_free =
        free > 0 || plan.bin.is_some() || (allow_unfit && plan.unfit_items().count() > 0);
    let has_bin = plan.recycle_items().count() > 0;
    match (has_free, has_bin) {
        (true, true) => env.tr.ta(
            "confirm-go-both",
            args!["free" => env.tr.size(free), "bin" => env.tr.size(bin)],
        ),
        (true, false) => env
            .tr
            .ta("confirm-go-free", args!["free" => env.tr.size(free)]),
        (false, true) => env
            .tr
            .ta("confirm-go-bin", args!["bin" => env.tr.size(bin)]),
        (false, false) => env.tr.t("confirm-go-free-zero"),
    }
}

fn confirm_ui(
    ctx: &egui::Context,
    env: &mut Env,
    c: &mut Confirm,
    go: &mut Option<(bool, bool)>,
    cancel: &mut bool,
) {
    let pal = env.pal;
    let plan = c.plan.clone();
    let dry = env.dry_run;
    let frame = egui::Frame::new()
        .fill(pal.surface)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(0, 0));
    let modal = egui::Modal::new(egui::Id::new("tesela-confirm")).frame(frame).show(ctx, |ui| {
        ui.set_width(640.0);
        ui.set_max_height(ctx.content_rect().height() - 80.0);
        egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(ctx.content_rect().height() - 190.0).show(ui, |ui| {
            egui::Frame::new().inner_margin(Margin::symmetric(28, 24)).show(ui, |ui| {
                let title = env.tr.t("confirm-title");
                let r = ui.label(RichText::new(&title).font(theme::font_semibold(24.0)).color(pal.text));
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, title.clone()));
                let n_items = plan.items.len() as u64 + u64::from(plan.bin.is_some());
                let total = free_bytes(&plan, c.allow_unfit) + plan.recycle_bytes();
                ui.label(
                    RichText::new(env.tr.ta(
                        "confirm-subtitle",
                        args!["n" => n_items, "count" => env.tr.int(n_items), "size" => env.tr.size(total)],
                    ))
                    .color(pal.text_muted),
                );
                if dry {
                    ui.add_space(6.0);
                    note_line(ui, &pal, &env.tr.t("confirm-dry-banner"), true);
                }
                if c.reconfirm {
                    ui.add_space(6.0);
                    note_line(ui, &pal, &env.tr.t("confirm-reconfirm"), true);
                }
                ui.add_space(14.0);
                confirm_groups(ui, env, c, &plan);
            });
        });
        // Pie: enlace, cancelar y el botón principal.
        egui::Frame::new()
            .fill(pal.bg)
            .corner_radius(CornerRadius { nw: 0, ne: 0, sw: 12, se: 12 })
            .inner_margin(Margin::symmetric(28, 16))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let see = if c.show_all { env.tr.t("confirm-hide-all") } else { env.tr.t("confirm-see-all") };
                    if ui.link(see).clicked() {
                        c.show_all = !c.show_all;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let unfit = plan.unfit_items().count() > 0;
                        let nothing = plan.items.is_empty() && plan.bin.is_none();
                        let label = go_label(env, &plan, c.allow_unfit, dry);
                        let enabled = !nothing;
                        let r = widgets::button(ui, env, ButtonStyle::Primary, Some(Icon::ShieldCheckmark), &label, enabled);
                        if r.clicked() {
                            *go = Some((dry, c.allow_unfit && unfit));
                        }
                        let cancel_label = env.tr.t("cancel");
                        let rc = widgets::button(ui, env, ButtonStyle::Secondary, None, &cancel_label, true);
                        if !c.focused {
                            // El foco empieza en "Cancelar": un Intro suelto nunca borra nada.
                            rc.request_focus();
                            c.focused = true;
                        }
                        if rc.clicked() {
                            *cancel = true;
                        }
                    });
                });
            });
    });
    if modal.should_close() {
        *cancel = true;
    }
}

fn confirm_groups(ui: &mut Ui, env: &mut Env, c: &mut Confirm, plan: &Plan) {
    let pal = env.pal;
    // 1. Se borra y se vuelve a crear sola.
    let perm: Vec<&PlanItem> = plan.permanent_items().collect();
    if !perm.is_empty() {
        let sum: u64 = perm.iter().map(|i| i.bytes()).sum();
        group_header(
            ui,
            env,
            Icon::DeleteOff,
            &env.tr.t("confirm-group-permanent"),
            &env.tr.size(sum),
        );
        ui.add_space(4.0);
        for it in &perm {
            let upto =
                it.min_age.as_secs() > 0 || it.category.is_some_and(CategoryId::is_browser_cache);
            let size = if upto {
                env.tr
                    .ta("confirm-upto", args!["size" => env.tr.size(it.bytes())])
            } else {
                env.tr.size(it.bytes())
            };
            let (folder, full) = folder_hint(env, it);
            row(
                ui,
                &pal,
                &item_name(env, it),
                None,
                &size,
                folder.as_deref(),
                full.as_deref(),
            );
        }
        ui.add_space(4.0);
        note_line(ui, &pal, &env.tr.t("confirm-permanent-note"), false);
        ui.add_space(16.0);
    }
    // 2. Vaciar la Papelera (alcance enumerado).
    if let Some(b) = &plan.bin {
        group_header(
            ui,
            env,
            Icon::BinRecycle,
            &env.tr.t("confirm-group-bin"),
            &env.tr.size(b.bytes_hint),
        );
        ui.add_space(4.0);
        note_line(
            ui,
            &pal,
            &env.tr.ta(
                "confirm-bin-note",
                args!["n" => b.items.len() as u64, "count" => env.tr.int(b.items.len() as u64), "size" => env.tr.size(b.bytes_hint)],
            ),
            false,
        );
        ui.add_space(16.0);
    }
    // 3. Va a la Papelera.
    let rec: Vec<&PlanItem> = plan.recycle_items().collect();
    if !rec.is_empty() {
        let sum: u64 = rec.iter().map(|i| i.bytes()).sum();
        group_header(
            ui,
            env,
            Icon::Delete,
            &env.tr.t("confirm-group-recycle"),
            &env.tr.size(sum),
        );
        ui.add_space(4.0);
        let mut sorted = rec.clone();
        sorted.sort_by(|a, b| b.bytes().cmp(&a.bytes()).then_with(|| a.path.cmp(&b.path)));
        let shown = if c.show_all {
            sorted.len()
        } else {
            sorted.len().min(5)
        };
        for it in &sorted[..shown] {
            recycle_row(ui, env, it);
        }
        if shown < sorted.len() {
            ui.label(
                RichText::new(env.tr.ta("confirm-more", args!["n" => (sorted.len() - shown) as u64, "count" => env.tr.int((sorted.len() - shown) as u64)]))
                    .size(13.0)
                    .color(pal.text_muted),
            );
        }
        ui.add_space(4.0);
        note_line(ui, &pal, &env.tr.t("confirm-recycle-note"), false);
        ui.add_space(16.0);
    }
    // 4. Lo que no cabe en la Papelera: grupo ámbar con su casilla.
    let unfit: Vec<&PlanItem> = plan.unfit_items().collect();
    if !unfit.is_empty() {
        egui::Frame::new()
            .fill(theme::mix(pal.review, pal.surface, 0.88))
            .stroke(Stroke::new(1.0, pal.review))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let sum: u64 = unfit.iter().map(|i| i.bytes()).sum();
                ui.horizontal(|ui| {
                    env.icons.show(ui, Icon::Warning, 18.0, pal.review);
                    ui.label(
                        RichText::new(env.tr.t("confirm-group-unfit"))
                            .font(theme::font_semibold(15.0))
                            .color(pal.text),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(env.tr.size(sum))
                                .font(theme::font_semibold(15.0))
                                .color(pal.text),
                        );
                    });
                });
                for it in &unfit {
                    let why = match it.fit {
                        Fit::Unfit(r) => env.tr.t(unfit_reason_key(r)),
                        Fit::Fits => String::new(),
                    };
                    row(
                        ui,
                        &pal,
                        &item_name(env, it),
                        None,
                        &env.tr.size(it.bytes()),
                        Some(&why),
                        Some(&it.path.display().to_string()),
                    );
                }
                // Sin tamaño máximo configurado, cómo arreglarlo (H6.3).
                if has_capacity_unknown(unfit.iter().map(|i| i.fit)) {
                    note_line(ui, &pal, &env.tr.t("confirm-unfit-capacity-hint"), false);
                }
                ui.add_space(6.0);
                let label = env.tr.t("confirm-unfit-check");
                ui.checkbox(&mut c.allow_unfit, &label);
                note_line(ui, &pal, &env.tr.t("confirm-unfit-note"), false);
            });
        ui.add_space(16.0);
    }
    // 5. Avisos: enlaces duros, nube, protegidos, rechazados.
    let (shared_n, shared_b) = plan.shared_total();
    if shared_n > 0 {
        note_line(
            ui,
            &pal,
            &env.tr.ta("confirm-hardlink", args!["n" => shared_n, "count" => env.tr.int(shared_n), "size" => env.tr.size(shared_b)]),
            true,
        );
    }
    if plan.items.iter().any(|i| i.cloud_synced) {
        note_line(ui, &pal, &env.tr.t("confirm-cloud"), true);
    }
    let (prot_n, prot_b) = plan.protected_total();
    if prot_n > 0 {
        let text = if prot_b > 0 {
            env.tr.ta(
                "confirm-protected-size",
                args!["n" => prot_n, "count" => env.tr.int(prot_n), "size" => env.tr.size(prot_b)],
            )
        } else {
            env.tr.ta(
                "confirm-protected",
                args!["n" => prot_n, "count" => env.tr.int(prot_n)],
            )
        };
        note_line(ui, &pal, &text, false);
    }
    if !plan.rejected.is_empty() {
        let n = plan.rejected.len() as u64;
        note_line(
            ui,
            &pal,
            &env.tr.ta(
                "confirm-rejected",
                args!["n" => n, "count" => env.tr.int(n)],
            ),
            true,
        );
        if c.show_all {
            for r in plan.rejected.iter().take(30) {
                let why = match r.reason {
                    RejectReason::Incomplete(k) => env.tr.ta(
                        "reject-incomplete",
                        args!["n" => k, "count" => env.tr.int(k)],
                    ),
                    RejectReason::Missing => env.tr.t("reject-missing"),
                    RejectReason::AccessDenied => env.tr.t("reject-denied"),
                    RejectReason::NoIdentity => env.tr.t("reject-identity"),
                    RejectReason::Reparse => env.tr.t("reject-reparse"),
                    RejectReason::ReparseAncestor => env.tr.t("reject-reparse-ancestor"),
                    RejectReason::OutsideLocalAppData => env.tr.t("reject-outside-local"),
                    RejectReason::Protected(Verdict::Denied) => env.tr.t("reject-protected"),
                    RejectReason::Protected(_) => env.tr.t("reject-untouchable"),
                    RejectReason::NotLocal => env.tr.t("reject-not-local"),
                    RejectReason::Other(_) => env.tr.t("reject-other"),
                };
                let name = r.path.file_name().map_or_else(
                    || r.path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                row(
                    ui,
                    &pal,
                    &name,
                    None,
                    &why,
                    None,
                    Some(&r.path.display().to_string()),
                );
            }
        }
    }
}

fn recycle_row(ui: &mut Ui, env: &mut Env, it: &PlanItem) {
    let pal = env.pal;
    let name = item_name(env, it);
    let chip = it.category.is_none().then(|| env.tr.t("confirm-your-file"));
    let mut size = env.tr.size(it.bytes());
    if it.hard_linked || it.estimate.shared_files > 0 {
        size = env.tr.t("confirm-no-space");
    }
    let (folder, full) = folder_hint(env, it);
    row(
        ui,
        &pal,
        &name,
        chip.as_deref(),
        &size,
        folder.as_deref(),
        full.as_deref(),
    );
}

/// Para los archivos y carpetas del usuario ("Tu archivo"): "en <carpeta padre>" y la ruta completa (para el ratón). Las
/// categorías con regla se llaman por su nombre conocido y no lo necesitan.
fn folder_hint(env: &Env, it: &PlanItem) -> (Option<String>, Option<String>) {
    if it.category.is_some() {
        return (None, None);
    }
    let folder = it.path.parent().map(|p| {
        p.file_name().map_or_else(
            || p.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    });
    (
        folder.map(|f| env.tr.ta("confirm-in-folder", args!["folder" => f])),
        Some(it.path.display().to_string()),
    )
}

fn running_ui(ctx: &egui::Context, env: &mut Env, r: &mut Running, cancel: &mut bool) {
    let pal = env.pal;
    let frame = egui::Frame::new()
        .fill(pal.surface)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(28, 24));
    egui::Modal::new(egui::Id::new("tesela-running")).frame(frame).show(ctx, |ui| {
        ui.set_width(520.0);
        let title = env.tr.t(if r.dry { "run-title-dry" } else { "run-title" });
        let t = ui.label(RichText::new(&title).font(theme::font_semibold(22.0)).color(pal.text));
        t.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, title.clone()));
        ui.add_space(10.0);
        let phase = match r.progress.phase() {
            Phase::Preparing => "run-phase-preparing",
            Phase::EmptyingBin => "run-phase-bin",
            Phase::Deleting => "run-phase-delete",
            Phase::Recycling => "run-phase-recycle",
            Phase::Verifying | Phase::Done => "run-phase-verify",
        };
        ui.horizontal(|ui| {
            widgets::busy(ui, env.animate, 18.0, pal.accent);
            ui.label(RichText::new(env.tr.t(phase)).color(pal.text));
        });
        ui.add_space(6.0);
        let total = r.progress.items_total.load(Ordering::Relaxed).max(1);
        let done = r.progress.items_done.load(Ordering::Relaxed).min(total);
        let entries = r.progress.entries.load(Ordering::Relaxed);
        widgets::progress_bar(ui, &pal, done as f32 / total as f32, pal.accent, 8.0, false, None);
        ui.add_space(4.0);
        ui.label(
            RichText::new(env.tr.ta(
                "run-progress",
                args!["done" => done as u64, "total" => total as u64, "n" => entries, "count" => env.tr.int(entries)],
            ))
            .size(13.0)
            .color(pal.text_muted),
        );
        if r.cancelled {
            ui.add_space(6.0);
            note_line(ui, &pal, &env.tr.t("run-cancelling"), true);
        }
        if r.close_blocked {
            ui.add_space(6.0);
            note_line(ui, &pal, &env.tr.t("run-close-blocked"), true);
        }
        ui.add_space(14.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = env.tr.t("cancel");
            if widgets::button(ui, env, ButtonStyle::Secondary, None, &label, !r.cancelled).clicked() {
                *cancel = true;
            }
        });
    });
    let _ = r.started;
}

// -------------------------------------------------------------------------------------------------------------------
// Resumen
// -------------------------------------------------------------------------------------------------------------------

/// Lo que el usuario eligió en el resumen.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SummaryClick {
    Back,
    EmptyNow,
    OpenBin,
    Recheck,
}

impl CleanFlow {
    /// Pinta el resumen (ocupa todo el espacio disponible) y devuelve lo que se pulsó.
    pub fn show_summary(&mut self, ui: &mut Ui, env: &mut Env) -> Option<SummaryClick> {
        let Stage::Summary(s) = &self.stage else {
            return None;
        };
        let click = summary_ui(ui, env, s);
        match click {
            Some(SummaryClick::Back) => self.stage = Stage::Idle,
            Some(SummaryClick::Recheck) => {
                self.recheck = true;
                self.stage = Stage::Idle;
            }
            _ => {}
        }
        click
    }
}

fn summary_lines(env: &Env, s: &Summary) -> Vec<(String, bool)> {
    let r = &s.report;
    let t = &r.totals;
    let tr = env.tr;
    let mut v: Vec<(String, bool)> = Vec::new();
    let n = |k: &str, n: u64| tr.ta(k, args!["n" => n, "count" => tr.int(n)]);
    // Las dos cosas pueden darse a la vez y se dicen las dos: la cancelación del usuario y el fallo del registro (que
    // detiene la limpieza solo).
    if r.cancelled {
        v.push((tr.t("sum-cancelled"), true));
    }
    if r.log_failed {
        v.push((tr.t("sum-log-failed"), true));
    }
    if r.log_truncated {
        v.push((tr.t("sum-log-truncated"), true));
    }
    if r.deleted_unexpectedly > 0 {
        v.push((n("sum-unexpected", r.deleted_unexpectedly), true));
    }
    if let Some(code) = r.shell_error {
        v.push((
            tr.ta(
                "sum-shell-error",
                args!["code" => format!("{:08X}", code as u32)],
            ),
            true,
        ));
    }
    if r.recycle_stopped {
        let key = match r.stop_reason {
            Some(StopReason::Unverified) => "sum-recycle-stopped-unverified",
            Some(StopReason::Changed) => "sum-recycle-stopped-changed",
            Some(StopReason::ShellDeclined) => "sum-recycle-stopped-declined",
            _ => "sum-recycle-stopped",
        };
        v.push((n(key, r.stopped_items), true));
    }
    if r.no_room > 0 {
        v.push((n("sum-no-room", r.no_room), true));
    }
    if r.recycled_other > 0 {
        v.push((n("sum-recycled-other", r.recycled_other), true));
    }
    if r.unverified > 0 {
        v.push((n("sum-unverified", r.unverified), true));
    }
    // `not_recycled` también cuenta los que no tuvieron sitio y los del corte de seguridad (que ya tienen su línea): aquí
    // solo el resto, los que Windows no movió y se comprobó que siguen intactos en su sitio.
    let checked_in_place = r.not_recycled.saturating_sub(r.no_room + r.stopped_items);
    if checked_in_place > 0 {
        v.push((n("sum-not-recycled", checked_in_place), true));
    }
    if r.skipped_unfit > 0 {
        v.push((n("sum-skipped-unfit", r.skipped_unfit), false));
    }
    if t.in_use > 0 {
        v.push((n("sum-in-use", t.in_use), false));
    }
    if t.denied > 0 {
        v.push((n("sum-denied", t.denied), false));
    }
    if t.changed > 0 {
        v.push((n("sum-changed", t.changed), true));
    }
    if t.protected > 0 {
        v.push((n("sum-protected", t.protected), false));
    }
    if t.reparse > 0 {
        v.push((n("sum-reparse", t.reparse), false));
    }
    if t.young > 0 {
        v.push((n("sum-young", t.young), false));
    }
    if t.shared_files > 0 {
        v.push((n("sum-shared", t.shared_files), false));
    }
    if let Some(b) = &r.bin {
        if b.changed > 0 {
            v.push((n("sum-bin-changed", b.changed), true));
        }
        if b.unverifiable > 0 {
            v.push((n("sum-bin-unverifiable", b.unverifiable), true));
        }
        if b.replaced > 0 {
            v.push((n("sum-bin-replaced", b.replaced), true));
        }
        let explained = b.changed + b.unverifiable + b.replaced;
        if b.still_there > explained {
            v.push((n("sum-bin-failed", b.still_there - explained), true));
        }
    }
    // Si "liberado ahora" y "eliminado" divergen, se dice por qué en una línea.
    if let Some(f) = r.freed_now {
        let planned = r.permanent_bytes + r.bin.map_or(0, |b| b.bytes_hint);
        let diff = (f.max(0) as u64).abs_diff(planned);
        if planned > 0 && diff * 20 > planned {
            v.push((tr.t("sum-diverge"), false));
        }
    }
    v
}

fn counter(ui: &mut Ui, pal: &Palette, label: &str, value: &str) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(RichText::new(label).size(12.0).color(pal.text_muted));
        ui.label(
            RichText::new(value)
                .font(theme::font_semibold(18.0))
                .color(pal.text),
        );
    });
}

fn summary_ui(ui: &mut Ui, env: &mut Env, s: &Summary) -> Option<SummaryClick> {
    let pal = env.pal;
    let r = &s.report;
    let mut click = None;
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(pal.bg).inner_margin(Margin::symmetric(64, 32)))
        .show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(12.0);
                    let (rect, _) = ui.allocate_exact_size(vec2(56.0, 56.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 28.0, theme::mix(pal.safe, pal.bg, 0.82));
                    env.icons.paint(
                        ui.painter(),
                        egui::Rect::from_center_size(rect.center(), vec2(28.0, 28.0)),
                        Icon::Checkmark,
                        pal.safe,
                    );
                    ui.add_space(6.0);
                    let head = env.tr.t(if r.dry_run { "sum-dry" } else { "sum-freed" });
                    ui.label(RichText::new(&head).size(15.0).color(pal.text_muted));
                    // "Liberado ahora" es el CAMBIO medido del espacio libre: otros programas que escriban a la vez también
                    // lo mueven. Se dice, y los totales eliminado / a la Papelera se cuentan aparte.
                    if !r.dry_run {
                        ui.label(
                            RichText::new(env.tr.t("sum-freed-hint"))
                                .size(12.0)
                                .color(pal.text_muted),
                        );
                    }
                    let big = if r.dry_run {
                        r.permanent_bytes + r.bin.map_or(0, |b| b.bytes_hint)
                    } else {
                        r.freed_now.unwrap_or(0).max(0) as u64
                    };
                    let big_txt = env.tr.size(big);
                    let l = ui.label(RichText::new(&big_txt).font(theme::font_semibold(64.0)).color(pal.text));
                    l.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("{head}: {big_txt}"))
                    });
                    ui.add_space(14.0);
                    // Los tres contadores.
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 40.0;
                        let total_w = 3.0 * 170.0;
                        ui.add_space(((ui.available_width() - total_w) / 2.0).max(0.0));
                        counter(ui, &pal, &env.tr.t("sum-permanent"), &env.tr.size(r.permanent_bytes));
                        counter(ui, &pal, &env.tr.t("sum-recycled"), &env.tr.size(r.recycled_bytes));
                        if let Some(b) = r.bin {
                            counter(
                                ui,
                                &pal,
                                &env.tr.t("sum-bin-purged"),
                                &env.tr.ta("sum-bin-items", args!["n" => b.purged, "count" => env.tr.int(b.purged)]),
                            );
                        }
                    });
                    ui.add_space(18.0);
                    // Antes y ahora (con las cifras del volumen).
                    if let (Some((total, free_before)), false) = (s.vol.volume, r.dry_run) {
                        let freed = r.freed_now.unwrap_or(0);
                        let free_after = (free_before as i64 + freed).clamp(0, total as i64) as u64;
                        ui.allocate_ui_with_layout(vec2(560.0, 0.0), Layout::top_down(Align::Min), |ui| {
                            for (key, free, bold) in [("sum-before", free_before, false), ("sum-now", free_after, true)] {
                                let used = total.saturating_sub(free);
                                let frac = used as f32 / total.max(1) as f32;
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(env.tr.t(key)).color(pal.text_muted));
                                    let w = (ui.available_width() - 190.0).max(60.0);
                                    let (rect, _) = ui.allocate_exact_size(vec2(w, 12.0), egui::Sense::hover());
                                    ui.painter().rect_filled(rect, CornerRadius::same(6), pal.track);
                                    let col = if frac >= 0.9 { pal.review } else { pal.accent };
                                    ui.painter().rect_filled(
                                        egui::Rect::from_min_size(rect.min, vec2(rect.width() * frac, 12.0)),
                                        CornerRadius::same(6),
                                        col,
                                    );
                                    let txt = env.tr.ta(
                                        "sum-used-pct",
                                        args!["used" => env.tr.size(used), "pct" => env.tr.pct(frac as f64 * 100.0)],
                                    );
                                    let rt = RichText::new(txt).color(pal.text);
                                    ui.label(if bold { rt.font(theme::font_semibold(14.0)) } else { rt });
                                });
                            }
                        });
                        ui.add_space(18.0);
                    }
                    // Tarjeta de la Papelera: "Vaciar ahora" es el botón principal si lo que hay supera lo liberado.
                    if r.recycled_bytes > 0 && !r.dry_run {
                        let bin_all = r.bin_total_after.unwrap_or(r.recycled_bytes);
                        let primary = r.recycled_bytes > r.freed_now.unwrap_or(0).max(0) as u64;
                        egui::Frame::new()
                            .fill(pal.surface)
                            .stroke(Stroke::new(1.0, pal.border))
                            .corner_radius(CornerRadius::same(12))
                            .inner_margin(Margin::symmetric(24, 20))
                            .show(ui, |ui| {
                                ui.set_width(592.0);
                                ui.horizontal(|ui| {
                                    env.icons.show(ui, Icon::Delete, 28.0, pal.text);
                                    ui.vertical(|ui| {
                                        ui.label(
                                            RichText::new(env.tr.ta("sum-bin-card", args!["size" => env.tr.size(r.recycled_bytes)]))
                                                .font(theme::font_semibold(16.0))
                                                .color(pal.text),
                                        );
                                        ui.label(RichText::new(env.tr.t("sum-bin-card-note")).size(13.0).color(pal.text_muted));
                                        // "Vaciar ahora" vacía TODA la Papelera (la lista enumerada al confirmar), no solo lo recién enviado.
                                        if bin_all > r.recycled_bytes {
                                            ui.label(
                                                RichText::new(env.tr.ta("sum-bin-card-all", args!["total" => env.tr.size(bin_all)]))
                                                    .size(13.0)
                                                    .color(pal.text_muted),
                                            );
                                        }
                                    });
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        let label = env.tr.ta("sum-empty-now", args!["size" => env.tr.size(bin_all)]);
                                        let style = if primary { ButtonStyle::Primary } else { ButtonStyle::Secondary };
                                        if widgets::button(ui, env, style, None, &label, true).clicked() {
                                            click = Some(SummaryClick::EmptyNow);
                                        }
                                    });
                                });
                            });
                        ui.add_space(14.0);
                    }
                    // Líneas de aviso.
                    ui.allocate_ui_with_layout(vec2(640.0, 0.0), Layout::top_down(Align::Min), |ui| {
                        for (text, warn) in summary_lines(env, s) {
                            ui.horizontal_top(|ui| {
                                env.icons.show(ui, if warn { Icon::Warning } else { Icon::Info }, 16.0, if warn { pal.review } else { pal.text_muted });
                                ui.add(egui::Label::new(RichText::new(text).color(pal.text)).wrap());
                            });
                            ui.add_space(4.0);
                        }
                        for (b, bytes) in &s.blocked {
                            ui.horizontal_top(|ui| {
                                env.icons.show(ui, Icon::Info, 16.0, pal.text_muted);
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(
                                            env.tr.ta("sum-browser-open", args!["name" => b.name(), "size" => env.tr.size(*bytes)]),
                                        )
                                        .color(pal.text),
                                    )
                                    .wrap(),
                                );
                                if ui.link(env.tr.t("recs-recheck")).clicked() {
                                    click = Some(SummaryClick::Recheck);
                                }
                            });
                            ui.add_space(4.0);
                        }
                    });
                    ui.add_space(20.0);
                    ui.horizontal(|ui| {
                        let w = 2.0 * 190.0;
                        ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                        let open_label = env.tr.t("sum-open-bin");
                        let b = widgets::button(ui, env, ButtonStyle::Secondary, None, &open_label, !env.backend.is_demo());
                        if env.backend.is_demo() {
                            b.clone().on_hover_text(env.tr.t("demo-unavailable"));
                        }
                        if b.clicked() {
                            click = Some(SummaryClick::OpenBin);
                        }
                        let back_label = env.tr.t("sum-back");
                        if widgets::button(ui, env, ButtonStyle::Ghost, None, &back_label, true).clicked() {
                            click = Some(SummaryClick::Back);
                        }
                    });
                });
            });
        });
    click
}

#[cfg(test)]
mod log_gate_tests {
    use super::{abort_without_log, has_capacity_unknown};
    use tesela_cleaner::{Fit, UnfitReason};

    /// H6.3: la pista de "fija un tamaño máximo" solo sale si algo no cabe por `CapacityUnknown`.
    #[test]
    fn la_pista_de_la_capacidad_solo_sale_con_capacity_unknown() {
        assert!(has_capacity_unknown(
            [
                Fit::Unfit(UnfitReason::TooBig),
                Fit::Unfit(UnfitReason::CapacityUnknown)
            ]
            .into_iter()
        ));
        assert!(!has_capacity_unknown(
            [
                Fit::Unfit(UnfitReason::TooBig),
                Fit::Unfit(UnfitReason::Policy)
            ]
            .into_iter()
        ));
        assert!(!has_capacity_unknown(std::iter::empty()));
    }

    /// E1: elevado, solo la limpieza REAL se niega; la simulación y la demostración siguen.
    #[test]
    fn elevado_solo_se_niega_la_limpieza_real() {
        use super::blocked_by_elevation;
        assert!(blocked_by_elevation(true, false, false));
        assert!(!blocked_by_elevation(true, true, false), "simulación");
        assert!(!blocked_by_elevation(true, false, true), "demostración");
        assert!(!blocked_by_elevation(false, false, false), "sin elevar");
    }

    /// M4: sin registro, una ejecución REAL se aborta; la simulación y la demostración siguen.
    #[test]
    fn sin_registro_solo_la_simulacion_y_la_demostracion_siguen() {
        assert!(abort_without_log(false, false, false));
        assert!(!abort_without_log(false, true, false), "simulación");
        assert!(!abort_without_log(false, false, true), "demostración");
        assert!(!abort_without_log(true, false, false), "con registro");
        assert!(!abort_without_log(true, true, true));
    }
}

#[cfg(test)]
mod summary_tests {
    //! m-5: las líneas del resumen que dependen de contadores del informe (el doble conteo de "no movidos", los sustituidos al
    //! vaciar y lo que explican los demás contadores de la Papelera).
    use super::{Summary, VolumeCtx, summary_lines};
    use crate::app::{Action, Env, Settings};
    use crate::args;
    use crate::i18n::{Lang, Tr};
    use crate::icons::IconCache;
    use crate::theme::Palette;
    use tesela_cleaner::Report;
    use tesela_cleaner::exec::BinReport;

    fn lines(lang: Lang, report: Report) -> (Tr, Vec<(String, bool)>) {
        let tr = Tr::new(lang);
        let mut icons = IconCache::default();
        let backend: std::sync::Arc<dyn tesela_cleaner::Backend> = std::sync::Arc::new(
            crate::demo_backend::DemoBackend::new(std::time::Duration::ZERO),
        );
        let mut actions: Vec<Action> = Vec::new();
        let env = Env {
            tr: &tr,
            pal: Palette::of(false),
            icons: &mut icons,
            demo: false,
            animate: false,
            drives: &[],
            drives_loading: false,
            settings: Settings::default(),
            backend: &backend,
            dry_run: false,
            history: &[],
            modal_open: false,
            actions: &mut actions,
        };
        let s = Summary {
            report,
            vol: VolumeCtx::default(),
            blocked: Vec::new(),
        };
        let v = summary_lines(&env, &s);
        (tr, v)
    }

    fn text(tr: &Tr, key: &str, n: u64) -> String {
        tr.ta(key, args!["n" => n, "count" => tr.int(n)])
    }

    fn has(v: &[(String, bool)], t: &str) -> bool {
        v.iter().any(|(x, _)| x == t)
    }

    /// T1: ningún texto promete lo que no se puede cumplir: ni un mínimo garantizado ("al menos"), ni ausencia de riesgo, ni
    /// "liberado ahora" a secas (es el cambio medido del espacio libre).
    #[test]
    fn ningun_texto_promete_un_minimo_ni_ausencia_de_riesgo() {
        let es = include_str!("../i18n/es.ftl");
        let en = include_str!("../i18n/en.ftl");
        for (name, text, banned) in [
            (
                "es",
                es,
                &[
                    "sin riesgo",
                    "sin miedo",
                    "al menos { $size }",
                    "Liberados ahora",
                ][..],
            ),
            (
                "en",
                en,
                &["with no risk", "without risk", "free at least", "Freed now"][..],
            ),
        ] {
            for b in banned {
                assert!(!text.contains(b), "{name}: no debe decir `{b}`");
            }
        }
        // R-3: los botones de la confirmación tampoco prometen la cifra exacta: es una estimación ("hasta").
        for (key, es_txt, en_txt) in [
            (
                "confirm-go-free",
                "Liberar hasta { $free }",
                "Free up to { $free }",
            ),
            (
                "confirm-go-both",
                "Liberar hasta { $free } y mover",
                "Free up to { $free } and move",
            ),
        ] {
            assert!(es.contains(&format!("{key} = {es_txt}")), "{key}");
            assert!(en.contains(&format!("{key} = {en_txt}")), "{key}");
        }
        // Y lo que sí dicen: un tope y que se comprueba al limpiar; el cambio medido con su aviso.
        for key in ["scan-known-sum", "recs-title-safe"] {
            assert!(es.contains(&format!("{key} = Hasta {{ $size }}")), "{key}");
            assert!(en.contains(&format!("{key} = Up to {{ $size }}")), "{key}");
        }
        assert!(es.contains("sum-freed = Cambio medido del espacio libre"));
        assert!(en.contains("sum-freed = Measured change in free space"));
        assert!(es.contains("sum-freed-hint =") && en.contains("sum-freed-hint ="));
    }

    /// A2-m1: lo que no se pudo comprobar se dice como tal ("no se pudo comprobar") y nunca como una pérdida confirmada ("se
    /// eliminó"): un elemento `Unverified` puede seguir entero en su sitio.
    #[test]
    fn los_textos_de_lo_no_verificado_dicen_que_no_se_pudo_comprobar_y_no_que_se_elimino() {
        let block = |text: &'static str, key: &str| -> String {
            let start = text
                .find(&format!("\n{key} ="))
                .unwrap_or_else(|| panic!("falta la clave {key}"))
                + 1;
            text[start..]
                .lines()
                .enumerate()
                .take_while(|(i, l)| *i == 0 || l.starts_with(' '))
                .map(|(_, l)| l)
                .collect::<Vec<_>>()
                .join("\n")
        };
        for key in ["sum-unverified", "sum-recycle-stopped-unverified"] {
            let es = block(include_str!("../i18n/es.ftl"), key);
            let en = block(include_str!("../i18n/en.ftl"), key);
            assert!(
                es.to_lowercase().contains("no se pudo comprobar"),
                "{key}: {es}"
            );
            assert!(
                !es.contains("elimin"),
                "{key}: no debe decir que se eliminó: {es}"
            );
            assert!(en.to_lowercase().contains("could not be"), "{key}: {en}");
            assert!(!en.to_lowercase().contains("delete"), "{key}: {en}");
        }
        // Y la línea del contador y la del corte dicen lo mismo, sin afirmar el borrado de un directorio cuyo contenido solo
        // cambió (H2): "se eliminó ... o su contenido cambió o perdió una parte", y mandan al registro y a la Papelera.
        for key in ["sum-unexpected", "sum-recycle-stopped"] {
            let es = block(include_str!("../i18n/es.ftl"), key);
            let en = block(include_str!("../i18n/en.ftl"), key);
            assert!(
                es.contains("eliminó") || es.contains("eliminaron"),
                "{key}: {es}"
            );
            assert!(es.contains("cambió"), "{key}: {es}");
            assert!(
                en.contains("deleted permanently") || en.contains("permanently deleted"),
                "{key}: {en}"
            );
            assert!(en.contains("changed"), "{key}: {en}");
        }
        let es = block(include_str!("../i18n/es.ftl"), "sum-unexpected");
        let en = block(include_str!("../i18n/en.ftl"), "sum-unexpected");
        assert!(es.contains("registro") && es.contains("Papelera"), "{es}");
        assert!(en.contains("log") && en.contains("Recycle Bin"), "{en}");
        // Y el texto no promete el borrado en seco: nunca una frase como "se eliminó definitivamente (entero".
        assert!(!es.contains("definitivamente (entero"), "{es}");
    }

    /// E3: la nota de `hiberfil.sys` dice lo que hay que saber antes de tocarlo: que hace falta un símbolo del sistema como
    /// administrador, que desactivar la hibernación quita también la suspensión híbrida y el inicio rápido, la alternativa
    /// menos drástica (`/type reduced`) y que Tesela no ejecuta nada (es/en en paridad).
    #[test]
    fn la_nota_de_hiberfil_dice_lo_necesario_antes_de_tocarlo() {
        let line = |text: &'static str| {
            text.lines()
                .find(|l| l.starts_with("info-hiberfil ="))
                .expect("la clave existe")
        };
        let es = line(include_str!("../i18n/es.ftl"));
        let en = line(include_str!("../i18n/en.ftl"));
        for needle in [
            "como administrador",
            "powercfg /h /type reduced",
            "powercfg /h off",
            "suspensión híbrida",
            "inicio rápido",
            "Tesela no ejecuta nada",
        ] {
            assert!(es.contains(needle), "es: falta `{needle}`");
        }
        for needle in [
            "as administrator",
            "powercfg /h /type reduced",
            "powercfg /h off",
            "hybrid sleep",
            "fast startup",
            "Tesela runs nothing",
        ] {
            assert!(en.contains(needle), "en: falta `{needle}`");
        }
    }

    #[test]
    fn los_no_movidos_no_cuentan_dos_veces_los_que_no_tuvieron_sitio_ni_los_del_corte() {
        for lang in [Lang::Es, Lang::En] {
            // 5 sin mover: 1 sin sitio, 1 del corte de seguridad y 3 de Windows (en uso, sin permiso...).
            let (tr, v) = lines(
                lang,
                Report {
                    not_recycled: 5,
                    no_room: 1,
                    stopped_items: 1,
                    ..Report::default()
                },
            );
            assert!(has(&v, &text(&tr, "sum-not-recycled", 3)), "{v:?}");
            assert!(!has(&v, &text(&tr, "sum-not-recycled", 5)), "{v:?}");
            assert!(has(&v, &text(&tr, "sum-no-room", 1)), "{v:?}");
            // Solo sin sitio y del corte: no hay línea de "no movidos".
            let (tr, v) = lines(
                lang,
                Report {
                    not_recycled: 3,
                    no_room: 2,
                    stopped_items: 1,
                    ..Report::default()
                },
            );
            for n in 1..=3 {
                assert!(!has(&v, &text(&tr, "sum-not-recycled", n)), "{n}: {v:?}");
            }
            // Los contadores nunca se salen de rango (resta saturada).
            let (tr, v) = lines(
                lang,
                Report {
                    not_recycled: 1,
                    no_room: 2,
                    stopped_items: 2,
                    ..Report::default()
                },
            );
            assert!(!has(&v, &text(&tr, "sum-not-recycled", 1)), "{v:?}");
        }
    }

    #[test]
    fn los_sustituidos_al_vaciar_tienen_su_linea_y_explican_lo_que_queda() {
        for lang in [Lang::Es, Lang::En] {
            let bin = |still_there, changed, unverifiable, replaced| Report {
                bin: Some(BinReport {
                    requested: 10,
                    still_there,
                    changed,
                    unverifiable,
                    replaced,
                    ..BinReport::default()
                }),
                ..Report::default()
            };
            // 6 siguen: 1 cambiado, 1 sin verificar y 2 sustituidos explican 4; quedan 2 "no se pudo vaciar".
            let (tr, v) = lines(lang, bin(6, 1, 1, 2));
            assert!(has(&v, &text(&tr, "sum-bin-replaced", 2)), "{v:?}");
            assert!(has(&v, &text(&tr, "sum-bin-changed", 1)), "{v:?}");
            assert!(has(&v, &text(&tr, "sum-bin-unverifiable", 1)), "{v:?}");
            assert!(has(&v, &text(&tr, "sum-bin-failed", 2)), "{v:?}");
            // Si lo explican todo, no hay línea de "no se pudo vaciar del todo".
            let (tr, v) = lines(lang, bin(4, 1, 1, 2));
            assert!(has(&v, &text(&tr, "sum-bin-replaced", 2)), "{v:?}");
            for n in 1..=4 {
                assert!(!has(&v, &text(&tr, "sum-bin-failed", n)), "{n}: {v:?}");
            }
            // Sin sustituidos no hay línea de sustituidos.
            let (tr, v) = lines(lang, bin(0, 0, 0, 0));
            assert!(!has(&v, &text(&tr, "sum-bin-replaced", 1)), "{v:?}");
        }
    }
}
