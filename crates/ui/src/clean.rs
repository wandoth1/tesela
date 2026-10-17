//! The interface cleanup flow: basket -> approval -> confirmation -> execution -> summary (PLAN.md §2.5).
//!
//! It is a separate component of scanning: it is used by the scanning screen ("Wipe safe") and the
//! major. Everything that touches the disk goes through [`Backend`] (the real `cleaner`, or the simulated `--demo`), in threads
//! own; the interface only reads atomic and channels. NOTHING is deleted without going through confirmation, which shows the total and
//! destiny of each thing.

use crate::app::Env;
use crate::args;
use crate::icons::Icon;
use crate::theme::{self, Palette};
use crate::widgets::{self, ButtonStyle};
use crossbeam_channel::{Receiver, TryRecvError};
use egui::{Align, CornerRadius, Layout, Margin, RichText, Stroke, Ui, vec2};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tesela_cleaner::log::{HistoryEntry, history_append};
use tesela_cleaner::{
    ApproveOptions, Backend, Basket, Browser, CategoryId, ExecError, ExecOptions, Fit, LogWriter,
    Phase, Plan, PlanItem, Progress, RejectReason, Removed, Report, StopReason, UnfitReason,
    Verdict,
};

/// Context of the volume being cleaned (for history and summary before/now bars).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct VolumeCtx {
    /// Drive letter (if an entire drive was scanned).
    pub drive: Option<char>,
    /// (capacity, free) of the volume at the end of the analysis, with volume scope.
    pub volume: Option<(u64, u64)>,
}

/// Flow events that the owner screen has to attend to.
#[derive(Debug)]
pub(crate) enum FlowEvent {
    /// What really disappeared: the scan updates the tree with this (overlay), without rescanning.
    Removed(Vec<Removed>),
}

struct Confirm {
    plan: Arc<Plan>,
    /// The amber group box ("will be permanently deleted because it doesn't fit").
    allow_unfit: bool,
    allow_mail_permanent: bool,
    /// "See the full list."
    show_all: bool,
    /// It is confirmed again because something changed.
    reconfirm: bool,
    focused: bool,
}

struct Running {
    progress: Arc<Progress>,
    rx: Receiver<Result<Completion, ExecError>>,
    dry: bool,
    cancelled: bool,
    started: Instant,
    /// Detect lack of progress only within the current execution phase.
    last_entries: u64,
    last_phase: Phase,
    last_advance: Instant,
    /// The user tried to close the window while it was running: told to wait or cancel.
    close_blocked: bool,
}

struct Completion {
    report: Report,
    blocked: Vec<(Browser, u64)>,
}

/// Filesystem and process work runs only on execution workers. Applying this result is pure UI work.
fn complete(
    backend: &Arc<dyn Backend>,
    report: Report,
    dry: bool,
    vol: VolumeCtx,
    used_before: Option<u64>,
    hints: &[(Browser, u64)],
) -> Completion {
    if !dry && let Some(dir) = backend.data_dir() {
        let e = HistoryEntry {
            unix: unix_now(),
            drive: vol.drive.map_or(0, |c| c as u8),
            used: used_before.unwrap_or(0),
            freed_now: report.freed_now.unwrap_or(0).max(0) as u64,
            to_bin: report.recycled_bytes,
        };
        let _ = history_append(&dir, &e);
    }
    let open = backend.running_browsers();
    let blocked = hints
        .iter()
        .filter(|(b, bytes)| open.contains(b) && *bytes > 0)
        .copied()
        .collect();
    Completion { report, blocked }
}

struct Summary {
    report: Report,
    vol: VolumeCtx,
    /// Open browsers with an uncleaned cache and how much would be freed by closing them.
    blocked: Vec<(Browser, u64)>,
}

enum Stage {
    Idle,
    Approving {
        rx: Receiver<Plan>,
        cancel: Arc<AtomicBool>,
        reconfirm: bool,
        progress: Arc<AtomicU64>,
    },
    Confirm(Box<Confirm>),
    Running(Box<Running>),
    Summary(Box<Summary>),
}

/// Brief notice (toast) pending painting.
pub(crate) enum Note {
    NothingToClean,
    /// There is nothing left to offer because everything was modified less than the minimum age (7 days) ago.
    NothingYoung,
    /// Failed to create local registry: no registry is not cleaned (M4). Nothing has been touched.
    NoLog,
    /// What fits in the Recycle Bin changed since confirmation: review and confirm again.
    Reconfirm,
    /// The application is run as administrator: the actual cleanup is not done (E1). Nothing has been touched.
    Elevated,
}

impl Drop for CleanFlow {
    /// If the screen is closed while the basket is approved or executed, it is canceled (between entries: what has already started
    /// ends well and nothing else is touched).
    fn drop(&mut self) {
        match &self.stage {
            Stage::Approving { cancel, .. } => cancel.store(true, Ordering::SeqCst),
            Stage::Running(r) => r.progress.cancel(),
            _ => {}
        }
    }
}

/// The cleaning flow.
pub(crate) struct CleanFlow {
    pub basket: Basket,
    stage: Stage,
    events: Vec<FlowEvent>,
    note: Option<Note>,
    /// Context of the cleaned volume (remembers `start` to `finish`).
    vol: VolumeCtx,
    /// Request to "Recheck" from the summary (the scan recalculates the recommendations).
    pub recheck: bool,
    /// Caches of open browsers that were NOT cleaned and how much would have to be freed if they were closed (the screen says it
    /// owner based on recommendations; the summary shows it as "Close X to free up to N more").
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

/// The notice of an empty plan, if it is: "nothing to clean", or, if EVERYTHING rejected is "too recent" (a plan without
/// elements and only with rejections `Young`), the notice that explains it. An empty plan with other rejections is taught (with its reason).
pub(crate) fn empty_plan_note(plan: &Plan) -> Option<Note> {
    if !plan.is_empty() {
        return None;
    }
    if plan.rejected.is_empty() {
        return Some(Note::NothingToClean);
    }
    plan.rejected
        .iter()
        .all(|r| r.reason == RejectReason::Young)
        .then_some(Note::NothingYoung)
}

/// How many items would actually be touched: the planned items other than those that "stay where they are" (`kept`), plus Recycle Bin contents if
/// empty. The confirmation counts on this and disables the button if it is zero.
pub(crate) fn actionable_count(items: usize, kept: usize, has_bin: bool) -> usize {
    items.saturating_sub(kept) + usize::from(has_bin)
}

/// How much space is actually freed (permanent deletions + emptied Recycle Bin + items that do not fit, if confirmed)?
pub(crate) fn free_bytes(plan: &Plan, allow_unfit: bool) -> u64 {
    plan.permanent_bytes()
        + plan.bin.as_ref().map_or(0, |b| b.bytes_hint)
        + if allow_unfit { plan.unfit_bytes() } else { 0 }
}

impl CleanFlow {
    /// Is there a modal window open (commit or execute)? The scan keys do not work.
    pub fn modal_open(&self) -> bool {
        matches!(
            self.stage,
            Stage::Approving { .. } | Stage::Confirm(_) | Stage::Running(_)
        )
    }

    /// Is the summary showing (takes up the entire window)?
    pub fn in_summary(&self) -> bool {
        matches!(self.stage, Stage::Summary(_))
    }

    /// Is cleanup running (deleting items or sending them to the Recycle Bin)? Closing the window then would leave it halfway done.
    pub fn is_running(&self) -> bool {
        matches!(self.stage, Stage::Running(_))
    }

    /// Note that an attempt was made to close the window during execution (the progress window explains this).
    pub fn block_close(&mut self) {
        if let Stage::Running(r) = &mut self.stage {
            r.close_blocked = true;
        }
    }

    /// Is there anything going on (approving, committing, or executing)?
    pub fn busy(&self) -> bool {
        !matches!(self.stage, Stage::Idle | Stage::Summary(_))
    }

    /// Closes the summary (and anything else in progress that is not running).
    pub fn close_summary(&mut self) {
        if matches!(self.stage, Stage::Summary(_)) {
            self.stage = Stage::Idle;
        }
    }

    /// Pending events for the owner screen.
    pub fn take_events(&mut self) -> Vec<FlowEvent> {
        std::mem::take(&mut self.events)
    }

    /// For captures and tests with SIMULATED support: approve the basket instantly and open the confirmation.
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
            allow_mail_permanent: false,
            show_all: false,
            reconfirm: false,
            focused: true,
        }));
    }

    /// For captures and tests with SIMULATED backup: approve and execute instantly and leave the summary.
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
            allow_local_mail_permanent: false,
            log: None,
        };
        match backend.execute(&plan, &opts, &Progress::new()) {
            Ok(report) => {
                let completion = complete(
                    backend,
                    report,
                    dry,
                    vol,
                    vol.volume.map(|(t, f)| t.saturating_sub(f)),
                    &self.blocked_hint,
                );
                self.finish(completion, dry);
            }
            // The error is not swallowed: it is displayed with the same warning as in the normal flow (H6.7).
            Err(ExecError::LogRequired) => self.note = Some(Note::NoLog),
            Err(ExecError::NeedsReconfirm(_)) => self.note = Some(Note::Reconfirm),
            Err(ExecError::Elevated) => self.note = Some(Note::Elevated),
        }
    }

    /// Cart approval begins (in a thread); When finished, the confirmation opens.
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
        let progress = Arc::new(AtomicU64::new(0));
        let count = progress.clone();
        std::thread::spawn(move || {
            let opts = ApproveOptions {
                progress: Some(count),
                ..ApproveOptions::default()
            };
            let plan = b.approve(&basket, &opts, &c);
            let _ = tx.send(plan);
            ctx2.request_repaint();
        });
        self.stage = Stage::Approving {
            rx,
            cancel,
            reconfirm,
            progress,
        };
    }

    /// Advances the flow (collects results from threads). Returns the notice to teach, if there is one.
    pub fn poll(&mut self, ctx: &egui::Context, backend: &Arc<dyn Backend>, dry: bool) {
        match &mut self.stage {
            Stage::Approving {
                rx,
                reconfirm,
                cancel,
                ..
            } => match rx.try_recv() {
                Ok(plan) => {
                    let reconfirm = *reconfirm;
                    if cancel.load(Ordering::Relaxed) {
                        self.stage = Stage::Idle;
                    } else if let Some(note) = empty_plan_note(&plan) {
                        self.note = Some(note);
                        self.stage = Stage::Idle;
                    } else {
                        self.stage = Stage::Confirm(Box::new(Confirm {
                            plan: Arc::new(plan),
                            allow_unfit: false,
                            allow_mail_permanent: false,
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
                Ok(Ok(completion)) => {
                    let dry_run = run.dry;
                    let _ = dry;
                    self.finish(completion, dry_run);
                }
                Ok(Err(ExecError::LogRequired)) => {
                    // The executor requires a usable log: nothing has been touched.
                    self.stage = Stage::Idle;
                    self.note = Some(Note::NoLog);
                }
                Ok(Err(ExecError::Elevated)) => {
                    // High: nothing has been touched (the `cleaner` rejects it before starting).
                    self.stage = Stage::Idle;
                    self.note = Some(Note::Elevated);
                }
                Ok(Err(ExecError::NeedsReconfirm(_))) => {
                    // What fits in the Recycle Bin has changed since confirmation: review and confirm again.
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

    fn finish(&mut self, completion: Completion, dry: bool) {
        let Completion { report, blocked } = completion;
        if !dry {
            let removed: Vec<Removed> = report
                .items
                .iter()
                .flat_map(|i| i.removed.clone())
                .collect();
            if !removed.is_empty() {
                self.events.push(FlowEvent::Removed(removed));
            }
            self.basket.clear();
        }
        self.stage = Stage::Summary(Box::new(Summary {
            report,
            vol: self.vol,
            blocked,
        }));
    }

    /// Set Running immediately; preparation (including mandatory durable log creation) happens on the worker.
    fn begin_execution<F>(
        &mut self,
        ctx: &egui::Context,
        backend: Arc<dyn Backend>,
        plan: Arc<Plan>,
        dry: bool,
        used_before: Option<u64>,
        prepare: F,
    ) where
        F: FnOnce() -> Result<ExecOptions, ExecError> + Send + 'static,
    {
        let progress = Progress::new();
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.stage = Stage::Running(Box::new(Running {
            progress: progress.clone(),
            rx,
            dry,
            cancelled: false,
            started: Instant::now(),
            last_entries: 0,
            last_phase: Phase::Preparing,
            last_advance: Instant::now(),
            close_blocked: false,
        }));
        let (ctx2, vol, hints) = (ctx.clone(), self.vol, self.blocked_hint.clone());
        std::thread::spawn(move || {
            let result = prepare()
                .and_then(|opts| backend.execute(&plan, &opts, &progress))
                .map(|report| complete(&backend, report, dry, vol, used_before, &hints));
            let _ = tx.send(result);
            ctx2.request_repaint();
        });
        ctx.request_repaint();
    }

    // ---------------------------------------------------------------------------------------------------------------
    // Modal windows: confirmation and execution
    // ---------------------------------------------------------------------------------------------------------------

    /// Paint confirmation or progress, if you touch, and attend to its buttons.
    pub fn show_modals(
        &mut self,
        ctx: &egui::Context,
        env: &mut Env,
        vol: VolumeCtx,
        used_before: Option<u64>,
    ) {
        let backend = env.backend.clone();
        let mut go: Option<(bool, bool)> = None; // (dry_run, allow_unfit)
        let mut cancel_confirm = false;
        let mut cancel_run = false;
        let mut cancel_approval = false;
        match &mut self.stage {
            Stage::Approving {
                cancel, progress, ..
            } => {
                approving_ui(ctx, env, progress, &mut cancel_approval);
                if cancel_approval {
                    cancel.store(true, Ordering::SeqCst);
                }
            }
            Stage::Confirm(c) => confirm_ui(ctx, env, c, &mut go, &mut cancel_confirm),
            Stage::Running(r) => running_ui(ctx, env, r, &mut cancel_run),
            _ => {}
        }
        if let Some(note) = &self.note {
            let (key, secs) = match note {
                Note::NothingToClean => ("confirm-empty", 4.0),
                Note::NothingYoung => ("confirm-empty-young", 8.0),
                // Longer: explains why nothing happened.
                Note::NoLog => ("confirm-no-log", 12.0),
                Note::Reconfirm => ("confirm-reconfirm", 8.0),
                // Long: tells what to do.
                Note::Elevated => ("confirm-elevated", 12.0),
            };
            let msg = env.tr.t(key);
            toast(ctx, &msg);
            // The warning lasts a few seconds.
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
            let allow_local_mail_permanent = mail_consent_for_execution(c, allow_unfit);
            // E1: elevated processes cannot clean (the `cleaner` also rejects them); this is reported before creating the log.
            if blocked_by_elevation(backend.is_elevated(), dry, backend.is_demo()) {
                self.note = Some(Note::Elevated);
                ctx.request_repaint();
                return;
            }
            self.vol = vol;
            let b = backend.clone();
            self.begin_execution(ctx, backend, plan, dry, used_before, move || {
                let log = if b.is_demo() {
                    None
                } else {
                    b.data_dir()
                        .and_then(|d| LogWriter::create(&d, dry).ok())
                        .map(Arc::new)
                };
                if abort_without_log(log.is_some(), dry, b.is_demo()) {
                    return Err(ExecError::LogRequired);
                }
                Ok(ExecOptions {
                    dry_run: dry,
                    allow_unfit_permanent: allow_unfit,
                    allow_local_mail_permanent,
                    log,
                })
            });
        }
    }
}

/// Should cleanup stop because no log could be created (M4)? Without a log, real cleanup is blocked. Dry-run mode and demo mode
/// can continue because neither performs real cleanup or writes to the log.
fn abort_without_log(has_log: bool, dry: bool, is_demo: bool) -> bool {
    !has_log && !dry && !is_demo
}

/// Should elevated privileges block cleanup (E1)? Only real cleanup is blocked. Dry-run and demo modes can continue because
/// they do not perform real cleanup, and scanning does not pass through this check.
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

/// Display name of a plan item.
fn item_name(env: &Env, it: &PlanItem) -> String {
    match it.category {
        Some(CategoryId::UserTemp) => env.tr.t("known-user-temp"),
        Some(CategoryId::WindowsTemp) => env.tr.t("known-windows-temp"),
        Some(CategoryId::ChromeCache) => env.tr.t("known-chrome-cache"),
        Some(CategoryId::EdgeCache) => env.tr.t("known-edge-cache"),
        Some(CategoryId::FirefoxCache) => env.tr.t("known-firefox-cache"),
        Some(CategoryId::Downloads) => env.tr.t("known-downloads"),
        Some(CategoryId::RecycleBin) => env.tr.t("known-recycle-bin"),
        // Each item is a report folder or dump: it is displayed with its own name and the category is labeled.
        _ => it.path.file_name().map_or_else(
            || it.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
    }
}

/// Does an item fail to fit because the drive has no configured Recycle Bin size limit?
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

/// `hover`: the full path, displayed when you mouse over the name (distinguishes elements with the same name).
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

fn can_execute(plan: &Plan, allow_unfit: bool, allow_mail_permanent: bool) -> bool {
    let actionable = plan.permanent_items().next().is_some()
        || plan.recycle_items().next().is_some()
        || plan.bin.as_ref().is_some_and(|b| !b.items.is_empty())
        || (allow_unfit && plan.unfit_items().next().is_some());
    actionable && (plan.permanent_mail_files(allow_unfit) == 0 || allow_mail_permanent)
}

/// Consent is valid only while the confirmation still offers permanent deletion of local mail.
/// A previously checked box must not authorize a PST that appears later after the amber group is deselected.
fn mail_consent_for_execution(c: &Confirm, allow_unfit: bool) -> bool {
    c.allow_mail_permanent && c.plan.permanent_mail_files(allow_unfit) > 0
}

/// Main button label depending on what there is.
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
                let kept = plan.kept_unfit_items().count();
                let actionable = actionable_count(plan.items.len(), kept, plan.bin.is_some());
                let n_items = actionable as u64;
                let total = free_bytes(&plan, true) + plan.recycle_bytes();
                ui.label(
                    RichText::new(env.tr.ta(
                        "confirm-subtitle",
                        args!["n" => n_items, "count" => env.tr.int(n_items), "size" => env.tr.size(total)],
                    ))
                    .color(pal.text_muted),
                );
                // When only never-permanent diagnostics remain, explain why no cleanup is available.
                if actionable == 0 && kept > 0 {
                    ui.add_space(6.0);
                    note_line(ui, &pal, &env.tr.t("confirm-only-kept"), true);
                }
                if !c.allow_unfit && plan.unfit_items().next().is_some() {
                    note_line(ui, &pal, &env.tr.t("confirm-unfit-pending"), true);
                }
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
        // Footer: link, cancel and the main button.
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
                        let label = go_label(env, &plan, c.allow_unfit, dry);
                        let enabled = can_execute(&plan, c.allow_unfit, c.allow_mail_permanent);
                        let r = widgets::button(ui, env, ButtonStyle::Primary, Some(Icon::ShieldCheckmark), &label, enabled);
                        if r.clicked() {
                            *go = Some((dry, c.allow_unfit && unfit));
                        }
                        let cancel_label = env.tr.t("cancel");
                        let rc = widgets::button(ui, env, ButtonStyle::Secondary, None, &cancel_label, true);
                        if !c.focused {
                            // Focus starts on "Cancel": pressing Enter alone never deletes anything.
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
    // 1. It deletes and recreates itself.
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
    // 2. Empty the Recycle Bin (scope listed).
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
    // 3. Goes to the Recycle Bin.
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
        // "They are your files" only if there are your files: reports and dumps have their own note.
        let is_diag = |i: &&PlanItem| i.category.is_some_and(CategoryId::is_diagnostic);
        if rec.iter().any(|i| !is_diag(i)) {
            note_line(ui, &pal, &env.tr.t("confirm-recycle-note"), false);
        }
        if rec.iter().any(is_diag) {
            note_line(ui, &pal, &env.tr.t("confirm-diag-note"), false);
        }
        // m-1: one Recycle Bin operation per folder; with many, it takes time (can be cancelled).
        if let Some(n) = plan.many_diagnostic_items() {
            note_line(
                ui,
                &pal,
                &env.tr.ta(
                    "confirm-diag-many",
                    args!["n" => n as u64, "count" => env.tr.int(n as u64)],
                ),
                true,
            );
        }
        ui.add_space(16.0);
    }
    // 4. What does not fit in the Recycle Bin: the amber group with its checkbox.
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
                // No maximum size set, how to fix it (H6.3).
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
    // 4b. What does not fit and is NEVER permanently deleted (reports, dumps): it is left where it is, without a box.
    let kept: Vec<&PlanItem> = plan.kept_unfit_items().collect();
    if !kept.is_empty() {
        let sum: u64 = kept.iter().map(|i| i.bytes()).sum();
        group_header(
            ui,
            env,
            Icon::Warning,
            &env.tr.t("confirm-group-kept"),
            &env.tr.size(sum),
        );
        ui.add_space(4.0);
        let shown = if c.show_all {
            kept.len()
        } else {
            kept.len().min(5)
        };
        for it in &kept[..shown] {
            let why = match it.fit {
                Fit::Unfit(r) => env.tr.t(unfit_reason_key(r)),
                Fit::Fits => String::new(),
            };
            let chip = it.category.map(|id| crate::basket::category_name(env, id));
            row(
                ui,
                &pal,
                &item_name(env, it),
                chip.as_deref(),
                &env.tr.size(it.bytes()),
                Some(&why),
                Some(&it.path.display().to_string()),
            );
        }
        if shown < kept.len() {
            let more = (kept.len() - shown) as u64;
            ui.label(
                RichText::new(env.tr.ta(
                    "confirm-more",
                    args!["n" => more, "count" => env.tr.int(more)],
                ))
                .size(13.0)
                .color(pal.text_muted),
            );
        }
        if has_capacity_unknown(kept.iter().map(|i| i.fit)) {
            note_line(ui, &pal, &env.tr.t("confirm-unfit-capacity-hint"), false);
        }
        note_line(ui, &pal, &env.tr.t("confirm-kept-note"), true);
        ui.add_space(16.0);
    }
    // 5. Mail warnings do not block owner-selected files; permanent PST deletion needs a second explicit consent.
    let ((local_n, local_b), (cache_n, cache_b)) = plan.mail_totals();
    if cache_n > 0 {
        note_line(
            ui,
            &pal,
            &env.tr.ta(
                "confirm-mail-cache",
                args!["count" => env.tr.int(cache_n), "size" => env.tr.size(cache_b)],
            ),
            true,
        );
    }
    if local_n > 0 {
        ui.add(
            egui::Label::new(
                RichText::new(env.tr.ta(
                    "confirm-mail-local",
                    args!["count" => env.tr.int(local_n), "size" => env.tr.size(local_b)],
                ))
                .color(pal.danger),
            )
            .wrap(),
        );
    }
    if local_n + cache_n > 0 && plan.outlook_open {
        note_line(ui, &pal, &env.tr.t("confirm-outlook-open"), true);
    }
    if plan.permanent_mail_files(c.allow_unfit) > 0 {
        ui.checkbox(&mut c.allow_mail_permanent, env.tr.t("confirm-mail-check"));
    } else {
        c.allow_mail_permanent = false;
    }
    // Other warnings: hard links, cloud, protected and rejected entries.
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
    // m-2: what is left as recent has its own notice (it is not "changed, does not exist or is protected").
    let young = plan.young_count() as u64;
    if young > 0 {
        note_line(
            ui,
            &pal,
            &env.tr.ta(
                "confirm-young",
                args!["n" => young, "count" => env.tr.int(young)],
            ),
            false,
        );
    }
    let n = plan.rejected.len() as u64 - young;
    if n > 0 {
        note_line(
            ui,
            &pal,
            &env.tr.ta(
                "confirm-rejected",
                args!["n" => n, "count" => env.tr.int(n)],
            ),
            true,
        );
    }
    if !plan.rejected.is_empty() && c.show_all {
        let others = plan
            .rejected
            .iter()
            .filter(|r| r.reason != RejectReason::Young);
        let youngs = plan
            .rejected
            .iter()
            .filter(|r| r.reason == RejectReason::Young);
        for r in others.chain(youngs).take(30) {
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
                RejectReason::Young => env.tr.t("reject-young"),
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

fn recycle_row(ui: &mut Ui, env: &mut Env, it: &PlanItem) {
    let pal = env.pal;
    let name = item_name(env, it);
    // "Your file", or the diagnostic category (bug reports, dumps) to which the item belongs.
    let chip = match it.category {
        None => Some(env.tr.t("confirm-your-file")),
        Some(id) if id.is_diagnostic() => Some(crate::basket::category_name(env, id)),
        Some(_) => None,
    };
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

/// For user files and folders ("Your file"): "in <parent folder>" and the full path (for the mouse). The
/// Ruled categories are called by their known name and do not need it; reports and dumps (each item is a
/// folder or a specific file) also carry its folder and the full path.
fn folder_hint(env: &Env, it: &PlanItem) -> (Option<String>, Option<String>) {
    if it.category.is_some_and(|c| !c.is_diagnostic()) {
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

fn approving_ui(ctx: &egui::Context, env: &mut Env, progress: &AtomicU64, cancel: &mut bool) {
    egui::Modal::new(egui::Id::new("tesela-approving")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.horizontal(|ui| {
            widgets::busy(ui, env.animate, 18.0, env.pal.accent);
            ui.label(env.tr.t("approve-title"));
        });
        let n = progress.load(Ordering::Relaxed);
        ui.label(
            env.tr
                .ta("approve-progress", args!["count" => env.tr.int(n)]),
        );
        if widgets::button(
            ui,
            env,
            ButtonStyle::Secondary,
            None,
            &env.tr.t("cancel"),
            true,
        )
        .clicked()
        {
            *cancel = true;
        }
    });
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
        let current_phase = r.progress.phase();
        if entries != r.last_entries || current_phase != r.last_phase {
            r.last_entries = entries; r.last_phase = current_phase; r.last_advance = Instant::now();
        }
        if !r.dry && current_phase == Phase::Deleting && r.last_advance.elapsed().as_secs() >= 3 {
            note_line(ui, &pal, &env.tr.t("run-file-wait"), true);
        }
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

// ------------------------------------------------------------------------------------------------------------------
// Summary
// ------------------------------------------------------------------------------------------------------------------

/// What the user chose in the summary.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SummaryClick {
    Back,
    EmptyNow,
    OpenBin,
    Recheck,
}

impl CleanFlow {
    /// Paints the summary (takes up all the available space) and returns what was clicked.
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
    // The two things can happen at the same time and both are said: the cancellation of the user and the failure of the registration (which
    // stops cleaning alone).
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
    // `not_recycled` also counts those who did not have a place and those from the security cut (who already have their line): here
    // only the rest, the ones that Windows did not move and were proven to remain intact in their place.
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
        if b.with_links > 0 {
            v.push((n("sum-bin-with-links", b.with_links), true));
        }
        // They were not in the list (their `$I` could not be read): they do not explain anything that follows.
        if b.unlisted > 0 {
            v.push((n("sum-bin-unlisted", b.unlisted), true));
        }
        let explained = b.changed + b.unverifiable + b.replaced + b.with_links;
        if b.still_there > explained {
            v.push((n("sum-bin-failed", b.still_there - explained), true));
        }
    }
    // If "released now" and "deleted" diverge, say why on one line.
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
                    // "Released now" is the measured CHANGE of free space: other programs writing at the same time too
                    // they move it. The summary explains this, and deleted and Recycle Bin totals are counted separately.
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
                    // The three accountants.
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
                    // Then and now (with volume figures).
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
                    // Recycle Bin Card: "Empty now" is the main button if what is there exceeds what is released.
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
                                        // "Empty Now" empties the ENTIRE Recycle Bin (the list shown at confirmation), not just the items just sent there.
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
                    // Warning lines.
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
mod actionable_tests {
    use super::{Note, actionable_count, empty_plan_note};
    use std::path::PathBuf;
    use tesela_cleaner::{Plan, RejectReason, Rejected};

    /// m-12: an empty plan with only rejections "too recent" again gives the warning of "nothing to clean", with its text; with
    /// In other rejections, confirmation is shown so that the reason can be seen.
    #[test]
    fn un_plan_vacio_con_solo_recientes_da_el_aviso_de_que_no_hay_nada() {
        let rej = |reason| Rejected {
            path: PathBuf::from("x"),
            reason,
        };
        let mut plan = Plan::default();
        assert!(matches!(empty_plan_note(&plan), Some(Note::NothingToClean)));
        plan.rejected = vec![rej(RejectReason::Young), rej(RejectReason::Young)];
        assert!(matches!(empty_plan_note(&plan), Some(Note::NothingYoung)));
        plan.rejected.push(rej(RejectReason::Missing));
        assert!(empty_plan_note(&plan).is_none());
    }

    /// Items that "stay where they are" do not count; with nothing to do (no Recycle Bin to empty), the button is disabled.
    #[test]
    fn lo_que_se_queda_donde_esta_no_cuenta_como_elemento() {
        assert_eq!(actionable_count(5, 2, false), 3);
        assert_eq!(actionable_count(5, 0, false), 5);
        // Only elements that remain: nothing to do.
        assert_eq!(actionable_count(2, 2, false), 0);
        // Emptying the Recycle Bin is something to do.
        assert_eq!(actionable_count(2, 2, true), 1);
        assert_eq!(actionable_count(0, 0, false), 0);
        // Never below zero.
        assert_eq!(actionable_count(1, 3, false), 0);
    }
}

#[cfg(test)]
mod log_gate_tests {
    use super::{abort_without_log, has_capacity_unknown};
    use tesela_cleaner::{Fit, UnfitReason};

    /// H6.3: The "set a maximum size" hint only comes out if something does not fit by `CapacityUnknown`.
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

    /// E1: elevated, only REAL cleaning is denied; simulation and demonstration follow.
    #[test]
    fn elevado_solo_se_niega_la_limpieza_real() {
        use super::blocked_by_elevation;
        assert!(blocked_by_elevation(true, false, false));
        assert!(!blocked_by_elevation(true, true, false), "simulación");
        assert!(!blocked_by_elevation(true, false, true), "demostración");
        assert!(!blocked_by_elevation(false, false, false), "sin elevar");
    }

    /// M4: without registration, a REAL execution is aborted; simulation and demonstration follow.
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
    //! m-5: summary lines that depend on report counters (double counting of "not moved", those replaced at
    //! emptied and what the other Recycle Bin counters explain).
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
            update: Default::default(),
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

    /// T1: no text promises what cannot be fulfilled: neither a guaranteed minimum ("at least"), nor absence of risk, nor
    /// "released now" simply (it is the measured change in free space).
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
        // A-3: The confirmation buttons do not promise the exact figure either: it is an estimate ("up to").
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
        // And what they do say: a stop and that is checked when cleaning; the change measured with your notice.
        for key in ["scan-known-sum", "recs-title-safe"] {
            assert!(es.contains(&format!("{key} = Hasta {{ $size }}")), "{key}");
            assert!(en.contains(&format!("{key} = Up to {{ $size }}")), "{key}");
        }
        assert!(es.contains("sum-freed = Cambio medido del espacio libre"));
        assert!(en.contains("sum-freed = Measured change in free space"));
        assert!(es.contains("sum-freed-hint =") && en.contains("sum-freed-hint ="));
    }

    /// A2-m1: What could not be verified is said as such ("could not be verified") and never as a confirmed loss ("was
    /// deleted"): A `Unverified` element can remain intact in its place.
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
        // And the counter line and the cut line say the same thing, without stating the deletion of a directory whose content only
        // changed (H2): "it was deleted... or its content changed or a part was lost"; those items are recorded in the log and sent to the Recycle Bin.
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
        // And the text does not promise dry erase: never a phrase like "it was permanently deleted (entirely)."
        assert!(!es.contains("definitivamente (entero"), "{es}");
    }

    /// E3: `hiberfil.sys`'s note says what you need to know before touching it: that this requires an elevated command prompt; disabling hibernation also removes hybrid sleep and fast startup. A
    /// less drastic alternative is `/type reduced`, and Tesela does not run any command (the Spanish and English texts match).
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
            // 5 unmoved: 1 without room, 1 from the security cut and 3 from Windows (in use, without permission...).
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
            // Only without room and from the cut: there is no "not moved" line.
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
            // Counters never go out of range (saturated subtraction).
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
            // 6 follow: 1 changed, 1 unverified and 2 replaced explain 4; 2 "could not be emptied" left.
            let (tr, v) = lines(lang, bin(6, 1, 1, 2));
            assert!(has(&v, &text(&tr, "sum-bin-replaced", 2)), "{v:?}");
            assert!(has(&v, &text(&tr, "sum-bin-changed", 1)), "{v:?}");
            assert!(has(&v, &text(&tr, "sum-bin-unverifiable", 1)), "{v:?}");
            assert!(has(&v, &text(&tr, "sum-bin-failed", 2)), "{v:?}");
            // If they explain everything, there's no "couldn't completely empty it" line.
            let (tr, v) = lines(lang, bin(4, 1, 1, 2));
            assert!(has(&v, &text(&tr, "sum-bin-replaced", 2)), "{v:?}");
            for n in 1..=4 {
                assert!(!has(&v, &text(&tr, "sum-bin-failed", n)), "{n}: {v:?}");
            }
            // Without substitutes there is no line of substitutes.
            let (tr, v) = lines(lang, bin(0, 0, 0, 0));
            assert!(!has(&v, &text(&tr, "sum-bin-replaced", 1)), "{v:?}");
        }
    }

    #[test]
    fn los_elementos_con_enlaces_y_los_ilegibles_tienen_su_linea_al_vaciar() {
        for lang in [Lang::Es, Lang::En] {
            let r = |still_there, with_links, unlisted| Report {
                bin: Some(BinReport {
                    requested: 10,
                    still_there,
                    with_links,
                    unlisted,
                    ..BinReport::default()
                }),
                ..Report::default()
            };
            // 3 follow and all 3 have links: they explain everything, without "could not empty".
            let (tr, v) = lines(lang, r(3, 3, 0));
            assert!(has(&v, &text(&tr, "sum-bin-with-links", 3)), "{v:?}");
            for n in 1..=3 {
                assert!(!has(&v, &text(&tr, "sum-bin-failed", n)), "{n}: {v:?}");
            }
            // The illegible ones were not on the list: they have their line and do not explain what follows.
            let (tr, v) = lines(lang, r(2, 1, 4));
            assert!(has(&v, &text(&tr, "sum-bin-unlisted", 4)), "{v:?}");
            assert!(has(&v, &text(&tr, "sum-bin-with-links", 1)), "{v:?}");
            assert!(has(&v, &text(&tr, "sum-bin-failed", 1)), "{v:?}");
            // Without any, neither of the two lines.
            let (tr, v) = lines(lang, r(0, 0, 0));
            assert!(!has(&v, &text(&tr, "sum-bin-with-links", 1)), "{v:?}");
            assert!(!has(&v, &text(&tr, "sum-bin-unlisted", 1)), "{v:?}");
        }
    }
}

#[cfg(test)]
mod responsiveness_tests {
    use super::*;
    use std::time::Duration;
    use tesela_cleaner::engine::UnitSpec;
    use tesela_cleaner::{Destination, Risk, StatsSnapshot};

    fn mail_item(destination: Destination, fit: Fit) -> PlanItem {
        PlanItem {
            id: 0,
            category: None,
            path: "synthetic-mail/mail.pst".into(),
            risk: Risk::Review,
            destination,
            fit,
            spec: UnitSpec::Contents {
                path: "synthetic-mail".into(),
                approved: tesela_cleaner::sys::FileIdentity {
                    volume_serial: 1,
                    file_id: 1,
                },
            },
            min_age: Duration::ZERO,
            estimate: StatsSnapshot {
                bytes: 1024,
                mail_local_files: 1,
                mail_local_bytes: 1024,
                ..StatsSnapshot::default()
            },
            cloud_synced: false,
            hard_linked: false,
            diagnostic_path: false,
            volume: None,
        }
    }

    #[test]
    fn amber_only_and_permanent_mail_require_their_own_consent() {
        let mut plan = Plan::default();
        plan.items.push(mail_item(
            Destination::Recycle,
            Fit::Unfit(UnfitReason::TooBig),
        ));
        assert!(!can_execute(&plan, false, false));
        assert!(!can_execute(&plan, true, false));
        assert!(can_execute(&plan, true, true));
        plan.items[0].fit = Fit::Fits;
        assert!(
            can_execute(&plan, false, false),
            "recoverable PST needs no extra consent"
        );
        plan.items[0].destination = Destination::Permanent;
        assert!(!can_execute(&plan, false, false));
        assert!(can_execute(&plan, false, true));
        assert_eq!(plan.mail_totals(), ((1, 1024), (0, 0)));
    }

    #[test]
    fn kept_diagnostic_pst_does_not_require_mail_consent() {
        let mut kept = mail_item(Destination::Recycle, Fit::Unfit(UnfitReason::TooBig));
        kept.diagnostic_path = true;
        let mut ordinary = mail_item(Destination::Recycle, Fit::Unfit(UnfitReason::TooBig));
        ordinary.path = "synthetic-other/ordinary.bin".into();
        ordinary.estimate.mail_local_files = 0;
        ordinary.estimate.mail_local_bytes = 0;
        let plan = Plan {
            items: vec![kept, ordinary],
            ..Plan::default()
        };
        assert_eq!(plan.kept_unfit_items().count(), 1);
        assert_eq!(plan.unfit_items().count(), 1);
        assert_eq!(plan.permanent_mail_files(true), 0);
        assert!(can_execute(&plan, true, false));
    }

    #[test]
    fn hiding_the_mail_box_revokes_its_consent() {
        use crate::app::{Action, Settings};
        use crate::i18n::{Lang, Tr};
        use crate::icons::IconCache;
        use egui_kittest::Harness;
        use std::cell::RefCell;
        use std::rc::Rc;

        let mut plan = Plan::default();
        plan.items.push(mail_item(
            Destination::Recycle,
            Fit::Unfit(UnfitReason::TooBig),
        ));
        let mut ordinary = mail_item(Destination::Permanent, Fit::Fits);
        ordinary.path = "synthetic-other/ordinary.bin".into();
        ordinary.estimate.mail_local_files = 0;
        ordinary.estimate.mail_local_bytes = 0;
        plan.items.push(ordinary);
        let plan = Arc::new(plan);
        let confirm = Rc::new(RefCell::new(Confirm {
            plan: plan.clone(),
            allow_unfit: true,
            allow_mail_permanent: true,
            show_all: false,
            reconfirm: false,
            focused: true,
        }));
        assert!(mail_consent_for_execution(&confirm.borrow(), true));
        confirm.borrow_mut().allow_unfit = false;
        assert!(!mail_consent_for_execution(&confirm.borrow(), false));

        let tr = Tr::new(Lang::En);
        let mut icons = IconCache::default();
        let backend: Arc<dyn Backend> =
            Arc::new(crate::demo_backend::DemoBackend::new(Duration::ZERO));
        let c = confirm.clone();
        let mut initialized = false;
        let mut h = Harness::builder()
            .with_size(vec2(900.0, 900.0))
            .build_ui(move |ui| {
                if !initialized {
                    crate::fonts::install(ui.ctx(), crate::fonts::Scripts::NONE);
                    initialized = true;
                    return;
                }
                let mut actions: Vec<Action> = Vec::new();
                let mut env = Env {
                    tr: &tr,
                    pal: Palette::of(false),
                    icons: &mut icons,
                    demo: true,
                    animate: false,
                    drives: &[],
                    drives_loading: false,
                    settings: Settings::default(),
                    backend: &backend,
                    dry_run: false,
                    history: &[],
                    modal_open: true,
                    update: crate::update::UpdateView::default(),
                    actions: &mut actions,
                };
                confirm_groups(ui, &mut env, &mut c.borrow_mut(), &plan);
            });
        h.run_steps(2);
        assert!(!confirm.borrow().allow_mail_permanent);
        confirm.borrow_mut().allow_unfit = true;
        h.run_steps(1);
        assert!(!confirm.borrow().allow_mail_permanent);
    }

    #[test]
    fn cancelling_approval_discards_its_partial_plan() {
        let backend: Arc<dyn Backend> =
            Arc::new(crate::demo_backend::DemoBackend::new(Duration::ZERO));
        let (tx, rx) = crossbeam_channel::bounded(1);
        let mut plan = Plan::default();
        plan.items
            .push(mail_item(Destination::Permanent, Fit::Fits));
        tx.send(plan).unwrap();
        let mut flow = CleanFlow {
            stage: Stage::Approving {
                rx,
                cancel: Arc::new(AtomicBool::new(true)),
                reconfirm: false,
                progress: Arc::new(AtomicU64::new(0)),
            },
            basket: Basket::new(),
            events: Vec::new(),
            note: None,
            vol: VolumeCtx::default(),
            recheck: false,
            blocked_hint: Vec::new(),
        };
        flow.poll(&egui::Context::default(), &backend, false);
        assert!(!flow.busy());
        assert!(!flow.modal_open());
    }

    #[test]
    fn simulated_open_outlook_shows_warning_in_both_languages() {
        use crate::app::{Action, Settings};
        use crate::i18n::{Lang, Tr};
        use crate::icons::IconCache;
        use egui_kittest::Harness;
        use egui_kittest::kittest::Queryable;
        for lang in [Lang::Es, Lang::En] {
            let tr = Tr::new(lang);
            let warning = tr.t("confirm-outlook-open");
            let mut icons = IconCache::default();
            let backend: Arc<dyn Backend> =
                Arc::new(crate::demo_backend::DemoBackend::new(Duration::ZERO));
            let mut plan = Plan {
                outlook_open: true,
                ..Plan::default()
            };
            plan.items
                .push(mail_item(Destination::Permanent, Fit::Fits));
            let mut confirm = Confirm {
                plan: Arc::new(plan),
                allow_unfit: false,
                allow_mail_permanent: false,
                show_all: false,
                reconfirm: false,
                focused: true,
            };
            let mut initialized = false;
            let mut h = Harness::builder()
                .with_size(vec2(900.0, 900.0))
                .build_ui(move |ui| {
                    if !initialized {
                        crate::fonts::install(ui.ctx(), crate::fonts::Scripts::NONE);
                        initialized = true;
                        return;
                    }
                    let mut actions: Vec<Action> = Vec::new();
                    let mut env = Env {
                        tr: &tr,
                        pal: Palette::of(false),
                        icons: &mut icons,
                        demo: true,
                        animate: false,
                        drives: &[],
                        drives_loading: false,
                        settings: Settings::default(),
                        backend: &backend,
                        dry_run: false,
                        history: &[],
                        modal_open: true,
                        update: crate::update::UpdateView::default(),
                        actions: &mut actions,
                    };
                    let plan = confirm.plan.clone();
                    confirm_groups(ui, &mut env, &mut confirm, &plan);
                });
            h.run_steps(2);
            assert!(h.query_all_by_label(&warning).next().is_some());
        }
    }

    #[test]
    fn blocked_log_preparation_leaves_running_ui_responsive() {
        let backend: Arc<dyn Backend> =
            Arc::new(crate::demo_backend::DemoBackend::new(Duration::ZERO));
        let mut flow = CleanFlow::default();
        let ctx = egui::Context::default();
        let (release_tx, release_rx) = crossbeam_channel::bounded(1);
        let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
        let before = Instant::now();
        flow.begin_execution(
            &ctx,
            backend.clone(),
            Arc::new(Plan::default()),
            true,
            None,
            move || {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Err(ExecError::LogRequired)
            },
        );
        assert!(before.elapsed() < Duration::from_millis(50));
        assert!(flow.is_running());
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        flow.poll(&ctx, &backend, true);
        assert!(
            flow.is_running(),
            "worker preparation is blocked, not the UI"
        );
        release_tx.send(()).unwrap();
    }
}
