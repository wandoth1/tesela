//! Estado global de la interfaz: pantallas, ajustes, unidades y el paso de una pantalla a otra.
//!
//! `TeselaUi::show` pinta contra un `egui::Ui` (sin depender de `eframe`): el mismo código lo llama `eframe` desde
//! `App::ui` y `egui_kittest` desde los tests. Las pantallas no cambian el estado global mientras dibujan: devuelven
//! [`Action`]s que se aplican al terminar el frame.

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

/// Preferencia de idioma: el del sistema o uno fijo.
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
    /// Idioma efectivo.
    pub fn resolve(self) -> Lang {
        match self {
            LangPref::System => Lang::system(),
            LangPref::Es => Lang::Es,
            LangPref::En => Lang::En,
        }
    }
}

/// Lo que se guarda de la comprobación de actualizaciones (OPCIONAL, desactivada por defecto; ver `tesela-update`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UpdatePrefs {
    /// El ajuste "Buscar actualizaciones al abrir Tesela". Sin activar, Tesela no se conecta a nada.
    #[serde(default)]
    pub check: bool,
    /// Segundos Unix del último intento (con o sin éxito): como mucho una petición cada 24 horas.
    #[serde(default)]
    pub last_check: Option<u64>,
    /// La última versión publicada que se supo (para que el aviso sobreviva a reiniciar dentro de las 24 horas).
    #[serde(default, deserialize_with = "tesela_update::deserialize_lenient")]
    pub latest_seen: Option<Version>,
    /// La versión cuyo aviso se descartó: no vuelve a salir para esa misma.
    #[serde(default, deserialize_with = "tesela_update::deserialize_lenient")]
    pub dismissed: Option<Version>,
}

/// Ajustes que se guardan (la ventana y el archivo `config.toml` los gestiona el ejecutable).
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

/// Opciones del modo demostración.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DemoOptions {
    /// "Parar y ver" produce un árbol con carpetas incompletas, y `StartScreen::Map` arranca con él.
    pub stopped: bool,
    /// El árbol del peor caso (~20 000 celdas) para medir el coste por frame; solo con `StartScreen::Map`.
    pub stress: bool,
}

/// Pantalla con la que arranca (solo se usa en el modo demostración, para capturas y pruebas).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum StartScreen {
    #[default]
    Home,
    /// Escaneo simulado congelado en esa fracción (0..=1).
    Scan(f32),
    /// Directamente el mapa (o la lista) del árbol de demostración.
    Map(MapStart),
    /// Una pantalla del flujo de limpieza, con el respaldo SIMULADO (capturas y pruebas).
    Clean(CleanStart),
}

/// Pantalla de la limpieza con la que arranca la demostración.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanStart {
    /// Recomendaciones.
    Recommendations,
    /// La confirmación, con una cesta de ejemplo (lo seguro, un archivo y una carpeta de proyectos).
    Confirm,
    /// El resumen tras limpiar esa misma cesta.
    Summary,
}

/// Dónde arranca la exploración de la demostración.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapStart {
    /// Empieza en la lista en vez de en el mapa.
    pub list: bool,
    /// Carpetas por las que se entra desde la raíz (por nombre).
    pub open: Vec<String>,
    /// Elemento seleccionado dentro de esa carpeta (por nombre).
    pub select: Option<String>,
    /// Items marked in the current folder for a deterministic demo screenshot.
    pub marks: Vec<String>,
}

/// El respaldo de la limpieza (el `cleaner` real o el simulado de `--demo`).
#[derive(Clone)]
pub struct BackendRef(pub Arc<dyn Backend>);

impl std::fmt::Debug for BackendRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Backend(demo: {})", self.0.is_demo())
    }
}

/// Cómo se hace la petición de la comprobación de actualizaciones (la real, de WinHTTP, o la de una prueba).
#[derive(Clone)]
pub struct TransportRef(pub Arc<dyn Transport>);

impl std::fmt::Debug for TransportRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Transport")
    }
}

/// Opciones de arranque.
#[derive(Clone, Debug)]
pub struct Options {
    pub settings: Settings,
    /// `Some` = modo demostración: árbol sintético y unidades ficticias; nada se lee del disco.
    pub demo: Option<DemoOptions>,
    /// ¿Animar? (`SPI_GETCLIENTAREAANIMATION`); los tests lo desactivan.
    pub animate: bool,
    pub start: StartScreen,
    /// Modo simulación (`--dry-run`): la limpieza registra las identidades aprobadas SIN tocar nada.
    pub dry_run: bool,
    /// Sustituye al respaldo por defecto (las pruebas inyectan el suyo).
    pub backend: Option<BackendRef>,
    /// Sesión AISLADA (las herramientas de medición `--idle-memory` y `--bench-hover`, con o sin `--demo`): no se lee
    /// nada del disco del usuario. Sin historial de limpiezas y con un respaldo INERTE (sin raíces de limpieza ni carpeta
    /// de datos: ni `SHGetKnownFolderPath` para las raíces, ni `%LOCALAPPDATA%\Tesela\history.jsonl`).
    pub isolated: bool,
    /// Con qué se hace la comprobación de actualizaciones. `None` = no hay comprobación (la demostración y las
    /// herramientas de medición NUNCA comprueban, lo pasen o no: ver `crate::update::Updater`).
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

/// Qué escanear.
#[derive(Clone, Debug)]
pub struct ScanRequest {
    /// Carpeta o raíz de la unidad.
    pub root: PathBuf,
    /// Nombre para mostrar ("Sistema (C:)" o el nombre de la carpeta).
    pub label: String,
    /// La unidad, si se escanea una unidad entera (para el progreso y el espacio libre).
    pub drive: Option<DriveInfo>,
}

/// Lo que una pantalla pide al terminar de dibujar.
#[derive(Clone, Debug)]
pub enum Action {
    GoHome,
    StartScan(ScanRequest),
    PickFolder,
    Rescan,
    SetTheme(ThemePref),
    SetLanguage(LangPref),
    /// Abre la ventana "Acerca de".
    ShowAbout,
    /// Borra el historial de limpiezas (Ajustes).
    ClearHistory,
    /// Activa o desactiva "Buscar actualizaciones al abrir Tesela" (Ajustes).
    SetCheckUpdates(bool),
    /// Descarta el aviso de esa versión.
    DismissUpdate(Version),
    /// Abre en el navegador la página de esa versión (la URL la construye Tesela con la versión validada).
    OpenRelease(Version),
}

/// Lo que las pantallas necesitan de la interfaz mientras dibujan.
pub struct Env<'a> {
    pub tr: &'a Tr,
    pub pal: Palette,
    pub icons: &'a mut IconCache,
    pub demo: bool,
    pub animate: bool,
    pub drives: &'a [DriveInfo],
    pub drives_loading: bool,
    pub settings: Settings,
    /// La limpieza (real o simulada).
    pub backend: &'a Arc<dyn Backend>,
    /// Modo simulación: no se borra nada.
    pub dry_run: bool,
    /// Resúmenes de las limpiezas anteriores (`history.jsonl`).
    pub history: &'a [tesela_cleaner::HistoryEntry],
    /// Hay una ventana modal abierta ("Acerca de"): las teclas de navegación de la exploración no actúan.
    pub modal_open: bool,
    /// Lo que la comprobación de actualizaciones enseña (Ajustes y aviso del inicio).
    pub update: crate::update::UpdateView,
    pub actions: &'a mut Vec<Action>,
}

pub(crate) enum Screen {
    Home,
    Scan(Box<ScanScreen>),
    Explore(Box<Explore>),
}

/// La interfaz completa.
pub struct TeselaUi {
    settings: Settings,
    settings_dirty: bool,
    tr: Tr,
    icons: IconCache,
    demo: Option<DemoOptions>,
    animate: bool,
    backend: Arc<dyn Backend>,
    dry_run: bool,
    /// Sesión aislada: ni historial ni respaldo real (ver [`Options::isolated`]).
    isolated: bool,
    history: Vec<tesela_cleaner::HistoryEntry>,
    drives: Vec<DriveInfo>,
    drives_rx: Option<Receiver<Vec<DriveInfo>>>,
    folder_rx: Option<Receiver<Option<PathBuf>>>,
    about: crate::about::About,
    /// La comprobación de actualizaciones opcional.
    updater: crate::update::Updater,
    /// Ventana principal, padre (modal) del selector de carpeta.
    parent: Option<crate::platform::ParentWindow>,
    pub(crate) screen: Screen,
    notice: Option<Notice>,
    last_request: Option<ScanRequest>,
    /// Las fuentes del sistema se activan al empezar el SIGUIENTE pase de egui: el primer pase se descarta.
    fonts_pending: bool,
    /// Escrituras cuyas fuentes de reserva ya están cargadas.
    font_scripts: crate::fonts::Scripts,
}

impl TeselaUi {
    /// Crea la interfaz: instala fuentes del sistema y estilos en `ctx`.
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
            // Las unidades se consultan en un hilo: una unidad de red caída no debe congelar el arranque.
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

    /// Vuelve a leer el historial de limpiezas (la demostración no lo tiene: nunca escribe nada).
    fn reload_history(&mut self) {
        self.history = load_history(self.demo.is_some(), self.isolated, self.backend.data_dir());
    }

    /// Pide la última versión si el ajuste está activado, ya toca (24 horas desde el último intento) y esta sesión no es
    /// la demostración ni una herramienta de medición. Anota el intento YA: aunque la petición no llegue a terminar, no se
    /// repite antes de 24 horas.
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

    /// Indica la ventana principal: el selector de carpeta se abre como diálogo modal suyo.
    pub fn set_parent_window(&mut self, w: &impl raw_window_handle::HasWindowHandle) {
        if self.parent.is_none() {
            self.parent = crate::platform::ParentWindow::from_handle(w);
        }
    }

    /// Ajustes actuales.
    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Los ajustes si cambiaron desde la última vez que se preguntó (para guardarlos).
    pub fn take_settings_change(&mut self) -> Option<Settings> {
        std::mem::take(&mut self.settings_dirty).then_some(self.settings)
    }

    /// Idioma en uso.
    pub fn lang(&self) -> Lang {
        self.tr.lang()
    }

    /// Empieza a analizar `root` (para `tesela --scan <ruta>`). Si es la raíz de una unidad se usa su espacio para el
    /// progreso.
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

    /// La exploración, si es la pantalla actual (para los tests).
    #[allow(dead_code)]
    pub(crate) fn explore(&self) -> Option<&Explore> {
        match &self.screen {
            Screen::Explore(e) => Some(e),
            _ => None,
        }
    }

    /// Si la pantalla de escaneo desplaza sus dos tarjetas (para los tests).
    #[cfg(test)]
    pub(crate) fn scan_overflow(&self) -> Option<(bool, bool)> {
        match &self.screen {
            Screen::Scan(s) => Some(s.overflow()),
            _ => None,
        }
    }

    /// Sustituye las unidades que enseña el inicio (para los tests).
    #[cfg(test)]
    pub(crate) fn set_drives(&mut self, drives: Vec<DriveInfo>) {
        self.drives = drives;
    }

    /// Sustituye la forma de abrir el navegador de "Ver novedades" (para los tests: no se abre nada).
    #[cfg(test)]
    pub(crate) fn set_url_opener(&mut self, opener: Box<dyn Fn(&str)>) {
        self.updater.set_opener(opener);
    }

    /// Lo que la comprobación de actualizaciones enseña ahora (para los tests).
    #[cfg(test)]
    pub(crate) fn update_view(&self) -> crate::update::UpdateView {
        self.updater.view(&self.settings.update)
    }

    /// Aplica una acción como si una pantalla la hubiera pedido (para los tests).
    #[cfg(test)]
    pub(crate) fn apply_for_test(&mut self, action: Action) {
        self.apply(&egui::Context::default(), vec![action]);
    }

    /// Marca la última comprobación como fallida (para los tests de disposición).
    #[cfg(test)]
    pub(crate) fn force_update_failed_for_test(&mut self) {
        self.updater.force_failed();
    }

    /// La exploración, mutable (para los tests).
    #[allow(dead_code)]
    pub(crate) fn explore_mut(&mut self) -> Option<&mut Explore> {
        match &mut self.screen {
            Screen::Explore(e) => Some(e),
            _ => None,
        }
    }

    /// Nombre de la pantalla actual (`home`, `scan` o `explore`), para las herramientas de medición.
    pub fn screen_name(&self) -> &'static str {
        match self.screen {
            Screen::Home => "home",
            Screen::Scan(_) => "scan",
            Screen::Explore(_) => "explore",
        }
    }

    /// Cifras del último dibujo del mapa, si se está explorando.
    pub fn map_stats(&self) -> Option<crate::map::MapStats> {
        match &self.screen {
            Screen::Explore(e) => Some(e.map.stats()),
            _ => None,
        }
    }

    /// Título de la ventana según la pantalla.
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
        // ¿Terminó el escaneo?
        let outcome = match &mut self.screen {
            Screen::Scan(s) => s.poll(ctx),
            _ => None,
        };
        match outcome {
            Some(ScanOutcome::Done(ex)) => self.screen = Screen::Explore(ex),
            Some(ScanOutcome::Failed(e)) => {
                // El aviso se guarda TIPADO: se traduce al pintarlo (y cambia con el idioma).
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
                    // Si hay un escaneo en marcha se cancela (los hilos terminan solos).
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
                        // En la demostración no se abre ningún diálogo: se analiza la carpeta de usuario ficticia.
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
                    // Una sesión aislada no toca nada del disco del usuario.
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
                    // Al activarlo se comprueba ya (si toca): el usuario acaba de pedirlo.
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

    /// Si se pidió cerrar la ventana (`requested`) mientras se ejecuta una limpieza, devuelve `true` (hay que cancelar el
    /// cierre) y la ventana de progreso explica que espere o cancele: cerrar a mitad dejaría la limpieza a medias. En
    /// cualquier otro momento no se interfiere.
    pub(crate) fn guard_close(&mut self, requested: bool) -> bool {
        if !requested {
            return false;
        }
        match &mut self.screen {
            Screen::Explore(e) if e.flow.is_running() => {
                e.flow.block_close();
                true
            }
            // "Limpiar lo seguro" también se lanza desde la pantalla de escaneo.
            Screen::Scan(s) if s.clean_running() => {
                s.block_close();
                true
            }
            _ => false,
        }
    }

    /// Pinta la pantalla actual.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if std::mem::take(&mut self.fonts_pending) {
            // `set_fonts` (en `new`) solo vale desde el siguiente pase; sin esto, el primer frame pediría la familia
            // "semibold" antes de que exista.
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

    /// Carga las fuentes de reserva de las escrituras que hay en pantalla (nombres del árbol, etiquetas de unidades y
    /// buscador) si aún no están. Valen desde el siguiente frame.
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

/// ¿Modo simulación efectivo? Una sesión aislada que NO es la demostración (las herramientas de medición sin `--demo`) es siempre
/// una simulación: su respaldo inerte tiene la protección vacía y solo no borra nada porque no tiene carpeta de datos (sin
/// registro, la limpieza no se ejecuta); la simulación lo garantiza por sí misma (defensa en profundidad). La demostración
/// NO cambia (su respaldo simulado bifurca por `dry_run` y los resúmenes y las capturas del README dependen de ello).
fn effective_dry_run(dry_run: bool, isolated: bool, demo: bool) -> bool {
    dry_run || (isolated && !demo)
}

/// El historial de limpiezas que se enseña: el de la carpeta de datos del respaldo, salvo en la demostración y en una
/// sesión aislada, que no leen nada del disco (la demostración nunca tiene carpeta de datos; el aislamiento lo garantiza
/// aunque el respaldo la tenga).
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

/// El respaldo por defecto: el `cleaner` real (raíces de `SHGetKnownFolderPath`), el simulado en la demostración, o uno
/// INERTE en una sesión aislada (`Roots::default()`: sin raíces de limpieza, sin descargas y sin carpeta de datos, así que
/// no consulta las carpetas conocidas ni lee ni escribe el historial).
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

/// Segundos Unix de ahora.
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
        // Normal: se lee.
        assert_eq!(
            load_history(false, false, Some(t.path().to_path_buf())).len(),
            1
        );
        // GM-M4: aislada (`--idle-memory`/`--bench-hover` sin `--demo`) o demostración: nada.
        assert!(load_history(false, true, Some(t.path().to_path_buf())).is_empty());
        assert!(load_history(true, false, Some(t.path().to_path_buf())).is_empty());
        assert!(load_history(true, true, Some(t.path().to_path_buf())).is_empty());
        // Sin carpeta de datos tampoco hay historial.
        assert!(load_history(false, false, None).is_empty());
    }

    #[test]
    fn el_respaldo_de_una_sesion_aislada_es_inerte() {
        // GM-M4: sin raíces de limpieza, sin descargas, sin carpeta de datos (ni historial ni registros) y sin `from_system`.
        let b = default_backend(false, true);
        assert!(!b.is_demo());
        assert_eq!(b.data_dir(), None);
        assert_eq!(b.downloads_dir(), None);
        for id in tesela_cleaner::CategoryId::ALL {
            assert!(b.category_roots(id).is_empty(), "{id:?}");
            assert!(b.unavailable_roots(id).is_empty(), "{id:?}");
        }
        // La demostración sigue siendo la simulada, y por defecto el respaldo real tiene su carpeta de datos.
        assert!(default_backend(true, true).is_demo());
        assert!(default_backend(true, false).is_demo());
    }

    #[test]
    fn la_sesion_aislada_sin_demo_es_siempre_una_simulacion_y_la_demo_no_cambia() {
        // R-S1: herramientas de medición sin `--demo` (aisladas, sin demostración): simulación forzada.
        assert!(effective_dry_run(false, true, false));
        assert!(effective_dry_run(true, true, false));
        // La demostración (aislada, con demo) y el uso normal respetan `--dry-run` tal cual.
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
