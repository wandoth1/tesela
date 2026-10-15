//! Estado global de la interfaz: pantallas, ajustes, unidades y el paso de una pantalla a otra.
//!
//! `TeselaUi::show` pinta contra un `egui::Ui` (sin depender de `eframe`): el mismo código lo llama `eframe` desde
//! `App::ui` y `egui_kittest` desde los tests. Las pantallas no cambian el estado global mientras dibujan: devuelven
//! [`Action`]s que se aplican al terminar el frame.

use crate::demo;
use crate::explore::Explore;
use crate::i18n::{Lang, Tr};
use crate::icons::IconCache;
use crate::platform::{self, DriveInfo};
use crate::scan::{ScanOutcome, ScanScreen};
use crate::theme::{self, Palette, ThemePref};
use crate::{home, scan};
use crossbeam_channel::{Receiver, TryRecvError};
use std::path::PathBuf;

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

/// Ajustes que se guardan (la ventana y el archivo `config.toml` los gestiona el ejecutable).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    #[serde(default = "default_theme")]
    pub theme: ThemePref,
    #[serde(default)]
    pub language: LangPref,
}

fn default_theme() -> ThemePref {
    ThemePref::System
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            theme: ThemePref::System,
            language: LangPref::System,
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
}

impl Default for Options {
    fn default() -> Options {
        Options {
            settings: Settings::default(),
            demo: None,
            animate: true,
            start: StartScreen::Home,
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
    drives: Vec<DriveInfo>,
    drives_rx: Option<Receiver<Vec<DriveInfo>>>,
    folder_rx: Option<Receiver<Option<PathBuf>>>,
    /// Ventana principal, padre (modal) del selector de carpeta.
    parent: Option<crate::platform::ParentWindow>,
    pub(crate) screen: Screen,
    notice: Option<String>,
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
            drives: Vec::new(),
            drives_rx: None,
            folder_rx: None,
            parent: None,
            screen: Screen::Home,
            notice: None,
            last_request: None,
            fonts_pending: true,
            font_scripts: crate::fonts::Scripts::NONE,
        };
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
                let mut ex = Explore::demo(req, variant, ui.animate);
                ex.demo_navigate(&m);
                ui.screen = Screen::Explore(Box::new(ex));
            }
            _ => {}
        }
        ui
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
            Some(ScanOutcome::Failed(msg)) => {
                self.notice = Some(self.tr.ta("error-scan-start", crate::args!["error" => msg]));
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
            ScanScreen::demo(req, None, d.stopped)
        } else {
            ScanScreen::real(ctx, req, self.animate)
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
            }
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
                actions: &mut actions,
            };
            match &mut self.screen {
                Screen::Home => home::show(
                    ui,
                    &mut env,
                    self.notice.as_deref(),
                    self.folder_rx.is_some(),
                ),
                Screen::Scan(s) => scan::show(ui, &mut env, s),
                Screen::Explore(e) => e.show(ui, &mut env),
            }
        }
        self.apply(&ctx, actions);
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
