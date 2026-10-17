//! Pantalla de escaneo (PLAN.md §2.4): progreso real, filas de las rutas conocidas con su tamaño a medida que la cola
//! prioritaria las completa, barras vivas por carpeta de primer nivel y "Parar y ver".
//!
//! La clasificación de riesgo y "Limpiar lo seguro" son del incremento 3: las filas son informativas y el botón está
//! deshabilitado con la pista "Disponible en la próxima versión".
//!
//! El modo demostración usa un escaneo SIMULADO sobre el árbol sintético (misma pantalla, sin tocar el disco).

use crate::app::{Action, Env, ScanRequest};
use crate::args;
use crate::clean::{CleanFlow, FlowEvent, SummaryClick, VolumeCtx};
use crate::demo::{self, DEMO_KNOWN_ROWS};
use crate::error::StartError;
use crate::explore::{Explore, ScanInfo};
use crate::icons::Icon;
use crate::theme::{self, Palette};
use crate::widgets::{self, ButtonStyle};
use egui::{CornerRadius, Margin, RichText, Ui, vec2};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tesela_cleaner::{Backend, Basket, CategoryId, Plan};
use tesela_model::{FileType, Tree};
use tesela_scanner::{KnownKind, Progress, ScanHandle, ScanOptions};

/// Cómo acabó (o sigue) un escaneo.
pub enum ScanOutcome {
    Done(Box<Explore>),
    /// No pudo empezar o acabar: error TIPADO (la interfaz lo traduce).
    Failed(StartError),
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

/// Punto de encuentro entre la pantalla y el hilo que arranca el escaneo. Todo pasa bajo un único candado, así que no hay
/// carrera entre "llega el resultado" y "se cancela": o el hilo ve la cancelación antes de entregar (y cancela él mismo lo
/// que acaba de crear), o quien cancela ve el resultado ya entregado (y lo cancela). Nunca queda un escaneo huérfano.
#[derive(Default)]
struct Startup {
    slot: Mutex<Slot>,
    /// Bandera de cancelación del escaneo, creada ANTES de lanzar el hilo de arranque y pasada a `ScanOptions::cancel`:
    /// cancelar (o soltar la pantalla, o "Parar y ver") la activa al instante y el arranque la consulta entre sus fases
    /// (abrir y listar la raíz, resolver las rutas conocidas), en vez de terminar todo el trabajo para descartarlo.
    cancel: Arc<AtomicBool>,
}

/// Hilos de arranque vivos ahora mismo. Cada pantalla cancelada o soltada libera el suyo en cuanto el arranque mira la
/// bandera, así que no se acumulan escaneos activos (las pruebas comprueban que vuelve a 0).
static ACTIVE_STARTUPS: AtomicUsize = AtomicUsize::new(0);

/// Descuenta un hilo de arranque al terminar (también si entra en pánico).
struct ActiveGuard;

impl ActiveGuard {
    fn new() -> ActiveGuard {
        ACTIVE_STARTUPS.fetch_add(1, Ordering::SeqCst);
        ActiveGuard
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        ACTIVE_STARTUPS.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct Slot {
    cancelled: bool,
    result: Option<Result<Real, StartError>>,
}

/// Cancela un escaneo (único sitio que lo hace, para que las pruebas puedan comprobarlo).
fn cancel_handle(h: &ScanHandle) {
    h.cancel();
    #[cfg(test)]
    test_hooks::cancelled(h.progress());
}

enum Kind {
    /// Abriendo la raíz y resolviendo las rutas conocidas (E/S que puede tardar: va en un hilo, no en el de la interfaz).
    Starting(Arc<Startup>),
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
    /// "Parar y ver" pedido antes de que el escaneo haya arrancado.
    stop_pending: bool,
    /// (tarjeta de sitios conocidos, tarjeta del mapa): su contenido no cabe y se desplaza (la barra está a la vista).
    overflow: (bool, bool),
    kind: Kind,
    backend: Arc<dyn Backend>,
    /// La limpieza ("Limpiar lo seguro"): se puede lanzar sin esperar al mapa.
    flow: CleanFlow,
    /// Lo medido de cada categoría segura cuando su fila terminó (con la antigüedad y las protecciones reales).
    measures: HashMap<CategoryId, Measure>,
    /// Navegadores abiertos al terminar el barrido de sitios conocidos.
    browsers_open: Vec<tesela_cleaner::Browser>,
    /// Lo que se limpió durante el escaneo (para actualizar el árbol cuando acabe).
    cleaned: Vec<tesela_cleaner::Removed>,
}

/// Una limpieza lanzada durante el escaneo acaba de quitar cosas del disco: se anotan para el árbol final y se DESCARTAN las
/// medidas de las categorías seguras (eran de antes de limpiar: el titular y "Limpiar lo seguro" no pueden seguir ofreciendo
/// lo que ya no está). `update_measures` vuelve a medir cada fila terminada en el siguiente dibujo.
fn absorb_removed(
    measures: &mut HashMap<CategoryId, Measure>,
    cleaned: &mut Vec<tesela_cleaner::Removed>,
    removed: Vec<tesela_cleaner::Removed>,
) {
    cleaned.extend(removed);
    measures.clear();
}

/// La medida de una categoría segura.
enum Measure {
    Computing(crossbeam_channel::Receiver<Plan>),
    Done(u64),
}

/// Nombre canónico en minúsculas para comparar rutas (formas largas, sin `\\?\`).
fn canon_key(p: &Path) -> Option<String> {
    let c = std::fs::canonicalize(p).ok()?;
    Some(dunce::simplified(&c).to_string_lossy().to_lowercase())
}

/// Arranca el escaneo real y empareja las rutas prioritarias con su clase (hilo auxiliar).
fn start_real(req: &ScanRequest, startup: &Startup) -> Result<Real, StartError> {
    let cancel = &startup.cancel;
    // Fase 0: si la pantalla ya se cerró o se canceló (descartar), no se empieza nada. Con "Parar y ver" (que activa la
    // misma bandera pero no descarta) sí se arranca: el escaneo se construye con lo que se alcance a listar.
    if startup
        .slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .cancelled
    {
        return Err(StartError::Cancelled);
    }
    let mut opts = ScanOptions::new(&req.root);
    opts.cancel = Some(cancel.clone());
    let handle = tesela_scanner::start(opts).map_err(StartError::Scan)?;
    // Fase 2: nombrar las rutas conocidas (E/S: lista carpetas de los navegadores). Si se canceló mientras arrancaba el
    // escaneo, se omite: el resultado se va a descartar (o a mostrar tal cual con "Parar y ver").
    if cancel.load(Ordering::SeqCst) {
        let progress = handle.progress().clone();
        #[cfg(test)]
        test_hooks::started(&req.root, &progress);
        return Ok(Real {
            handle: Some(handle),
            progress,
            kinds: Vec::new(),
        });
    }
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
    #[cfg(test)]
    test_hooks::started(&req.root, &progress);
    Ok(Real {
        handle: Some(handle),
        progress,
        kinds,
    })
}

impl ScanScreen {
    /// Escaneo real de `req.root`. Arrancarlo (abrir y listar la raíz, resolver las rutas prioritarias) es E/S que puede
    /// tardar en un disco frío o de red: se hace en un hilo y la pantalla aparece al instante.
    pub fn real(
        ctx: &egui::Context,
        req: ScanRequest,
        animate: bool,
        backend: Arc<dyn Backend>,
    ) -> ScanScreen {
        // El token de cancelación existe antes que el hilo y viaja en `ScanOptions`.
        let startup = Arc::new(Startup::default());
        let (c, r, s) = (ctx.clone(), req.clone(), startup.clone());
        let guard = ActiveGuard::new();
        std::thread::spawn(move || {
            let _guard = guard;
            let res = start_real(&r, &s);
            {
                let mut slot = s.slot.lock().unwrap_or_else(|e| e.into_inner());
                if slot.cancelled {
                    // Se cerró o canceló la pantalla mientras arrancaba: nadie lo espera, que los hilos terminen.
                    if let Ok(real) = &res
                        && let Some(h) = &real.handle
                    {
                        cancel_handle(h);
                    }
                } else {
                    slot.result = Some(res);
                }
            }
            c.request_repaint();
        });
        ScanScreen {
            req,
            started: Instant::now(),
            discard: false,
            animate,
            stop_pending: false,
            overflow: (false, false),
            kind: Kind::Starting(startup),
            browsers_open: backend.running_browsers(),
            backend,
            flow: CleanFlow::default(),
            measures: HashMap::new(),
            cleaned: Vec::new(),
        }
    }

    /// Escaneo simulado sobre el árbol sintético. `frozen` fija la fracción (capturas y pruebas).
    pub fn demo(
        req: ScanRequest,
        frozen: Option<f32>,
        _stopped: bool,
        backend: Arc<dyn Backend>,
    ) -> ScanScreen {
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
            stop_pending: false,
            overflow: (false, false),
            browsers_open: backend.running_browsers(),
            backend,
            flow: CleanFlow::default(),
            measures: HashMap::new(),
            cleaned: Vec::new(),
            kind: Kind::Demo(Demo {
                tree,
                top,
                files,
                frozen,
                stop: false,
            }),
        }
    }

    /// (tarjeta de sitios conocidos, tarjeta del mapa): ¿se desplaza su contenido porque no cabe? (para los tests)
    #[cfg(test)]
    pub(crate) fn overflow(&self) -> (bool, bool) {
        self.overflow
    }

    /// La bandera de cancelación del arranque, si la pantalla sigue arrancando (para los tests).
    #[cfg(test)]
    fn startup_cancel(&self) -> Option<Arc<AtomicBool>> {
        match &self.kind {
            Kind::Starting(s) => Some(s.cancel.clone()),
            _ => None,
        }
    }

    /// Nombre de lo que se escanea.
    pub fn label(&self) -> &str {
        &self.req.label
    }

    /// Pide cancelar (los hilos terminan solos y el resultado se descarta).
    pub fn cancel(&mut self) {
        self.discard = true;
        match &self.kind {
            Kind::Real(r) => {
                if let Some(h) = &r.handle {
                    cancel_handle(h);
                }
            }
            Kind::Starting(s) => {
                // Primero la bandera: el arranque la ve entre fases sin esperar al candado.
                s.cancel.store(true, Ordering::SeqCst);
                let mut slot = s.slot.lock().unwrap_or_else(|e| e.into_inner());
                slot.cancelled = true;
                // Si el resultado ya llegó pero la pantalla no lo ha recogido, se cancela aquí.
                if let Some(Ok(real)) = &slot.result
                    && let Some(h) = &real.handle
                {
                    cancel_handle(h);
                }
            }
            Kind::Demo(_) => {}
        }
    }

    fn demo_fraction(&self, d: &Demo) -> f32 {
        d.frozen
            .unwrap_or_else(|| (self.started.elapsed().as_secs_f32() / DEMO_SECS).min(1.0))
            .clamp(0.0, 1.0)
    }

    /// ¿Se está ejecutando una limpieza lanzada desde esta pantalla ("Limpiar lo seguro")?
    pub(crate) fn clean_running(&self) -> bool {
        self.flow.is_running()
    }

    /// Anota que se intentó cerrar la ventana durante la ejecución (la ventana de progreso lo explica).
    pub(crate) fn block_close(&mut self) {
        self.flow.block_close();
    }

    /// Avanza el escaneo: si terminó, devuelve el resultado.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<ScanOutcome> {
        let started = self.started;
        // La limpieza que se lance durante el escaneo avanza aquí; mientras haya una ventana de la limpieza abierta o un
        // resumen a la vista, el resultado del escaneo espera (no se le quita la pantalla al usuario a mitad de limpiar).
        self.flow.poll(ctx, &self.backend, false);
        for ev in self.flow.take_events() {
            let FlowEvent::Removed(v) = ev;
            absorb_removed(&mut self.measures, &mut self.cleaned, v);
        }
        let hold = self.flow.busy() || self.flow.in_summary();
        if let Kind::Starting(s) = &self.kind {
            let (result, thread_alive) = {
                let mut slot = s.slot.lock().unwrap_or_else(|e| e.into_inner());
                (slot.result.take(), Arc::strong_count(s) > 1)
            };
            match result {
                Some(Ok(real)) => {
                    if (self.discard || self.stop_pending)
                        && let Some(h) = &real.handle
                    {
                        cancel_handle(h);
                    }
                    self.kind = Kind::Real(real);
                }
                Some(Err(_)) | None if self.discard => return Some(ScanOutcome::Cancelled),
                Some(Err(StartError::Cancelled)) => return Some(ScanOutcome::Cancelled),
                Some(Err(e)) => return Some(ScanOutcome::Failed(e)),
                None if thread_alive => {
                    ctx.request_repaint_after(Duration::from_millis(50));
                    return None;
                }
                None => {
                    return Some(ScanOutcome::Failed(StartError::StartupLost));
                }
            }
        }
        match &mut self.kind {
            Kind::Starting(_) => None,
            Kind::Real(r) => {
                if r.handle.as_ref().is_some_and(ScanHandle::is_finished) && !hold {
                    let handle = r.handle.take()?;
                    let cleaned = std::mem::take(&mut self.cleaned);
                    return Some(match handle.wait() {
                        _ if self.discard => ScanOutcome::Cancelled,
                        Ok(res) => {
                            let mut ex = Explore::from_scan(
                                self.req.clone(),
                                res,
                                self.animate,
                                started.elapsed(),
                                &self.backend,
                            );
                            ex.apply_removed(&cleaned);
                            ScanOutcome::Done(Box::new(ex))
                        }
                        Err(e) => ScanOutcome::Failed(StartError::Scan(e)),
                    });
                }
                ctx.request_repaint_after(Duration::from_millis(50));
                None
            }
            Kind::Demo(d) => {
                let done = d.frozen.is_none()
                    && (d.stop || self.started.elapsed().as_secs_f32() >= DEMO_SECS)
                    && !hold;
                if done && !self.discard {
                    let variant = if d.stop {
                        demo::Variant::Stopped
                    } else {
                        demo::Variant::Complete
                    };
                    let mut ex = Explore::demo(
                        self.req.clone(),
                        variant,
                        self.animate,
                        self.backend.clone(),
                    );
                    ex.apply_removed(&std::mem::take(&mut self.cleaned));
                    return Some(ScanOutcome::Done(Box::new(ex)));
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
            Kind::Starting(s) => {
                // "Parar y ver" antes de que arranque: la misma bandera hace que el escaneo se construya con lo recibido.
                s.cancel.store(true, Ordering::SeqCst);
                self.stop_pending = true;
            }
            Kind::Real(r) => {
                if let Some(h) = &r.handle {
                    cancel_handle(h);
                }
            }
            Kind::Demo(d) => d.stop = true,
        }
    }
}

impl Drop for ScanScreen {
    /// Cerrar la pantalla (volver al inicio, lanzar otro escaneo) cancela el escaneo en marcha: los hilos no se quedan
    /// trabajando para nadie.
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Una fila de "sitios conocidos".
struct Row {
    name: String,
    bytes: u64,
    done: bool,
    /// Datos del usuario (Descargas, Papelera): riesgo "Revisar". Se enseñan, pero NO cuentan en la cifra grande de
    /// "sitios conocidos" (PLAN.md §2.4: el titular suma solo lo seguro) y llevan la etiqueta "Revisar".
    review: bool,
    /// Informes de errores o volcados de bloqueo (también "Revisar"): llevan su propia frase de "no cuentan".
    diag: bool,
    /// Categoría segura de la fila (la que mide el `cleaner` al terminar), si la tiene.
    safe: Option<CategoryId>,
    /// El navegador de esta caché está abierto: la fila no cuenta y dice "Cierra X para liberar hasta N".
    blocked: Option<&'static str>,
}

/// La categoría segura de una clase de sitio conocido (Descargas y la Papelera no lo son: son datos del usuario).
fn safe_category(k: Option<KnownKind>) -> Option<CategoryId> {
    match k? {
        KnownKind::UserTemp => Some(CategoryId::UserTemp),
        KnownKind::WindowsTemp => Some(CategoryId::WindowsTemp),
        KnownKind::ChromeCache => Some(CategoryId::ChromeCache),
        KnownKind::EdgeCache => Some(CategoryId::EdgeCache),
        KnownKind::FirefoxCache => Some(CategoryId::FirefoxCache),
        // Datos del usuario o de diagnóstico: riesgo "Revisar", nunca la categoría "segura" de una fila.
        KnownKind::Downloads
        | KnownKind::RecycleBin
        | KnownKind::WerReports
        | KnownKind::CrashDumps => None,
    }
}

fn kind_of_key(key: &str) -> Option<KnownKind> {
    Some(match key {
        "known-user-temp" => KnownKind::UserTemp,
        "known-windows-temp" => KnownKind::WindowsTemp,
        "known-chrome-cache" => KnownKind::ChromeCache,
        "known-edge-cache" => KnownKind::EdgeCache,
        "known-firefox-cache" => KnownKind::FirefoxCache,
        "known-downloads" => KnownKind::Downloads,
        "known-recycle-bin" => KnownKind::RecycleBin,
        "known-wer-reports" => KnownKind::WerReports,
        "known-crash-dumps" => KnownKind::CrashDumps,
        _ => return None,
    })
}

/// ¿Es una clase de sitio conocido con datos del usuario o de diagnóstico (riesgo "Revisar", nunca "seguro")?
fn kind_is_review(k: Option<KnownKind>) -> bool {
    matches!(
        k,
        Some(
            KnownKind::Downloads
                | KnownKind::RecycleBin
                | KnownKind::WerReports
                | KnownKind::CrashDumps
        )
    )
}

/// Junta las rutas prioritarias de la misma clase en una sola fila (suma de bytes; terminada si todas lo están), en el
/// orden de la primera aparición de cada clase.
fn group_by_kind(
    items: impl IntoIterator<Item = (Option<KnownKind>, u64, bool)>,
) -> Vec<(Option<KnownKind>, u64, bool)> {
    let mut out: Vec<(Option<KnownKind>, u64, bool)> = Vec::new();
    for (k, bytes, done) in items {
        match out.iter_mut().find(|r| r.0 == k) {
            Some(r) => {
                r.1 = r.1.saturating_add(bytes);
                r.2 &= done;
            }
            None => out.push((k, bytes, done)),
        }
    }
    out
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
        Some(KnownKind::WerReports) => "known-wer-reports",
        Some(KnownKind::CrashDumps) => "known-crash-dumps",
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
    /// Lo que se puede liberar sin riesgo (suma de lo medido por el `cleaner` de las filas seguras terminadas).
    safe_sum: u64,
    /// Todas las filas terminaron y ninguna medida está pendiente: "Limpiar lo seguro" se puede pulsar.
    can_clean: bool,
}

fn view_of(s: &ScanScreen, env: &Env) -> View {
    let total = s
        .req
        .drive
        .as_ref()
        .map(|d| d.total.saturating_sub(d.free))
        .filter(|t| *t > 0);
    match &s.kind {
        Kind::Starting(_) => View {
            files: 0,
            bytes: 0,
            total,
            errors: 0,
            rows: Vec::new(),
            bars: Vec::new(),
            finished: false,
            fraction: total.map(|_| 0.0),
            safe_sum: 0,
            can_clean: false,
        },
        Kind::Real(r) => {
            let p = &r.progress;
            let load = std::sync::atomic::Ordering::Relaxed;
            let bytes = p.alloc.load(load);
            // Un navegador tiene varias carpetas de caché (por perfil): una sola fila por clase.
            let rows = group_by_kind(
                p.priority_targets()
                    .iter()
                    .zip(&r.kinds)
                    .map(|(t, k)| (*k, t.alloc.load(load), t.is_done())),
            )
            .into_iter()
            .map(|(k, bytes, done)| Row {
                name: env.tr.t(kind_key(k)),
                bytes,
                done,
                review: kind_is_review(k),
                diag: matches!(k, Some(KnownKind::WerReports | KnownKind::CrashDumps)),
                safe: safe_category(k),
                blocked: None,
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
                safe_sum: 0,
                can_clean: false,
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
                    review: kind_is_review(kind_of_key(key)),
                    diag: false,
                    safe: safe_category(kind_of_key(key)),
                    blocked: None,
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
                safe_sum: 0,
                can_clean: false,
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
    // El resumen de una limpieza lanzada desde aquí ocupa la ventana; al volver se sigue con el escaneo.
    if s.flow.in_summary() {
        match s.flow.show_summary(ui, env) {
            Some(SummaryClick::EmptyNow) => {
                s.flow.close_summary();
                s.flow.basket.clear();
                s.flow.basket.set_empty_bin(true);
                let vol = s.volume_ctx();
                s.flow.start(ui.ctx(), env.backend, vol, false);
            }
            Some(SummaryClick::OpenBin) => crate::platform::open_recycle_bin(),
            _ => {}
        }
        return;
    }
    let mut v = view_of(s, env);
    s.update_measures(ui.ctx(), &mut v);
    let elapsed = s.started.elapsed().as_secs();
    let mut stop = false;
    let mut cancel = false;
    let mut clean_click = false;
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(pal.bg)
                .inner_margin(Margin::symmetric(40, 28)),
        )
        .show(ui, |ui| {
            // Título y botones en una fila; las cifras, debajo (en una ventana estrecha no caben las tres cosas).
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(env.tr.t("scan-title"))
                        .font(theme::font_semibold(26.0))
                        .color(pal.text),
                );
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
            {
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
            }
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
                    // Sin total conocido (una carpeta): un tramo que va y viene; con "reducir animaciones", un tramo
                    // fijo y sin repintado (el progreso REAL de los contadores se repinta por su cuenta cada 50 ms).
                    let t = ui.input(|i| i.time) as f32;
                    let r = widgets::indeterminate_segment(bar_rect, t, env.animate);
                    ui.painter()
                        .rect_filled(r, CornerRadius::same(4), pal.accent);
                    if env.animate {
                        ui.ctx().request_repaint_after(Duration::from_millis(33));
                    }
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

            // Cada tarjeta se desplaza si su contenido no cabe (ventana mínima): nada se corta sin barra a la vista.
            let (mut left_over, mut right_over) = (false, false);
            ui.scope_builder(egui::UiBuilder::new().max_rect(left_rect), |ui| {
                card_frame(&pal).show(ui, |ui| {
                    ui.set_min_size(vec2(left_rect.width() - 58.0, left_rect.height() - 50.0));
                    let out = egui::ScrollArea::vertical()
                        .id_salt("scan-known-scroll")
                        .auto_shrink([false, false])
                        .max_height(left_rect.height() - 50.0)
                        .show(ui, |ui| known_places(ui, env, &v, &mut clean_click));
                    left_over = out.content_size.y > out.inner_rect.height() + 0.5;
                });
            });
            ui.scope_builder(egui::UiBuilder::new().max_rect(right_rect), |ui| {
                card_frame(&pal).show(ui, |ui| {
                    ui.set_min_size(vec2(right_rect.width() - 58.0, right_rect.height() - 50.0));
                    let out = egui::ScrollArea::vertical()
                        .id_salt("scan-map-scroll")
                        .auto_shrink([false, false])
                        .max_height(right_rect.height() - 50.0)
                        .show(ui, |ui| live_map(ui, env, &v));
                    right_over = out.content_size.y > out.inner_rect.height() + 0.5;
                });
            });
            s.overflow = (left_over, right_over);
        });
    if stop {
        s.stop();
    }
    if cancel {
        env.actions.push(Action::GoHome);
    }
    if clean_click {
        s.start_safe_clean(ui.ctx(), env, &v);
    }
    let vol = s.volume_ctx();
    let used = vol.volume.map(|(t, f)| t.saturating_sub(f));
    s.flow.show_modals(ui.ctx(), env, vol, used);
}

impl ScanScreen {
    fn volume_ctx(&self) -> VolumeCtx {
        let d = self.req.drive.as_ref();
        VolumeCtx {
            drive: d.map(|d| d.letter),
            volume: d.filter(|d| d.volume_scope).map(|d| (d.total, d.free)),
        }
    }

    /// Recoge las medidas terminadas, lanza la de cada fila segura que acaba de terminar y calcula el titular.
    fn update_measures(&mut self, ctx: &egui::Context, v: &mut View) {
        let mut finished = Vec::new();
        for (id, m) in &self.measures {
            if let Measure::Computing(rx) = m
                && let Ok(plan) = rx.try_recv()
            {
                finished.push((*id, plan.permanent_bytes()));
            }
        }
        for (id, b) in finished {
            self.measures.insert(id, Measure::Done(b));
        }
        for r in &v.rows {
            if let (true, Some(cat)) = (r.done, r.safe)
                && !self.measures.contains_key(&cat)
            {
                let (tx, rx) = crossbeam_channel::bounded(1);
                let (b, c2) = (self.backend.clone(), ctx.clone());
                let mut basket = Basket::new();
                basket.set_category(cat, true);
                std::thread::spawn(move || {
                    let plan = b.approve(
                        &basket,
                        &Default::default(),
                        &Arc::new(AtomicBool::new(false)),
                    );
                    let _ = tx.send(plan);
                    c2.request_repaint();
                });
                self.measures.insert(cat, Measure::Computing(rx));
            }
        }
        let (mut sum, mut pending) = (0u64, false);
        for r in v.rows.iter_mut() {
            let Some(cat) = r.safe else { continue };
            match self.measures.get(&cat) {
                Some(Measure::Done(b)) => {
                    r.bytes = *b;
                    let blocked = tesela_cleaner::Browser::from_category(cat)
                        .filter(|br| self.browsers_open.contains(br));
                    r.blocked = blocked.map(tesela_cleaner::Browser::name);
                    if blocked.is_none() {
                        sum += *b;
                    }
                }
                Some(Measure::Computing(_)) => pending = true,
                None => {}
            }
        }
        if pending {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
        v.safe_sum = sum;
        v.can_clean = !v.rows.is_empty()
            && v.rows.iter().all(|r| r.done)
            && !pending
            && sum > 0
            && !self.flow.busy();
    }

    /// "Limpiar lo seguro": la cesta son las categorías seguras que se pueden limpiar ahora, y se pasa a la confirmación.
    fn start_safe_clean(&mut self, ctx: &egui::Context, env: &Env, v: &View) {
        self.flow.basket.clear();
        for r in &v.rows {
            if let Some(cat) = r.safe
                && r.blocked.is_none()
                && r.bytes > 0
                && matches!(self.measures.get(&cat), Some(Measure::Done(_)))
            {
                self.flow.basket.set_category(cat, true);
            }
        }
        let vol = self.volume_ctx();
        self.flow.start(ctx, env.backend, vol, false);
    }
}

/// Ancho del botón "Limpiar lo seguro" que se reserva a su derecha cuando la tarjeta es ancha.
const CLEAN_BUTTON_W: f32 = 190.0;

fn known_places(ui: &mut Ui, env: &mut Env, v: &View, clean_click: &mut bool) {
    let pal = env.pal;
    // El texto (que se ajusta al ancho) y el botón NUNCA se pisan: con sitio, el botón va a la derecha en su propia
    // columna reservada; si no (tarjeta estrecha, ventana mínima), el botón va debajo del texto.
    let side_by_side = ui.available_width() >= 560.0;
    let text_w = if side_by_side {
        ui.available_width() - CLEAN_BUTTON_W - 16.0
    } else {
        ui.available_width()
    };
    let text = |ui: &mut Ui, env: &Env| {
        ui.label(
            RichText::new(env.tr.t("scan-known-title"))
                .size(13.0)
                .color(pal.text_muted),
        );
        // Solo lo seguro y medido por el `cleaner` (con la antigüedad y las protecciones reales): Descargas y la Papelera
        // se muestran pero no cuentan aquí.
        let all_done = !v.rows.is_empty() && v.rows.iter().all(|r| r.done);
        let headline = if v.safe_sum == 0 && !all_done {
            env.tr.t("scan-measuring")
        } else if v.safe_sum > 0 {
            env.tr
                .ta("scan-known-sum", args!["size" => env.tr.size(v.safe_sum)])
        } else {
            env.tr.t("scan-nothing-safe")
        };
        ui.label(
            RichText::new(headline)
                .font(theme::font_semibold(30.0))
                .color(pal.text),
        );
        ui.label(
            RichText::new(env.tr.t("scan-known-note"))
                .size(13.0)
                .color(pal.text_muted),
        );
        if v.rows.iter().any(|r| r.review && !r.diag) {
            ui.label(
                RichText::new(env.tr.t("scan-known-excluded"))
                    .size(13.0)
                    .color(pal.text_muted),
            );
        }
        if v.rows.iter().any(|r| r.diag) {
            ui.label(
                RichText::new(env.tr.t("scan-known-excluded-diag"))
                    .size(13.0)
                    .color(pal.text_muted),
            );
        }
    };
    let mut button = |ui: &mut Ui, env: &mut Env| {
        let label = env.tr.t("scan-clean-safe");
        let r = widgets::button(
            ui,
            env,
            ButtonStyle::Primary,
            Some(Icon::ShieldCheckmark),
            &label,
            v.can_clean,
        );
        if !v.can_clean {
            r.clone().on_hover_text(env.tr.t("scan-clean-wait"));
        }
        if r.clicked() {
            *clean_click = true;
        }
    };
    if side_by_side {
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                vec2(text_w, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(text_w);
                    text(ui, env);
                },
            );
            ui.allocate_ui_with_layout(
                vec2(CLEAN_BUTTON_W, 0.0),
                egui::Layout::top_down(egui::Align::Max),
                |ui| button(ui, env),
            );
        });
    } else {
        ui.vertical(|ui| {
            ui.set_width(text_w);
            text(ui, env);
            ui.add_space(8.0);
            button(ui, env);
        });
    }
    ui.add_space(14.0);
    let done = v.rows.iter().filter(|r| r.done).count();
    if v.rows.is_empty() {
        ui.label(RichText::new(env.tr.t("scan-no-known")).color(pal.text_muted));
        return;
    }
    ui.horizontal(|ui| {
        if done < v.rows.len() {
            widgets::busy(ui, env.animate, 16.0, pal.accent);
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
                widgets::busy(ui, env.animate, 16.0, pal.accent);
            }
            ui.label(RichText::new(&r.name).size(15.0).color(pal.text));
            if r.review {
                ui.label(
                    RichText::new(env.tr.t("risk-review-tag"))
                        .size(12.0)
                        .color(pal.review),
                );
            }
            if let Some(b) = r.blocked {
                ui.label(
                    RichText::new(env.tr.ta(
                        "scan-row-blocked",
                        args!["name" => b, "size" => env.tr.size(r.bytes)],
                    ))
                    .size(12.0)
                    .color(pal.review),
                );
            }
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

/// ¿Son estimados los tamaños de este escaneo? Sí si se usó el respaldo `FindFirstFileExW` en todo el escaneo
/// (`FindFallback`) o en algún directorio (`Mixed`): ahí `alloc` es un redondeo del EOF al clúster, no hay `FileId` para
/// deduplicar enlaces duros y los directorios miden 0. Solo `Extended` es exacto.
pub(crate) fn is_approximate(api: tesela_scanner::ApiUsed) -> bool {
    use tesela_scanner::ApiUsed;
    match api {
        ApiUsed::Extended => false,
        ApiUsed::FindFallback | ApiUsed::Mixed => true,
    }
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
        volume_scope: summary.volume.is_none_or(|v| v.volume_scope),
        cancelled: summary.cancelled,
        approximate: is_approximate(summary.api),
        dir_errors: summary.errors(),
        files,
        elapsed,
        finished_at: Instant::now(),
    }
}

/// Registro de escaneos arrancados y cancelados, para que las pruebas comprueben que ninguno queda huérfano.
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use tesela_scanner::Progress;

    static STARTED: Mutex<Vec<(PathBuf, Arc<Progress>)>> = Mutex::new(Vec::new());
    static CANCELLED: Mutex<Vec<Arc<Progress>>> = Mutex::new(Vec::new());

    pub fn started(root: &Path, p: &Arc<Progress>) {
        STARTED
            .lock()
            .unwrap()
            .push((root.to_path_buf(), p.clone()));
    }
    pub fn cancelled(p: &Arc<Progress>) {
        CANCELLED.lock().unwrap().push(p.clone());
    }
    /// Hilos de arranque vivos ahora mismo (en todo el proceso de pruebas).
    pub fn active_startups() -> usize {
        super::ACTIVE_STARTUPS.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// (escaneos arrancados con esa raíz, de ellos cancelados)
    pub fn counts(root: &Path) -> (usize, usize) {
        let started: Vec<_> = STARTED
            .lock()
            .unwrap()
            .iter()
            .filter(|(r, _)| r == root)
            .map(|(_, p)| p.clone())
            .collect();
        let cancelled = CANCELLED.lock().unwrap();
        let n = started
            .iter()
            .filter(|p| cancelled.iter().any(|c| Arc::ptr_eq(c, p)))
            .count();
        (started.len(), n)
    }
}

#[cfg(test)]
mod removed_tests {
    use super::*;

    #[test]
    fn tras_limpiar_durante_el_escaneo_se_descartan_las_medidas_viejas() {
        let mut measures = HashMap::new();
        measures.insert(CategoryId::UserTemp, Measure::Done(5_000));
        let (_tx, rx) = crossbeam_channel::bounded(1);
        measures.insert(CategoryId::WindowsTemp, Measure::Computing(rx));
        let mut cleaned = Vec::new();
        let r = tesela_cleaner::Removed {
            path: "x".into(),
            is_dir: false,
            removed: true,
            bytes: 5_000,
            logical: 5_000,
            files: 1,
        };
        absorb_removed(&mut measures, &mut cleaned, vec![r.clone()]);
        assert!(
            measures.is_empty(),
            "las medidas de antes de limpiar no valen"
        );
        assert_eq!(cleaned, vec![r.clone()]);
        // Se acumulan entre limpiezas.
        absorb_removed(&mut measures, &mut cleaned, vec![r.clone()]);
        assert_eq!(cleaned.len(), 2);
    }
}

#[cfg(test)]
mod group_tests {
    use super::*;

    fn test_backend() -> Arc<dyn Backend> {
        Arc::new(crate::demo_backend::DemoBackend::new(Duration::ZERO))
    }

    /// Carpeta temporal propia (solo se borra lo creado aquí, bajo la carpeta temporal).
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Temp {
            let p = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(p.join("a")).unwrap();
            std::fs::write(p.join("a").join("f.bin"), vec![1u8; 5000]).unwrap();
            Temp(p)
        }
        fn req(&self) -> ScanRequest {
            ScanRequest {
                root: self.0.clone(),
                label: "t".into(),
                drive: None,
            }
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Espera a que no quede ningún hilo de arranque vivo.
    fn wait_no_active_startups() {
        let t0 = Instant::now();
        while test_hooks::active_startups() != 0 {
            assert!(t0.elapsed().as_secs() < 30, "hilos de arranque acumulados");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn el_token_de_cancelacion_nace_con_la_pantalla_y_lo_activan_cancelar_parar_y_soltar() {
        let t = Temp::new();
        let ctx = egui::Context::default();
        // Cancelar activa la bandera al instante, sin esperar al hilo de arranque.
        let mut s = ScanScreen::real(&ctx, t.req(), false, test_backend());
        let flag = s.startup_cancel().expect("arrancando");
        s.cancel();
        assert!(flag.load(Ordering::SeqCst), "cancelar activa el token");
        // "Parar y ver" también (el escaneo se construye con lo recibido), sin descartar.
        let mut s2 = ScanScreen::real(&ctx, t.req(), false, test_backend());
        let flag2 = s2.startup_cancel().unwrap();
        s2.stop();
        assert!(flag2.load(Ordering::SeqCst), "parar activa el token");
        assert!(!s2.discard);
        // Soltar la pantalla (Drop) también.
        let s3 = ScanScreen::real(&ctx, t.req(), false, test_backend());
        let flag3 = s3.startup_cancel().unwrap();
        drop(s3);
        assert!(flag3.load(Ordering::SeqCst), "soltar activa el token");
        // Los hilos de arranque terminan y no se acumulan.
        drop(s);
        drop(s2);
        wait_no_active_startups();
    }

    #[test]
    fn muchos_arranques_cancelados_no_acumulan_hilos_ni_escaneos_activos() {
        let t = Temp::new();
        let ctx = egui::Context::default();
        for i in 0..60u64 {
            let s = ScanScreen::real(&ctx, t.req(), false, test_backend());
            if i % 3 == 0 {
                std::thread::sleep(Duration::from_micros(i * 100));
            }
            drop(s);
        }
        wait_no_active_startups();
        let (started, cancelled) = test_hooks::counts(&t.0);
        assert_eq!(started, cancelled, "ningún escaneo queda sin cancelar");
        assert!(started <= 60);
    }

    #[test]
    fn descargas_y_papelera_son_datos_del_usuario_y_no_cuentan_como_seguros() {
        assert!(kind_is_review(Some(KnownKind::Downloads)));
        assert!(kind_is_review(Some(KnownKind::RecycleBin)));
        // Informes de errores y volcados: también "Revisar", sin categoría segura.
        for k in [KnownKind::WerReports, KnownKind::CrashDumps] {
            assert!(kind_is_review(Some(k)), "{k:?}");
            assert_eq!(safe_category(Some(k)), None, "{k:?}");
            assert_eq!(kind_of_key(kind_key(Some(k))), Some(k), "{k:?}");
        }
        for k in [
            KnownKind::UserTemp,
            KnownKind::WindowsTemp,
            KnownKind::ChromeCache,
            KnownKind::EdgeCache,
            KnownKind::FirefoxCache,
        ] {
            assert!(!kind_is_review(Some(k)), "{k:?}");
        }
        assert!(!kind_is_review(None));
    }

    #[test]
    fn aproximado_con_el_respaldo_completo_o_parcial_y_exacto_solo_con_la_api_extendida() {
        use tesela_scanner::ApiUsed;
        assert!(!is_approximate(ApiUsed::Extended));
        assert!(is_approximate(ApiUsed::FindFallback));
        assert!(is_approximate(ApiUsed::Mixed));
    }

    #[test]
    fn las_carpetas_de_cache_de_un_navegador_son_una_sola_fila() {
        let rows = group_by_kind([
            (Some(KnownKind::UserTemp), 10, true),
            (Some(KnownKind::ChromeCache), 5, true),
            (Some(KnownKind::ChromeCache), 7, false),
            (Some(KnownKind::FirefoxCache), 1, true),
            (Some(KnownKind::ChromeCache), 3, true),
            (None, 2, true),
        ]);
        assert_eq!(
            rows,
            vec![
                (Some(KnownKind::UserTemp), 10, true),
                (Some(KnownKind::ChromeCache), 15, false),
                (Some(KnownKind::FirefoxCache), 1, true),
                (None, 2, true),
            ]
        );
        assert!(group_by_kind([]).is_empty());
    }
}
