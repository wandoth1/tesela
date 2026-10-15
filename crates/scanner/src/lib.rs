//! Escáner paralelo de volúmenes NTFS/ReFS para Windows (PLAN.md §3.3). SOLO LECTURA: nunca escribe ni borra nada.
//!
//! * Enumeración con `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)` (tamaño lógico, `AllocationSize` y
//!   `FileId` por lotes), con `FindFirstFileExW` de respaldo (alloc estimado, sin deduplicación de enlaces duros y sin
//!   cola prioritaria porque no hay `FileId` para adoptar).
//! * Work-stealing con `crossbeam-deque`, una tarea por directorio; un `Batch` por directorio hacia un hilo colector
//!   que alimenta el `TreeBuilder` de `tesela-model`.
//! * Cola prioritaria con las rutas conocidas (temporales, cachés, Papelera del usuario, Descargas) y adopción por
//!   `FileId` de la cadena de ancestros.
//! * Los reparse points no se siguen; los placeholders de la nube llevan el flag `CLOUD`; rutas `\\?\`.
//! * Cancelación ("Parar y ver"): se construye el árbol con lo recibido y los directorios no recorridos quedan
//!   `INCOMPLETE`.
//!
//! ```no_run
//! use tesela_scanner::{ScanOptions, scan};
//! let result = scan(ScanOptions::new(r"C:\")).unwrap();
//! println!("{} archivos, {} bytes en disco", result.tree.stats().files, result.tree.node(0).alloc);
//! ```

mod classify;
mod known;
mod priority;
mod progress;
mod sys;
mod walker;

pub use known::{KnownKind, current_user_sid, downloads_dir, known_paths, known_paths_labeled};
pub use progress::{PriorityTarget, Progress, TopLevel};

use crossbeam_deque::{Stealer, Worker};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tesela_model::{Batch, BuildError, EntryData, FinishOptions, Tree, TreeBuilder, flags};
use walker::{NO_PRIO, Shared, Task, WorkerOut, WorkerState};

/// Tope de hilos por defecto: en las mediciones de la fase 0 la mejora se satura entre 8 y 16 hilos
/// (docs/fase-0-conclusiones.md).
pub const MAX_DEFAULT_THREADS: usize = 16;

/// Hilos por defecto: `min(16, 2 × núcleos lógicos disponibles)`.
pub fn default_threads() -> usize {
    let cores = std::thread::available_parallelism().map_or(2, std::num::NonZero::get);
    MAX_DEFAULT_THREADS.min(2 * cores)
}

/// Opciones de un escaneo.
#[derive(Clone, Debug)]
pub struct ScanOptions {
    /// Carpeta o volumen a escanear.
    pub root: PathBuf,
    /// Hilos de escaneo (por defecto [`default_threads`]).
    pub threads: usize,
    /// Usar la cola prioritaria con las rutas conocidas (por defecto sí). Solo afecta a las que cuelgan de `root`.
    pub known_paths: bool,
    /// Sustituye por completo la lista de rutas prioritarias (ignora `known_paths`). Para tests y herramientas.
    pub priority_paths: Option<Vec<PathBuf>>,
    /// Forzar `FindFirstFileExW` en todo el escaneo (para probar el respaldo).
    pub force_find: bool,
    /// Deduplicar enlaces duros por `FileId` (por defecto sí; con `FindFirstFileExW` nunca hay `FileId`).
    pub dedup_hard_links: bool,
    /// Calcular el "espacio no explicado" en un escaneo terminado de un volumen completo (por defecto sí).
    pub volume_gap: bool,
    /// "Ahora" en segundos Unix para los tramos de antigüedad (por defecto, el reloj del sistema).
    pub now_unix: Option<u32>,
    /// Bandera de cancelación externa (por ejemplo, la de un manejador de Ctrl+C). Si ya está activada al empezar, solo
    /// se lista la raíz. [`ScanHandle::cancel`] activa la misma bandera.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Solo para tests y herramientas: sustituye el espacio del volumen y trata la ruta como un volumen completo,
    /// lo que permite probar de punta a punta el cálculo del "espacio no explicado" sobre un árbol sintético.
    pub volume_override: Option<VolumeSpace>,
}

impl ScanOptions {
    /// Opciones por defecto para escanear `root`.
    pub fn new(root: impl Into<PathBuf>) -> ScanOptions {
        ScanOptions {
            root: root.into(),
            threads: default_threads(),
            known_paths: true,
            priority_paths: None,
            force_find: false,
            dedup_hard_links: true,
            volume_gap: true,
            now_unix: None,
            cancel: None,
            volume_override: None,
        }
    }
}

/// Errores de un escaneo.
#[derive(Debug)]
pub enum ScanError {
    /// La raíz no existe o no se puede abrir.
    Root(std::io::Error),
    /// La raíz no es un directorio.
    NotADirectory(PathBuf),
    /// Algún hilo entró en pánico (es un error del programa).
    Panicked,
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::Root(e) => write!(f, "no se puede abrir la ruta a escanear: {e}"),
            ScanError::NotADirectory(_) => write!(f, "la ruta a escanear no es un directorio"),
            ScanError::Panicked => write!(f, "un hilo del escáner entró en pánico"),
        }
    }
}

impl std::error::Error for ScanError {}

/// API de enumeración usada.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiUsed {
    /// `FileIdExtdDirectoryInfo` en todos los directorios.
    Extended,
    /// `FindFirstFileExW` en todo el escaneo (alloc estimado, sin deduplicación ni cola prioritaria).
    FindFallback,
    /// La extendida, salvo en algún directorio donde falló por no estar soportada.
    Mixed,
}

/// Espacio del volumen de la ruta escaneada.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeSpace {
    /// Bytes totales.
    pub total: u64,
    /// Bytes libres (`lpTotalNumberOfFreeBytes`).
    pub free: u64,
}

/// Resultado de una ruta prioritaria.
#[derive(Clone, Debug)]
pub struct PriorityResult {
    /// Ruta canónica.
    pub path: PathBuf,
    /// ¿Se terminó de recorrer entera?
    pub done: bool,
    /// Bytes en disco vistos (sin deduplicar enlaces duros).
    pub alloc: u64,
    /// Archivos vistos.
    pub files: u64,
}

/// Cifras del escaneo que no están en el árbol.
#[derive(Clone, Debug)]
pub struct ScanSummary {
    /// Ruta escaneada tal como se muestra (sin `\\?\` si no hace falta).
    pub root: PathBuf,
    /// Se canceló ("Parar y ver"): el árbol tiene directorios `INCOMPLETE` y no hay espacio no explicado.
    pub cancelled: bool,
    /// Tiempo desde el inicio hasta que acabó la enumeración (incluye listar la raíz y resolver rutas prioritarias).
    pub elapsed_scan: Duration,
    /// Tiempo de `TreeBuilder::finish`.
    pub elapsed_build: Duration,
    /// Hilos usados.
    pub threads: usize,
    /// API de enumeración.
    pub api: ApiUsed,
    /// Sistema de archivos del volumen (`NTFS`, `ReFS`, `exFAT`...).
    pub fs_name: String,
    /// Directorios que no se pudieron listar, por código de error de Windows.
    pub errors_by_code: BTreeMap<u32, u64>,
    /// Primeros directorios con error (código y ruta), para diagnóstico.
    pub error_samples: Vec<(u32, PathBuf)>,
    /// Espacio del volumen (si Windows lo da).
    pub volume: Option<VolumeSpace>,
    /// La ruta escaneada es la raíz de una unidad.
    pub full_volume: bool,
    /// Rutas prioritarias recorridas.
    pub priority: Vec<PriorityResult>,
    /// Se agotaron los ids provisionales (más de 4 000 millones de entradas): el árbol está incompleto.
    pub ids_exhausted: bool,
    /// Error del builder (no debería ocurrir: significaría más de 4 000 M de entradas o 4 GiB de nombres).
    pub build_error: Option<BuildError>,
}

impl ScanSummary {
    /// Total de directorios con error.
    pub fn errors(&self) -> u64 {
        self.errors_by_code.values().sum()
    }
}

/// Árbol y cifras de un escaneo.
pub struct ScanResult {
    /// El árbol final.
    pub tree: Tree,
    /// Cifras que no están en el árbol.
    pub summary: ScanSummary,
}

/// Escaneo en marcha.
pub struct ScanHandle {
    progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
    join: JoinHandle<ScanResult>,
}

impl ScanHandle {
    /// Contadores en vivo (atómicos).
    pub fn progress(&self) -> &Arc<Progress> {
        &self.progress
    }

    /// Pide parar ("Parar y ver"): los hilos terminan su directorio actual y se construye el árbol con lo recibido.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// ¿Ha terminado ya el hilo coordinador?
    pub fn is_finished(&self) -> bool {
        self.join.is_finished()
    }

    /// Espera al resultado.
    pub fn wait(self) -> Result<ScanResult, ScanError> {
        self.join.join().map_err(|_| ScanError::Panicked)
    }
}

/// Escanea y espera al resultado.
pub fn scan(opts: ScanOptions) -> Result<ScanResult, ScanError> {
    start(opts)?.wait()
}

fn io_err(code: u32) -> std::io::Error {
    std::io::Error::from_raw_os_error(code as i32)
}

/// Letra de unidad si `p` es exactamente la raíz de una unidad (`C:\` o `\\?\C:\`).
fn drive_root(p: &Path) -> Option<PathBuf> {
    let mut it = p.components();
    let Some(Component::Prefix(prefix)) = it.next() else {
        return None;
    };
    let letter = match prefix.kind() {
        Prefix::Disk(l) | Prefix::VerbatimDisk(l) => l,
        _ => return None,
    };
    (it.next() == Some(Component::RootDir) && it.next().is_none())
        .then(|| PathBuf::from(format!("{}:\\", letter as char)))
}

/// Raíz de la unidad (por letra) a la que pertenece `p`, aunque `p` no sea la raíz.
fn drive_of(p: &Path) -> Option<PathBuf> {
    match p.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(l) | Prefix::VerbatimDisk(l) => {
                Some(PathBuf::from(format!("{}:\\", l as char)))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Arranca un escaneo. La preparación (abrir y listar la raíz, resolver las rutas prioritarias) es síncrona, así que
/// los errores de la raíz se devuelven aquí; el resto corre en hilos y se sigue con [`ScanHandle`].
pub fn start(opts: ScanOptions) -> Result<ScanHandle, ScanError> {
    let t0 = Instant::now();
    let root = std::fs::canonicalize(&opts.root).map_err(ScanError::Root)?;
    if !root.is_dir() {
        return Err(ScanError::NotADirectory(root));
    }
    let root_w = sys::wide(&root);
    let display_root = dunce::simplified(&root).to_path_buf();
    let full_volume = drive_root(&root).is_some() || opts.volume_override.is_some();

    let attr_h = sys::open_dir(&root_w, sys::FILE_READ_ATTRIBUTES, false)
        .map_err(|c| ScanError::Root(io_err(c)))?;
    let root_meta = sys::dir_meta(&attr_h).map_err(|c| ScanError::Root(io_err(c)))?;
    let fs_name = sys::fs_name(&attr_h).unwrap_or_default();
    drop(attr_h);
    // Solo NTFS y ReFS tienen FileId estables y enlaces duros: en el resto no se guarda el FileId.
    let store_ids = fs_name.eq_ignore_ascii_case("NTFS") || fs_name.eq_ignore_ascii_case("ReFS");
    let cluster = sys::cluster_bytes(&root_w).unwrap_or(4096);

    let progress = Arc::new(Progress::default());
    let cancel = opts.cancel.clone().unwrap_or_default();
    let sh = Arc::new(Shared::new(
        cancel.clone(),
        progress.clone(),
        cluster,
        store_ids,
    ));
    sh.find_mode.store(opts.force_find, Ordering::Relaxed);
    let (tx, rx) = crossbeam_channel::unbounded::<Batch>();

    // Nodo raíz (id 0) y listado de la raíz, en este hilo.
    let root_id = sh.reserve_ids(1);
    debug_assert_eq!(root_id, 0);
    let mut root_batch = Batch::detached(root_id);
    root_batch.push_utf16(
        0,
        &sys::wide(&display_root),
        EntryData {
            flags: classify::entry_flags(root_meta.attrs, 0) | flags::DIR,
            alloc: root_meta.alloc,
            logical: root_meta.logical,
            mtime: classify::filetime_to_unix(root_meta.mtime_filetime),
            file_id: 0,
        },
    );
    tx.send(root_batch).expect("el receptor sigue vivo");
    let root_task = Task {
        path: root_w.clone(),
        id: 0,
        tag: 0,
        prio: NO_PRIO,
        claim: false,
        cloud: false,
    };
    let mut st = WorkerState::new();
    let root_stats = walker::process_dir(&sh, &tx, &root_task, true, &mut st)
        .map_err(|c| ScanError::Root(io_err(c)))?;
    let mut top = vec![TopLevel::new(OsString::new())];
    top.extend(st.top_names.drain(..).map(TopLevel::new));
    if st.top_overflow {
        top.push(TopLevel::new(OsString::from("(otras carpetas)")));
    }
    let _ = progress.top.set(top);
    walker::account(&sh, &root_task, &root_stats);
    let mut children = std::mem::take(&mut st.children);

    // Cola prioritaria (solo con FileId: no hay adopción posible con FindFirstFileExW).
    let find_mode = sh.find_mode.load(Ordering::Relaxed);
    let mut resolved = priority::Resolved::default();
    if !find_mode && (opts.known_paths || opts.priority_paths.is_some()) {
        let candidates = opts
            .priority_paths
            .clone()
            .unwrap_or_else(|| known::known_paths(drive_of(&root).as_deref()));
        let root_children: Vec<priority::RootChild> = children
            .iter()
            .map(|t| {
                let name = t.path.rsplit(|&c| c == b'\\' as u16).next().unwrap_or(&[]);
                priority::RootChild {
                    name: OsString::from_wide(name),
                    id: t.id,
                    tag: t.tag,
                }
            })
            .collect();
        resolved = priority::resolve(&sh, &root, &root_children, candidates);
    }
    if let Some(batch) = resolved.batch.take() {
        tx.send(batch).expect("el receptor sigue vivo");
    }
    let _ = progress.priority.set(
        resolved
            .targets
            .iter()
            .cloned()
            .map(PriorityTarget::new)
            .collect(),
    );
    let _ = sh.claimed.set(
        resolved
            .targets
            .iter()
            .map(|_| AtomicBool::new(false))
            .collect(),
    );
    let _ = sh.adopt.set(std::mem::take(&mut resolved.adopt));
    children.retain(|c| !resolved.takeover.contains(&c.id));
    sh.pending
        .fetch_add(children.len() + resolved.initial.len(), Ordering::SeqCst);
    for c in children {
        sh.injector.push(c);
    }
    sh.prio_hint
        .fetch_add(resolved.initial.len(), Ordering::Relaxed);
    for t in std::mem::take(&mut resolved.initial) {
        sh.prio.push(t);
    }

    let threads = opts.threads.max(1);
    let now_unix = opts.now_unix.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs().min(u32::MAX as u64) as u32)
    });
    let ctx = Ctx {
        sh,
        tx,
        rx,
        threads,
        root_w,
        display_root,
        full_volume,
        volume_override: opts.volume_override,
        fs_name,
        volume_gap: opts.volume_gap,
        dedup: opts.dedup_hard_links,
        now_unix,
        t0,
    };
    let join = std::thread::Builder::new()
        .name("tesela-scan".into())
        .spawn(move || run(ctx))
        .map_err(ScanError::Root)?;
    Ok(ScanHandle {
        progress,
        cancel,
        join,
    })
}

/// Todo lo que necesita el hilo coordinador.
struct Ctx {
    sh: Arc<Shared>,
    tx: crossbeam_channel::Sender<Batch>,
    rx: crossbeam_channel::Receiver<Batch>,
    threads: usize,
    root_w: Vec<u16>,
    display_root: PathBuf,
    full_volume: bool,
    volume_override: Option<VolumeSpace>,
    fs_name: String,
    volume_gap: bool,
    dedup: bool,
    now_unix: u32,
    t0: Instant,
}

/// Hilo coordinador: lanza el colector y los trabajadores, espera, construye el árbol.
fn run(ctx: Ctx) -> ScanResult {
    let Ctx {
        sh,
        tx,
        rx,
        threads,
        root_w,
        display_root,
        full_volume,
        volume_override,
        fs_name,
        volume_gap,
        dedup,
        now_unix,
        t0,
    } = ctx;
    let cancel = sh.cancel.clone();
    let workers: Vec<Worker<Task>> = (0..threads).map(|_| Worker::new_lifo()).collect();
    let stealers: Vec<Stealer<Task>> = workers.iter().map(Worker::stealer).collect();

    let (builder, build_err, outs) = std::thread::scope(|s| {
        let cancel_c = cancel.clone();
        let collector = s.spawn(move || {
            let mut builder = TreeBuilder::new();
            let mut err: Option<BuildError> = None;
            for batch in rx {
                if err.is_none()
                    && let Err(e) = builder.add_batch(batch)
                {
                    err = Some(e);
                    cancel_c.store(true, Ordering::SeqCst);
                }
            }
            (builder, err)
        });
        let handles: Vec<_> = workers
            .into_iter()
            .enumerate()
            .map(|(i, local)| {
                let tx = tx.clone();
                let (sh, stealers) = (&sh, &stealers);
                s.spawn(move || walker::worker_loop(sh, tx, local, stealers, i))
            })
            .collect();
        drop(tx);
        let outs: Vec<WorkerOut> = handles
            .into_iter()
            .map(|h| h.join().expect("hilo de escaneo"))
            .collect();
        let (builder, err) = collector.join().expect("hilo colector");
        (builder, err, outs)
    });
    let elapsed_scan = t0.elapsed();

    // "Parar y ver": hubo cancelación Y quedó trabajo sin hacer. Que `pending` sea 0 no basta: al cancelar se descartan
    // los subdirectorios recién descubiertos sin contarlos (`dropped`).
    let ids_exhausted = sh.id_overflow.load(Ordering::SeqCst);
    let cancel_flag = cancel.load(Ordering::SeqCst);
    let unfinished = sh.pending.load(Ordering::SeqCst) != 0 || sh.dropped.load(Ordering::SeqCst);
    let mut cancelled = ids_exhausted || (cancel_flag && unfinished);
    let volume = volume_override.or_else(|| {
        sys::volume_space(&root_w)
            .ok()
            .map(|(total, free)| VolumeSpace { total, free })
    });
    let mut fo = FinishOptions::new(now_unix);
    fo.dedup_hard_links = dedup;
    if full_volume && volume_gap && !cancelled && build_err.is_none() {
        fo.volume_used = volume.map(|v| v.total.saturating_sub(v.free));
    }
    let t1 = Instant::now();
    let tree = builder.finish(&fo);
    let elapsed_build = t1.elapsed();
    // Segunda red: si el árbol quedó con directorios sin listar tras cancelar, también cuenta como detenido.
    cancelled |= cancel_flag && tree.stats().incomplete_dirs > 0;

    let mut errors_by_code: BTreeMap<u32, u64> = BTreeMap::new();
    let mut error_samples = Vec::new();
    for o in outs {
        for (c, n) in o.errors {
            *errors_by_code.entry(c).or_default() += n;
        }
        for (c, p) in o.error_samples {
            if error_samples.len() < 10 {
                error_samples.push((c, PathBuf::from(OsString::from_wide(&p))));
            }
        }
    }
    let prio_done = |t: &PriorityTarget| PriorityResult {
        path: t.path.clone(),
        done: t.is_done(),
        alloc: t.alloc.load(Ordering::Relaxed),
        files: t.files.load(Ordering::Relaxed),
    };
    let priority = sh
        .progress
        .priority_targets()
        .iter()
        .map(prio_done)
        .collect();
    let api = match (
        sh.find_mode.load(Ordering::Relaxed),
        sh.any_find.load(Ordering::Relaxed),
    ) {
        (true, _) => ApiUsed::FindFallback,
        (false, true) => ApiUsed::Mixed,
        _ => ApiUsed::Extended,
    };
    let summary = ScanSummary {
        root: display_root,
        cancelled,
        elapsed_scan,
        elapsed_build,
        threads,
        api,
        fs_name,
        errors_by_code,
        error_samples,
        volume,
        full_volume,
        priority,
        ids_exhausted,
        build_error: build_err,
    };
    sh.progress.finished.store(true, Ordering::SeqCst);
    ScanResult { tree, summary }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn los_hilos_por_defecto_son_min_16_y_dos_veces_los_nucleos() {
        let n = default_threads();
        let cores = std::thread::available_parallelism().map_or(2, std::num::NonZero::get);
        assert!((1..=MAX_DEFAULT_THREADS).contains(&n));
        assert_eq!(n, MAX_DEFAULT_THREADS.min(2 * cores));
        assert_eq!(ScanOptions::new(".").threads, n);
    }
}
