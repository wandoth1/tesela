//! Global state of the interface: screens, settings, units and the transition from one screen to another.
//!
//! `TeselaUi::show` paints against a `egui::Ui` (without depending on `eframe`): the same code calls it `eframe` from
//! `App::ui` and `egui_kittest` from the tests. Screens do not change global state while drawing: they return
//! [`Action`]s that are applied at the end of the frame.

use crate::demo;
use crate::error::Notice;
use crate::explore::Explore;
use crate::i18n::{Lang, Tr};
use crate::icons::IconCache;
use crate::platform::{self, DriveInfo};
use crate::scan::{ScanOutcome, ScanScreen};
use crate::theme::{self, Palette, ThemePref};
use crate::{home, scan};
use crossbeam_channel::{Receiver, TryRecvError};
use std::path::PathBuf;
use std::sync::Arc;
use tesela_cleaner::Backend;
use tesela_update::{Transport, Version};

/// Language preference: system language or fixed language.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LangPref {
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "es")]
    Es,
    #[serde(rename = "en")]
    En,
}

impl LangPref {
    /// Effective language.
    pub fn resolve(self) -> Lang {
        match self {
            LangPref::System => Lang::system(),
            LangPref::Es => Lang::Es,
            LangPref::En => Lang::En,
        }
    }
}

/// What is saved from checking for updates (OPTIONAL, disabled by default; see `tesela-update`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UpdatePrefs {
    /// The "Check for updates when opening Tesela" setting. When disabled, Tesela does not connect to anything.
    #[serde(default)]
    pub check: bool,
    /// Unix seconds of last attempt (successful or unsuccessful): at most one request every 24 hours.
    #[serde(default)]
    pub last_check: Option<u64>,
    /// The last known published version (so that the notice survives rebooting within 24 hours).
    #[serde(default, deserialize_with = "tesela_update::deserialize_lenient")]
    pub latest_seen: Option<Version>,
    /// The version whose warning was discarded: does not appear again for that same one.
    #[serde(default, deserialize_with = "tesela_update::deserialize_lenient")]
    pub dismissed: Option<Version>,
}

/// Settings that are saved (the window and the `config.toml` file are managed by the executable).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    #[serde(default = "default_theme")]
    pub theme: ThemePref,
    #[serde(default)]
    pub language: LangPref,
    #[serde(default)]
    pub update: UpdatePrefs,
}

fn default_theme() -> ThemePref {
    ThemePref::System
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            theme: ThemePref::System,
            language: LangPref::System,
            update: UpdatePrefs::default(),
        }
    }
}

/// Demo mode options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DemoOptions {
    /// "Stop and See" produces a tree with incomplete folders, and `StartScreen::Map` boots with it.
    pub stopped: bool,
    /// The worst case tree (~20,000 cells) to measure the cost per frame; only with `StartScreen::Map`.
    pub stress: bool,
}

/// Screen with which it starts (only used in demo mode, for screenshots and tests).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum StartScreen {
    #[default]
    Home,
    /// Simulated scan frozen at that fraction (0..=1).
    Scan(f32),
    /// Directly map (or list) the demo tree.
    Map(MapStart),
    /// A screen of the cleaning flow, with SIMULATED support (captures and tests).
    Clean(CleanStart),
}

/// Cleaning screen with which the demonstration starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanStart {
    /// Recommendations.
    Recommendations,
    /// The confirmation screen with an example basket (safe cleanup items, a file, and a project folder).
    Confirm,
    /// The summary after cleaning that same basket.
    Summary,
}

/// Where the exploration of the demonstration begins.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapStart {
    /// Start on the list instead of the map.
    pub list: bool,
    /// Folders that are entered from the root (by name).
    pub open: Vec<String>,
    /// Selected item within that folder (by name).
    pub select: Option<String>,
    /// Items marked in the current folder for a deterministic demo screenshot.
    pub marks: Vec<String>,
}

/// The backup of the cleanup (the real `cleaner` or the simulated `--demo`).
#[derive(Clone)]
pub struct BackendRef(pub Arc<dyn Backend>);

impl std::fmt::Debug for BackendRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Backend(demo: {})", self.0.is_demo())
    }
}

/// How to request an update check (the real one, WinHTTP, or a test).
#[derive(Clone)]
pub struct TransportRef(pub Arc<dyn Transport>);

impl std::fmt::Debug for TransportRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Transport")
    }
}

/// Boot options.
#[derive(Clone, Debug)]
pub struct Options {
    pub settings: Settings,
    /// `Some` = demo mode: synthetic tree and dummy units; nothing is read from the disk.
    pub demo: Option<DemoOptions>,
    /// Encourage? (`SPI_GETCLIENTAREAANIMATION`); the tests deactivate it.
    pub animate: bool,
    pub start: StartScreen,
    /// Simulation mode (`--dry-run`): cleanup records approved file identities WITHOUT touching anything.
    pub dry_run: bool,
    /// Replaces the default fallback (tests inject their own).
    pub backend: Option<BackendRef>,
    /// ISOLATED session (the `--idle-memory` and `--bench-hover` measurement tools, with or without `--demo`): reads
    /// nothing from the user's disk. No cleanup history and an INERT backend (no cleanup roots or data folder:
    /// no `SHGetKnownFolderPath` calls for roots and no `%LOCALAPPDATA%\Tesela\history.jsonl` access).
    pub isolated: bool,
    /// How to check for updates. `None` = no check (demo and
    /// measurement tools NEVER check for updates, regardless of the setting: see `crate::update::Updater`).
    pub update_transport: Option<TransportRef>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            settings: Settings::default(),
            demo: None,
            animate: true,
            start: StartScreen::Home,
            dry_run: false,
            backend: None,
            isolated: false,
            update_transport: None,
        }
    }
}

/// What to scan.
#[derive(Clone, Debug)]
pub struct ScanRequest {
    /// Folder or root of the drive.
    pub root: PathBuf,
    /// Display name ("System (C:)" or the folder name).
    pub label: String,
    /// The drive, if scanning an entire drive (for progress and free space).
    pub drive: Option<DriveInfo>,
}

/// What a screen asks for when finishing drawing.
#[derive(Clone, Debug)]
pub enum Action {
    GoHome,
    StartScan(ScanRequest),
    PickFolder,
    Rescan,
    SetTheme(ThemePref),
    SetLanguage(LangPref),
    /// Open the "About" window.
    ShowAbout,
    /// Clear cleaning history (Settings).
    ClearHistory,
    /// Enable or disable "Check for updates when opening Tile" (Settings).
    SetCheckUpdates(bool),
    /// Dismiss the notice for that version.
    DismissUpdate(Version),
    /// Open the page for that version in the browser (the URL is built by Tesela with the validated version).
    OpenRelease(Version),
}

/// What screens need from the interface while drawing.
pub struct Env<'a> {
    pub tr: &'a Tr,
    pub pal: Palette,
    pub icons: &'a mut IconCache,
    pub demo: bool,
    pub animate: bool,
    pub drives: &'a [DriveInfo],
    pub drives_loading: bool,
    pub settings: Settings,
    /// Cleaning (real or simulated).
    pub backend: &'a Arc<dyn Backend>,
    /// Simulation mode: nothing is deleted.
    pub dry_run: bool,
    /// Summaries of previous cleanups (`history.jsonl`).
    pub history: &'a [tesela_cleaner::HistoryEntry],
    /// There is a modal window open ("About"): the navigation keys of the exploration do not act.
    pub modal_open: bool,
    /// What checking for updates teaches (Settings and startup notice).
    pub update: crate::update::UpdateView,
    pub actions: &'a mut Vec<Action>,
}

pub(crate) enum Screen {
    Home,
    Scan(Box<ScanScreen>),
    Explore(Box<Explore>),
}

/// The complete interface.
pub struct TeselaUi {
    settings: Settings,
    settings_dirty: bool,
    tr: Tr,
    icons: IconCache,
    demo: Option<DemoOptions>,
    animate: bool,
    backend: Arc<dyn Backend>,
    dry_run: bool,
    /// Isolated session: neither history nor real backup (see [`Options::isolated`]).
    isolated: bool,
    history: Vec<tesela_cleaner::HistoryEntry>,
    drives: Vec<DriveInfo>,
    drives_rx: Option<Receiver<Vec<DriveInfo>>>,
    folder_rx: Option<Receiver<Option<PathBuf>>>,
    about: crate::about::About,
    /// Checking for updates optional.
    updater: crate::update::Updater,
    /// Main window, parent (modal) of the folder selector.
    parent: Option<crate::platform::ParentWindow>,
    pub(crate) screen: Screen,
    notice: Option<Notice>,
    last_request: Option<ScanRequest>,
    /// System fonts are activated when the NEXT egui pass begins: the first pass is discarded.
    fonts_pending: bool,
    /// Scripts whose fallback sources are already loaded.
    font_scripts: crate::fonts::Scripts,
}

impl TeselaUi {
    /// Create the interface: install system fonts and styles in `ctx`.
    pub fn new(ctx: &egui::Context, opts: Options) -> TeselaUi {
        crate::fonts::install(ctx, crate::fonts::Scripts::NONE);
        theme::install(ctx, opts.settings.theme);
        let mut ui = TeselaUi {
            settings: opts.settings,
            settings_dirty: false,
            tr: Tr::new(opts.settings.language.resolve()),
            icons: IconCache::default(),
            demo: opts.demo,
            animate: opts.animate,
            backend: opts
                .backend
                .clone()
                .map(|b| b.0)
                .unwrap_or_else(|| default_backend(opts.demo.is_some(), opts.isolated)),
            dry_run: effective_dry_run(opts.dry_run, opts.isolated, opts.demo.is_some()),
            isolated: opts.isolated,
            history: Vec::new(),
            drives: Vec::new(),
            drives_rx: None,
            folder_rx: None,
            about: crate::about::About::default(),
            updater: crate::update::Updater::new(
                opts.update_transport.clone(),
                opts.demo.is_none() && !opts.isolated,
            ),
            parent: None,
            screen: Screen::Home,
            notice: None,
            last_request: None,
            fonts_pending: true,
            font_scripts: crate::fonts::Scripts::NONE,
        };
        ui.reload_history();
        ui.start_update_check(ctx);
        if opts.demo.is_some() {
            ui.drives = demo::drives();
        } else {
            // Drives are polled in a thread - a downed network drive should not freeze boot.
            let (tx, rx) = crossbeam_channel::bounded(1);
            let c = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(platform::drives());
                c.request_repaint();
            });
            ui.drives_rx = Some(rx);
        }
        match (opts.demo, opts.start.clone()) {
            (Some(_), StartScreen::Scan(p)) => {
                let req = ui.demo_request();
                ui.last_request = Some(req.clone());
                ui.screen = Screen::Scan(Box::new(ScanScreen::demo(
                    req,
                    Some(p),
                    opts.demo.unwrap_or_default().stopped,
                    ui.backend.clone(),
                )));
            }
            (Some(d), StartScreen::Map(m)) => {
                let req = ui.demo_request();
                ui.last_request = Some(req.clone());
                let variant = if d.stress {
                    demo::Variant::Stress
                } else if d.stopped {
                    demo::Variant::Stopped
                } else {
                    demo::Variant::Complete
                };
                let mut ex = Explore::demo(req, variant, ui.animate, ui.backend.clone());
                ex.demo_navigate(&m);
                ui.screen = Screen::Explore(Box::new(ex));
            }
            (Some(d), StartScreen::Clean(c)) => {
                let req = ui.demo_request();
                ui.last_request = Some(req.clone());
                let variant = if d.stopped {
                    demo::Variant::Stopped
                } else {
                    demo::Variant::Complete
                };
                let mut ex = Explore::demo(req, variant, ui.animate, ui.backend.clone());
                ex.demo_clean(&ui.backend, c, ui.dry_run);
                ui.screen = Screen::Explore(Box::new(ex));
            }
            _ => {}
        }
        ui
    }

    /// Reads the wipe history again (the demo doesn't have it: it never writes anything).
    fn reload_history(&mut self) {
        self.history = load_history(self.demo.is_some(), self.isolated, self.backend.data_dir());
    }

    /// Ask for the latest version if the setting is activated, it's time (24 hours since the last attempt) and this session is not
    /// demonstration nor a measurement tool. Write down the attempt NOW: even if the request does not finish, it is not
    /// Repeat within 24 hours.
    fn start_update_check(&mut self, ctx: &egui::Context) {
        let now = unix_now();
        if self.updater.start_if_due(ctx, &self.settings.update, now) {
            self.settings.update.last_check = Some(now);
            self.settings_dirty = true;
        }
    }

    fn demo_request(&self) -> ScanRequest {
        let drive = demo::drives().remove(0);
        ScanRequest {
            root: drive.root(),
            label: format!("{} ({}:)", drive.label, drive.letter),
            drive: Some(drive),
        }
    }

    /// Indicates the main window: the folder selector opens as its modal dialog.
    pub fn set_parent_window(&mut self, w: &impl raw_window_handle::HasWindowHandle) {
        if self.parent.is_none() {
            self.parent = crate::platform::ParentWindow::from_handle(w);
        }
    }

    /// Current settings.
    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// The settings have changed since the last time you asked (so save them).
    pub fn take_settings_change(&mut self) -> Option<Settings> {
        std::mem::take(&mut self.settings_dirty).then_some(self.settings)
    }

    /// Language in use.
    pub fn lang(&self) -> Lang {
        self.tr.lang()
    }

    /// Start analyzing `root` (for `tesela --scan <ruta>`). If it is the root of a unit, its space is used for the
    /// progress.
    pub fn request_scan(&mut self, ctx: &egui::Context, root: PathBuf) {
        let drive = platform::drives()
            .into_iter()
            .find(|d| dunce::simplified(&d.root()) == dunce::simplified(&root));
        let label = match &drive {
            Some(d) => crate::home::drive_title_plain(&self.tr, d),
            None => root.file_name().map_or_else(
                || root.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
        };
        self.start_scan(ctx, ScanRequest { root, label, drive });
    }

    /// The scan, if it is the current screen (for tests).
    #[allow(dead_code)]
    pub(crate) fn explore(&self) -> Option<&Explore> {
        match &self.screen {
            Screen::Explore(e) => Some(e),
            _ => None,
        }
    }

    /// If the scanning screen scrolls your two cards (for tests).
    #[cfg(test)]
    pub(crate) fn scan_overflow(&self) -> Option<(bool, bool)> {
        match &self.screen {
            Screen::Scan(s) => Some(s.overflow()),
            _ => None,
        }
    }

    /// Replaces the units taught at the beginning (for tests).
    #[cfg(test)]
    pub(crate) fn set_drives(&mut self, drives: Vec<DriveInfo>) {
        self.drives = drives;
    }

    /// Replaces the way of opening the "See what's new" browser (for tests: nothing opens).
    #[cfg(test)]
    pub(crate) fn set_url_opener(&mut self, opener: Box<dyn Fn(&str)>) {
        self.updater.set_opener(opener);
    }

    /// What checking for updates now teaches (for tests).
    #[cfg(test)]
    pub(crate) fn update_view(&self) -> crate::update::UpdateView {
        self.updater.view(&self.settings.update)
    }

    /// Applies an action as if a screen had requested it (for tests).
    #[cfg(test)]
    pub(crate) fn apply_for_test(&mut self, action: Action) {
        self.apply(&egui::Context::default(), vec![action]);
    }

    /// Marks the last check as failed (for readiness tests).
    #[cfg(test)]
    pub(crate) fn force_update_failed_for_test(&mut self) {
        self.updater.force_failed();
    }

    /// Exploration, mutable (for tests).
    #[allow(dead_code)]
    pub(crate) fn explore_mut(&mut self) -> Option<&mut Explore> {
        match &mut self.screen {
            Screen::Explore(e) => Some(e),
            _ => None,
        }
    }

    /// Name of the current screen (`home`, `scan` or `explore`), for measurement tools.
    pub fn screen_name(&self) -> &'static str {
        match self.screen {
            Screen::Home => "home",
            Screen::Scan(_) => "scan",
            Screen::Explore(_) => "explore",
        }
    }

    /// Figures from the last drawing of the map, if you are exploring.
    pub fn map_stats(&self) -> Option<crate::map::MapStats> {
        match &self.screen {
            Screen::Explore(e) => Some(e.map.stats()),
            _ => None,
        }
    }

    /// Window title depending on the screen.
    pub fn window_title(&self) -> String {
        let name = self.tr.t("app-name");
        match &self.screen {
            Screen::Home => name,
            Screen::Scan(s) => format!(
                "{name} · {}",
                self.tr
                    .ta("title-scanning", crate::args!["name" => s.label()])
            ),
            Screen::Explore(e) => format!("{name} · {}", e.title()),
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if self.updater.poll(&mut self.settings.update) {
            self.settings_dirty = true;
        }
        if let Some(rx) = &self.drives_rx {
            match rx.try_recv() {
                Ok(d) => {
                    self.drives = d;
                    self.drives_rx = None;
                }
                Err(TryRecvError::Disconnected) => self.drives_rx = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &self.folder_rx {
            match rx.try_recv() {
                Ok(Some(path)) => {
                    self.folder_rx = None;
                    let label = path.file_name().map_or_else(
                        || path.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    self.start_scan(
                        ctx,
                        ScanRequest {
                            root: path,
                            label,
                            drive: None,
                        },
                    );
                }
                Ok(None) | Err(TryRecvError::Disconnected) => self.folder_rx = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        // Is the scan finished?
        let outcome = match &mut self.screen {
            Screen::Scan(s) => s.poll(ctx),
            _ => None,
        };
        match outcome {
            Some(ScanOutcome::Done(ex)) => self.screen = Screen::Explore(ex),
            Some(ScanOutcome::Failed(e)) => {
                // The notice is saved TYPE: it is translated when painted (and changes with the language).
                self.notice = Some(Notice::ScanStart(e));
                self.screen = Screen::Home;
            }
            Some(ScanOutcome::Cancelled) => self.screen = Screen::Home,
            None => {}
        }
    }

    fn start_scan(&mut self, ctx: &egui::Context, req: ScanRequest) {
        self.notice = None;
        self.last_request = Some(req.clone());
        let screen = if let Some(d) = self.demo {
            ScanScreen::demo(req, None, d.stopped, self.backend.clone())
        } else {
            ScanScreen::real(ctx, req, self.animate, self.backend.clone())
        };
        self.screen = Screen::Scan(Box::new(screen));
    }

    fn apply(&mut self, ctx: &egui::Context, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::GoHome => {
                    // If there is a scan in progress it is canceled (the threads terminate themselves).
                    if let Screen::Scan(s) = &mut self.screen {
                        s.cancel();
                    }
                    self.screen = Screen::Home;
                    self.reload_history();
                }
                Action::StartScan(req) => self.start_scan(ctx, req),
                Action::Rescan => {
                    if let Some(req) = self.last_request.clone() {
                        self.start_scan(ctx, req);
                    }
                }
                Action::PickFolder => {
                    if self.demo.is_some() {
                        // In the demo no dialog is opened: the fictitious user folder is analyzed.
                        let req = self.demo_request();
                        self.start_scan(ctx, req);
                    } else if self.folder_rx.is_none() {
                        let (tx, rx) = crossbeam_channel::bounded(1);
                        let c = ctx.clone();
                        let parent = self.parent;
                        std::thread::spawn(move || {
                            let mut dialog = rfd::FileDialog::new();
                            if let Some(p) = &parent {
                                dialog = dialog.set_parent(p);
                            }
                            let _ = tx.send(dialog.pick_folder());
                            c.request_repaint();
                        });
                        self.folder_rx = Some(rx);
                    }
                }
                Action::ShowAbout => self.about.open = true,
                Action::ClearHistory => {
                    // An isolated session does not touch anything on the user's disk.
                    if let Some(dir) = self.backend.data_dir().filter(|_| !self.isolated) {
                        let _ = tesela_cleaner::log::history_clear(&dir);
                    }
                    self.history.clear();
                }
                Action::SetTheme(t) => {
                    self.settings.theme = t;
                    self.settings_dirty = true;
                    theme::apply_pref(ctx, t);
                }
                Action::SetLanguage(l) => {
                    self.settings.language = l;
                    self.settings_dirty = true;
                    self.tr = Tr::new(l.resolve());
                }
                Action::SetCheckUpdates(on) => {
                    self.settings.update.check = on;
                    self.settings_dirty = true;
                    self.updater.settings_changed(on);
                    // When you activate it, it is checked (if it touches): the user has just requested it.
                    self.start_update_check(ctx);
                }
                Action::DismissUpdate(v) => {
                    self.settings.update.dismissed = Some(v);
                    self.settings_dirty = true;
                }
                Action::OpenRelease(v) => self.updater.open_release(v),
            }
        }
    }

    /// If you were asked to close the window (`requested`) while a cleanup is running, it returns `true` (you must cancel the
    /// close) and the progress window explains to wait or cancel: closing halfway would leave the cleaning halfway done. In
    /// any other time is not interfered with.
    pub(crate) fn guard_close(&mut self, requested: bool) -> bool {
        if !requested {
            return false;
        }
        match &mut self.screen {
            Screen::Explore(e) if e.flow.is_running() => {
                e.flow.block_close();
                true
            }
            // “Wipe Safe” is also launched from the scanning screen.
            Screen::Scan(s) if s.clean_running() => {
                s.block_close();
                true
            }
            _ => false,
        }
    }

    /// Paints the current screen.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if std::mem::take(&mut self.fonts_pending) {
            // `set_fonts` (in `new`) is only valid from the next pass; Without this, the first frame would ask for the family
            // "semibold" before it exists.
            ctx.request_discard("activar las fuentes del sistema");
            return;
        }
        self.poll(&ctx);
        if self.guard_close(ctx.input(|i| i.viewport().close_requested())) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        self.about.handle_escape(&ctx);
        let pal = theme::palette(&ctx);
        let mut actions = Vec::new();
        {
            let mut env = Env {
                tr: &self.tr,
                pal,
                icons: &mut self.icons,
                demo: self.demo.is_some(),
                animate: self.animate,
                drives: &self.drives,
                drives_loading: self.drives_rx.is_some(),
                settings: self.settings,
                backend: &self.backend,
                dry_run: self.dry_run,
                history: &self.history,
                modal_open: self.about.open,
                update: self.updater.view(&self.settings.update),
                actions: &mut actions,
            };
            let notice_text = self.notice.as_ref().map(|n| n.text(&self.tr));
            match &mut self.screen {
                Screen::Home => home::show(
                    ui,
                    &mut env,
                    notice_text.as_deref(),
                    self.folder_rx.is_some(),
                ),
                Screen::Scan(s) => scan::show(ui, &mut env, s),
                Screen::Explore(e) => e.show(ui, &mut env),
            }
        }
        self.apply(&ctx, actions);
        self.about.show(&ctx, &self.tr, &pal);
        self.ensure_fonts(&ctx);
    }

    /// Loads backup sources for on-screen scripts (tree names, unit labels, and
    /// search engine) if they are not already there. They are valid from the next frame.
    fn ensure_fonts(&mut self, ctx: &egui::Context) {
        let mut needed = self
            .drives
            .iter()
            .fold(crate::fonts::Scripts::NONE, |a, d| {
                a | crate::fonts::scripts_of_str(&d.label)
            });
        if let Screen::Explore(e) = &self.screen {
            needed = needed | e.scripts | crate::fonts::scripts_of_str(&e.search);
        }
        if !self.font_scripts.contains(needed) {
            self.font_scripts = self.font_scripts | needed;
            crate::fonts::install(ctx, self.font_scripts);
            ctx.request_repaint();
        }
    }
}

/// Is dry-run mode in effect? An isolated session other than demo mode (measurement tools without `--demo`) always runs
/// in dry-run mode: its inert backend has no protection entries and no data folder, so cleanup cannot proceed without a log;
/// dry-run mode independently prevents writes (defense in depth). Demo behavior does not change (its mock backend branches
/// on `dry_run`, and the README summaries and screenshots depend on this).
fn effective_dry_run(dry_run: bool, isolated: bool, demo: bool) -> bool {
    dry_run || (isolated && !demo)
}

/// The cleaning history shown: that of the backup data folder, except in the demonstration and in a
/// isolated session, which do not read anything from the disk (the demo never has a data folder; isolation guarantees this
/// although the backup has it).
fn load_history(
    demo: bool,
    isolated: bool,
    data_dir: Option<PathBuf>,
) -> Vec<tesela_cleaner::HistoryEntry> {
    match data_dir {
        Some(dir) if !demo && !isolated => tesela_cleaner::log::history_load(&dir),
        _ => Vec::new(),
    }
}

/// The default fallback: the real `cleaner` (roots of `SHGetKnownFolderPath`), the one simulated in the demo, or one
/// INERT in an isolated session (`Roots::default()`: no cleanup roots, no downloads and no data folder, so
/// does not check known folders or read or write history).
fn default_backend(demo: bool, isolated: bool) -> Arc<dyn Backend> {
    if demo {
        return Arc::new(crate::demo_backend::DemoBackend::new(
            std::time::Duration::from_millis(1600),
        ));
    }
    if isolated {
        return Arc::new(tesela_cleaner::Cleaner::new(
            tesela_cleaner::Roots::default(),
        ));
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .and_then(|d| tesela_cleaner::paths::canonical_path(&d).ok());
    Arc::new(tesela_cleaner::Cleaner::new(
        tesela_cleaner::Roots::from_system(exe_dir.as_deref()),
    ))
}

/// Unix seconds from now.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TestDir;

    fn entry() -> tesela_cleaner::HistoryEntry {
        tesela_cleaner::HistoryEntry {
            unix: 1,
            drive: b'C',
            used: 2,
            freed_now: 3,
            to_bin: 4,
        }
    }

    #[test]
    fn una_sesion_aislada_no_lee_el_historial_aunque_haya_carpeta_de_datos() {
        let t = TestDir::new();
        tesela_cleaner::log::history_append(t.path(), &entry()).unwrap();
        // Normal: it reads.
        assert_eq!(
            load_history(false, false, Some(t.path().to_path_buf())).len(),
            1
        );
        // GM-M4: isolated (`--idle-memory`/`--bench-hover` without `--demo`) or demo: nothing.
        assert!(load_history(false, true, Some(t.path().to_path_buf())).is_empty());
        assert!(load_history(true, false, Some(t.path().to_path_buf())).is_empty());
        assert!(load_history(true, true, Some(t.path().to_path_buf())).is_empty());
        // Without a data folder there is no history either.
        assert!(load_history(false, false, None).is_empty());
    }

    #[test]
    fn el_respaldo_de_una_sesion_aislada_es_inerte() {
        // GM-M4: No cleanup roots, no downloads, no data folder (no history or logs) and no `from_system`.
        let b = default_backend(false, true);
        assert!(!b.is_demo());
        assert_eq!(b.data_dir(), None);
        assert_eq!(b.downloads_dir(), None);
        for id in tesela_cleaner::CategoryId::ALL {
            assert!(b.category_roots(id).is_empty(), "{id:?}");
            assert!(b.unavailable_roots(id).is_empty(), "{id:?}");
        }
        // The demo is still the simulated one, and by default the real backup has its data folder.
        assert!(default_backend(true, true).is_demo());
        assert!(default_backend(true, false).is_demo());
    }

    #[test]
    fn la_sesion_aislada_sin_demo_es_siempre_una_simulacion_y_la_demo_no_cambia() {
        // R-S1: measurement tools without `--demo` (isolated, no demonstration): forced simulation.
        assert!(effective_dry_run(false, true, false));
        assert!(effective_dry_run(true, true, false));
        // Demo (isolated, with demo) and normal use respect `--dry-run` as is.
        assert!(!effective_dry_run(false, true, true));
        assert!(effective_dry_run(true, true, true));
        assert!(!effective_dry_run(false, false, false));
        assert!(effective_dry_run(true, false, false));
        assert!(!effective_dry_run(false, false, true));
    }

    #[test]
    fn por_defecto_la_sesion_no_es_aislada() {
        assert!(!Options::default().isolated);
    }
}
