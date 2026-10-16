//! El recorrido: el MISMO código mide, simula y borra (PLAN.md §3.7.3 y §3.7.8).
//!
//! # Garantías
//! * Todo se abre RELATIVO al handle del directorio que se está recorriendo (`NtCreateFile` con `RootDirectory`): ninguna
//!   ruta se vuelve a resolver desde la raíz, así que cambiar un ancestro por un junction no redirige nada. Es el patrón
//!   con el que se reescribió `std::fs::remove_dir_all` tras CVE-2022-21658.
//! * Cada hijo se abre con `FILE_OPEN_REPARSE_POINT` y con el TIPO que dijo el listado (archivo o directorio): si cambió, el
//!   sistema lo rechaza. Nunca se entra en un reparse point y nunca se borra uno (se salta y se cuenta).
//! * El `FileId` del handle debe coincidir con el del listado y el volumen con el de la raíz; si no, "cambió desde la
//!   confirmación" y se salta. Un archivo se abre en EXCLUSIVA (`dwShareMode = 0`): `SHARING_VIOLATION` = en uso.
//! * Se actúa sobre ese mismo handle (`FileDispositionInfoEx`), de abajo arriba: un directorio solo se borra cuando sus
//!   hijos terminaron, y por su propio handle (que se abre compartiendo todo para poder listarlo aunque el Explorador o
//!   un antivirus lo observen; la identidad se verifica igual).
//! * La protección se consulta en CADA hijo ([`Protection::child_action`]), no solo al aprobar la cesta.
//! * Pool propio de hilos (`crossbeam-deque`, con pilas LIFO por hilo: recorrido en profundidad, así que los handles
//!   abiertos son del orden de la profundidad por hilo) y cancelación entre entradas.

use crate::paths::PathKey;
use crate::protect::{ChildAction, Protection, Verdict};
use crate::sys::{self, DirEntry, Expect, FileIdentity, Handle, Win32};
use crate::volume;
use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Qué hace el recorrido con lo que encuentra.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Solo cuenta (aperturas compartidas, de solo lectura): para estimar al aprobar.
    Measure,
    /// Hace TODO salvo marcar para borrar: abre en exclusiva, verifica identidades, cuenta y registra lo que borraría.
    DryRun,
    /// Borra.
    Delete,
}

/// Contadores de una unidad de trabajo (atómicos: los actualizan los hilos del pool y los lee la interfaz).
#[derive(Debug, Default)]
pub struct Stats {
    /// Bytes en disco de lo eliminado (o de lo que se eliminaría), SIN contar los enlaces duros compartidos.
    pub bytes: AtomicU64,
    /// Tamaño lógico de lo eliminado (para el encaje en la Papelera).
    pub logical: AtomicU64,
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    /// Archivos con más de un enlace: se borra un nombre, pero no liberan espacio.
    pub shared_files: AtomicU64,
    pub shared_bytes: AtomicU64,
    /// `ERROR_SHARING_VIOLATION`: en uso, se salta y se cuenta.
    pub in_use: AtomicU64,
    /// `ERROR_ACCESS_DENIED`.
    pub denied: AtomicU64,
    /// Identidad o tipo distintos de lo visto en el listado / aprobado.
    pub changed: AtomicU64,
    /// Entradas protegidas que se respetaron.
    pub protected: AtomicU64,
    pub protected_bytes: AtomicU64,
    /// Demasiado recientes (`min_age`).
    pub young: AtomicU64,
    /// Reparse points: no se entra ni se borran.
    pub reparse: AtomicU64,
    /// Otros errores del sistema.
    pub other_errors: AtomicU64,
    /// Directorios que no se pudieron borrar porque no quedaron vacíos (lo normal si algo se saltó).
    pub not_empty: AtomicU64,
    /// Archivos de la nube dentro (se saltan: son reparse points).
    pub cloud: AtomicU64,
}

impl Stats {
    fn add(a: &AtomicU64, n: u64) {
        a.fetch_add(n, Ordering::Relaxed);
    }
    fn get(a: &AtomicU64) -> u64 {
        a.load(Ordering::Relaxed)
    }
    /// Copia de los valores.
    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            bytes: Self::get(&self.bytes),
            logical: Self::get(&self.logical),
            files: Self::get(&self.files),
            dirs: Self::get(&self.dirs),
            shared_files: Self::get(&self.shared_files),
            shared_bytes: Self::get(&self.shared_bytes),
            in_use: Self::get(&self.in_use),
            denied: Self::get(&self.denied),
            changed: Self::get(&self.changed),
            protected: Self::get(&self.protected),
            protected_bytes: Self::get(&self.protected_bytes),
            young: Self::get(&self.young),
            reparse: Self::get(&self.reparse),
            other_errors: Self::get(&self.other_errors),
            not_empty: Self::get(&self.not_empty),
            cloud: Self::get(&self.cloud),
        }
    }
}

/// Copia inmutable de [`Stats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatsSnapshot {
    pub bytes: u64,
    pub logical: u64,
    pub files: u64,
    pub dirs: u64,
    pub shared_files: u64,
    pub shared_bytes: u64,
    pub in_use: u64,
    pub denied: u64,
    pub changed: u64,
    pub protected: u64,
    pub protected_bytes: u64,
    pub young: u64,
    pub reparse: u64,
    pub other_errors: u64,
    pub not_empty: u64,
    pub cloud: u64,
}

impl StatsSnapshot {
    /// Suma de otra instantánea.
    pub fn merge(&mut self, o: &StatsSnapshot) {
        self.bytes += o.bytes;
        self.logical += o.logical;
        self.files += o.files;
        self.dirs += o.dirs;
        self.shared_files += o.shared_files;
        self.shared_bytes += o.shared_bytes;
        self.in_use += o.in_use;
        self.denied += o.denied;
        self.changed += o.changed;
        self.protected += o.protected;
        self.protected_bytes += o.protected_bytes;
        self.young += o.young;
        self.reparse += o.reparse;
        self.other_errors += o.other_errors;
        self.not_empty += o.not_empty;
        self.cloud += o.cloud;
    }
}

/// Qué pasó con una entrada (para el registro local).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Deleted,
    /// Simulación: se habría borrado.
    WouldDelete,
    /// Enviado a la Papelera (verificado).
    Recycled,
    /// Simulación: se habría enviado a la Papelera.
    WouldRecycle,
    /// Tras enviarlo, sigue en su sitio: no se envió.
    NotRecycled,
    /// Desapareció sin llegar a la Papelera: se eliminó definitivamente.
    DeletedUnexpectedly,
    /// Se movió a la Papelera un objeto distinto del aprobado (recuperable).
    RecycledOther,
    /// No se pudo comprobar qué pasó.
    Unverified,
    InUse,
    Denied,
    Changed,
    Protected,
    Young,
    Reparse,
    Failed(Win32),
    /// Un directorio que no quedó vacío.
    NotEmpty,
}

/// Una entrada procesada, para el registro (solo se construye si hay quien escuche).
#[derive(Clone, Debug)]
pub struct Event {
    pub unit: usize,
    pub path: PathBuf,
    pub is_dir: bool,
    pub identity: Option<FileIdentity>,
    pub bytes: u64,
    pub links: u32,
    pub outcome: Outcome,
}

/// Receptor de eventos (el registro local, o una prueba).
pub type EventSink = dyn Fn(Event) + Send + Sync;

/// Parámetros comunes de un recorrido.
pub struct Env {
    pub protection: Arc<Protection>,
    pub mode: Mode,
    /// Antigüedad mínima en intervalos de 100 ns (0 = sin límite).
    pub min_age_ft: i64,
    /// "Ahora" (FILETIME).
    pub now_ft: i64,
    pub cancel: Arc<AtomicBool>,
    pub sink: Option<Arc<EventSink>>,
    /// Entradas procesadas (progreso para la interfaz).
    pub progress: Arc<AtomicU64>,
}

/// FILETIME de "ahora".
pub fn now_filetime() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    (secs + 11_644_473_600) * 10_000_000
}

/// Una antigüedad en intervalos de FILETIME.
pub fn duration_to_ft(d: Duration) -> i64 {
    (d.as_secs() as i64).saturating_mul(10_000_000)
}

/// Qué recorrer.
#[derive(Clone, Debug)]
pub enum UnitSpec {
    /// Borra los HIJOS de este directorio (que no se borra): las raíces de una categoría con regla.
    Contents {
        path: PathBuf,
        approved: FileIdentity,
    },
    /// Borra esta entrada entera (archivo o directorio) con la identidad aprobada.
    Entry {
        path: PathBuf,
        approved: FileIdentity,
        /// Etiqueta de reparse aprobada (0 salvo archivos de la nube, que se admiten solo como elemento suelto).
        approved_tag: u32,
        is_dir: bool,
    },
}

/// Cómo terminó el procesado de la RAÍZ de una unidad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootStatus {
    Done,
    /// La raíz ya no existe.
    Missing,
    /// La identidad de la raíz no es la aprobada.
    Changed,
    /// La ruta resuelta de la raíz no es la aprobada o ahora está protegida.
    Protected,
    /// La ruta resuelta de la raíz no es de una unidad local con letra (UNC, también hacia un disco local, o red).
    NotLocal,
    /// La raíz es un archivo en uso.
    InUse,
    Denied,
    Failed(Win32),
    Cancelled,
}

/// Lo eliminado (de verdad) bajo una entrada de primer nivel de una unidad: la interfaz lo usa para actualizar el árbol
/// sin reescanear (overlay, PLAN.md §3.6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopResult {
    /// Nombre de la entrada (hija de la raíz de un `Contents`; o el propio elemento de un `Entry`).
    pub name: OsString,
    pub is_dir: bool,
    /// La entrada desapareció entera (si no, solo una parte de su contenido).
    pub removed: bool,
    /// Bytes en disco eliminados dentro de ella (sin enlaces duros compartidos).
    pub bytes: u64,
    /// Tamaño lógico eliminado.
    pub logical: u64,
    /// Archivos eliminados.
    pub files: u64,
    /// Directorios eliminados (incluida ella si `removed`).
    pub dirs: u64,
}

/// Acumulador de lo eliminado bajo un directorio.
#[derive(Default)]
struct Acc {
    bytes: AtomicU64,
    logical: AtomicU64,
    files: AtomicU64,
    dirs: AtomicU64,
}

impl Acc {
    fn add_file(&self, bytes: u64, logical: u64) {
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        self.logical.fetch_add(logical, Ordering::Relaxed);
        self.files.fetch_add(1, Ordering::Relaxed);
    }
    fn add_all(&self, t: &TopResult) {
        self.bytes.fetch_add(t.bytes, Ordering::Relaxed);
        self.logical.fetch_add(t.logical, Ordering::Relaxed);
        self.files.fetch_add(t.files, Ordering::Relaxed);
        self.dirs.fetch_add(t.dirs, Ordering::Relaxed);
    }
}

/// Una unidad de trabajo con sus contadores y su resultado.
pub struct Unit {
    pub id: usize,
    pub spec: UnitSpec,
    pub env: Arc<Env>,
    pub stats: Stats,
    status: Mutex<Option<RootStatus>>,
    tops: Mutex<Vec<TopResult>>,
    root_result: Mutex<Option<TopResult>>,
}

impl Unit {
    pub fn new(id: usize, spec: UnitSpec, env: Arc<Env>) -> Arc<Unit> {
        Arc::new(Unit {
            id,
            spec,
            env,
            stats: Stats::default(),
            status: Mutex::new(None),
            tops: Mutex::new(Vec::new()),
            root_result: Mutex::new(None),
        })
    }
    /// Lo eliminado bajo cada entrada de primer nivel (solo en una unidad `Contents` ejecutada en modo `Delete`).
    pub fn tops(&self) -> Vec<TopResult> {
        self.tops.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    /// Lo eliminado del propio elemento (una unidad `Entry` ejecutada en modo `Delete`).
    pub fn root_result(&self) -> Option<TopResult> {
        self.root_result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// Cómo terminó la raíz (`None` si no llegó a procesarse).
    pub fn status(&self) -> Option<RootStatus> {
        *self.status.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn set_status(&self, s: RootStatus) {
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
    }
}

/// Un directorio abierto del recorrido.
struct DirNode {
    unit: Arc<Unit>,
    handle: Handle,
    path: PathBuf,
    key: PathKey,
    parent: Option<Arc<DirNode>>,
    /// Tareas pendientes de este directorio, más 1 de guarda mientras se reparten sus hijos.
    pending: AtomicUsize,
    /// ¿Se puede borrar este directorio al terminar? (No la raíz de un `Contents`, ni lo protegido.)
    deletable: bool,
    /// Algo de dentro se respetó o falló: el directorio no quedará vacío.
    kept: AtomicBool,
    /// La fecha más reciente del propio directorio al abrirlo ya cumple `min_age`.
    old_enough: bool,
    volume_serial: u64,
    /// Es la raíz de una unidad `Contents` (sus hijos son las entradas de primer nivel).
    contents_root: bool,
    /// Lo eliminado hasta ahora bajo este directorio.
    acc: Acc,
}

impl DirNode {
    fn env(&self) -> &Env {
        &self.unit.env
    }
    fn mark_parent_kept(&self) {
        if let Some(p) = &self.parent {
            p.kept.store(true, Ordering::Release);
        }
    }
}

enum Task {
    Start(Arc<Unit>),
    Child(Arc<DirNode>, DirEntry),
}

struct Shared {
    injector: Injector<Task>,
    stealers: Vec<Stealer<Task>>,
    outstanding: AtomicUsize,
}

/// Nombre de una entrada para construir claves de comparación: sin pérdida (ver `paths::wide_to_string`).
fn lossy(name: &[u16]) -> String {
    crate::paths::wide_to_string(name)
}

fn join_name(base: &Path, name: &[u16]) -> PathBuf {
    base.join(OsString::from_wide(name))
}

#[allow(clippy::too_many_arguments)]
fn emit(
    unit: &Unit,
    path: PathBuf,
    is_dir: bool,
    identity: Option<FileIdentity>,
    bytes: u64,
    links: u32,
    outcome: Outcome,
) {
    if let Some(s) = &unit.env.sink {
        s(Event {
            unit: unit.id,
            path,
            is_dir,
            identity,
            bytes,
            links,
            outcome,
        });
    }
}

/// ¿El código de Windows es "ya no existe"?
fn is_gone(code: Win32) -> bool {
    matches!(code, 2 | 3)
}

/// Cuenta un error del sistema en el contador que le toca. Devuelve el resultado para el registro.
fn count_error(unit: &Unit, code: Win32) -> Outcome {
    match code {
        sys::SHARING_VIOLATION => {
            Stats::add(&unit.stats.in_use, 1);
            Outcome::InUse
        }
        sys::ACCESS_DENIED => {
            Stats::add(&unit.stats.denied, 1);
            Outcome::Denied
        }
        other => {
            Stats::add(&unit.stats.other_errors, 1);
            Outcome::Failed(other)
        }
    }
}

/// Ejecuta las unidades con un pool de `threads` hilos y vuelve cuando todas terminaron (o se canceló).
pub fn run(units: &[Arc<Unit>], threads: usize) {
    let threads = threads.clamp(1, 32);
    let workers: Vec<Worker<Task>> = (0..threads).map(|_| Worker::new_lifo()).collect();
    let shared = Shared {
        injector: Injector::new(),
        stealers: workers.iter().map(Worker::stealer).collect(),
        outstanding: AtomicUsize::new(0),
    };
    for u in units {
        shared.outstanding.fetch_add(1, Ordering::SeqCst);
        shared.injector.push(Task::Start(u.clone()));
    }
    std::thread::scope(|s| {
        for w in workers {
            let shared = &shared;
            s.spawn(move || worker_loop(shared, w));
        }
    });
}

fn find_task(shared: &Shared, local: &Worker<Task>) -> Option<Task> {
    if let Some(t) = local.pop() {
        return Some(t);
    }
    loop {
        match shared.injector.steal_batch_and_pop(local) {
            Steal::Success(t) => return Some(t),
            Steal::Retry => continue,
            Steal::Empty => break,
        }
    }
    for st in &shared.stealers {
        loop {
            match st.steal() {
                Steal::Success(t) => return Some(t),
                Steal::Retry => continue,
                Steal::Empty => break,
            }
        }
    }
    None
}

fn worker_loop(shared: &Shared, local: Worker<Task>) {
    let mut idle = 0u32;
    loop {
        match find_task(shared, &local) {
            Some(task) => {
                idle = 0;
                // Un pánico en una tarea no puede dejar a los demás hilos esperando para siempre: se cancela todo.
                let cancel = match &task {
                    Task::Start(u) => u.env.cancel.clone(),
                    Task::Child(n, _) => n.env().cancel.clone(),
                };
                let r = std::panic::catch_unwind(AssertUnwindSafe(|| handle(shared, &local, task)));
                if r.is_err() {
                    cancel.store(true, Ordering::SeqCst);
                }
                shared.outstanding.fetch_sub(1, Ordering::SeqCst);
            }
            None => {
                if shared.outstanding.load(Ordering::SeqCst) == 0 {
                    return;
                }
                idle = (idle + 1).min(6);
                std::thread::sleep(Duration::from_micros(50u64 << idle));
            }
        }
    }
}

fn handle(shared: &Shared, local: &Worker<Task>, task: Task) {
    match task {
        Task::Start(unit) => start_unit(shared, local, unit),
        Task::Child(parent, entry) => {
            if process_child(shared, local, &parent, entry) {
                finish_one(parent);
            }
        }
    }
}

/// Una tarea del directorio terminó: si era la última, el directorio se finaliza y se avisa al padre.
fn finish_one(mut node: Arc<DirNode>) {
    loop {
        if node.pending.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        finalize(&node);
        match node.parent.clone() {
            Some(p) => node = p,
            None => return,
        }
    }
}

/// Cuenta lo eliminado bajo `node` en su padre y, si es una entrada de primer nivel de un `Contents` (o el elemento de un
/// `Entry`), lo deja anotado para el overlay de la interfaz.
fn propagate(node: &DirNode, removed: bool) {
    let me = TopResult {
        name: node
            .path
            .file_name()
            .map(OsString::from)
            .unwrap_or_default(),
        is_dir: true,
        removed,
        bytes: node.acc.bytes.load(Ordering::Relaxed),
        logical: node.acc.logical.load(Ordering::Relaxed),
        files: node.acc.files.load(Ordering::Relaxed),
        dirs: node.acc.dirs.load(Ordering::Relaxed) + u64::from(removed),
    };
    match &node.parent {
        Some(p) => {
            p.acc.add_all(&me);
            if p.contents_root {
                node.unit
                    .tops
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(me);
            }
        }
        None => {
            *node
                .unit
                .root_result
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(me)
        }
    }
}

/// Borra el directorio (vacío) por SU propio handle. Lo que no se borra mantiene vivos a sus ancestros.
fn finalize(node: &DirNode) {
    let unit = &node.unit;
    let real = node.env().mode == Mode::Delete;
    if !node.deletable
        || node.env().cancel.load(Ordering::Relaxed)
        || node.kept.load(Ordering::Acquire)
    {
        // Se queda (protegido, cancelado, o con algo dentro que se respetó o falló): su padre tampoco puede quedar vacío.
        node.mark_parent_kept();
        if real {
            propagate(node, false);
        }
        return;
    }
    if !node.old_enough {
        Stats::add(&unit.stats.young, 1);
        node.mark_parent_kept();
        if real {
            propagate(node, false);
        }
        return;
    }
    match node.env().mode {
        Mode::Measure => Stats::add(&unit.stats.dirs, 1),
        Mode::DryRun => {
            Stats::add(&unit.stats.dirs, 1);
            emit(
                unit,
                node.path.clone(),
                true,
                None,
                0,
                0,
                Outcome::WouldDelete,
            );
        }
        Mode::Delete => match sys::delete_on_handle(&node.handle) {
            Ok(()) => {
                Stats::add(&unit.stats.dirs, 1);
                emit(unit, node.path.clone(), true, None, 0, 0, Outcome::Deleted);
                propagate(node, true);
            }
            Err(sys::DIR_NOT_EMPTY) => {
                Stats::add(&unit.stats.not_empty, 1);
                node.mark_parent_kept();
                emit(unit, node.path.clone(), true, None, 0, 0, Outcome::NotEmpty);
                propagate(node, false);
            }
            Err(code) => {
                let o = count_error(unit, code);
                node.mark_parent_kept();
                emit(unit, node.path.clone(), true, None, 0, 0, o);
                propagate(node, false);
            }
        },
    }
}

/// Reparte los hijos de `node` como tareas y suelta la guarda.
fn list_and_spawn(shared: &Shared, local: &Worker<Task>, node: Arc<DirNode>) {
    let entries = match sys::list_dir(&node.handle) {
        Ok(v) => v,
        Err(code) => {
            let o = count_error(&node.unit, code);
            node.kept.store(true, Ordering::Release);
            emit(&node.unit, node.path.clone(), true, None, 0, 0, o);
            Vec::new()
        }
    };
    // Primero los contadores y después las tareas: `outstanding` no puede llegar a 0 con trabajo por repartir.
    node.pending.fetch_add(entries.len(), Ordering::AcqRel);
    shared
        .outstanding
        .fetch_add(entries.len(), Ordering::SeqCst);
    for e in entries {
        local.push(Task::Child(node.clone(), e));
    }
    finish_one(node);
}

fn root_open_error(unit: &Unit, code: Win32) {
    unit.set_status(match code {
        c if is_gone(c) => RootStatus::Missing,
        sys::ACCESS_DENIED => RootStatus::Denied,
        sys::SHARING_VIOLATION => RootStatus::InUse,
        other => RootStatus::Failed(other),
    });
}

fn start_unit(shared: &Shared, local: &Worker<Task>, unit: Arc<Unit>) {
    let env = &unit.env;
    if env.cancel.load(Ordering::Relaxed) {
        unit.set_status(RootStatus::Cancelled);
        return;
    }
    #[cfg(test)]
    match &unit.spec {
        UnitSpec::Contents { path, .. } | UnitSpec::Entry { path, .. } => {
            test_hooks::before_open(path)
        }
    }
    match unit.spec.clone() {
        UnitSpec::Contents { path, approved } => {
            let h = match sys::open_dir_for_listing(&path) {
                Ok(h) => h,
                Err(c) => return root_open_error(&unit, c),
            };
            let Some(node) = verify_root(&unit, h, &path, &approved, 0, true, false) else {
                return;
            };
            unit.set_status(RootStatus::Done);
            list_and_spawn(shared, local, node);
        }
        UnitSpec::Entry {
            path,
            approved,
            approved_tag,
            is_dir,
        } => {
            if is_dir {
                let opened = if env.mode == Mode::Measure {
                    sys::open_dir_for_listing(&path)
                } else {
                    sys::open_path_dir_for_delete(&path)
                };
                let h = match opened {
                    Ok(h) => h,
                    Err(c) => return root_open_error(&unit, c),
                };
                let Some(node) = verify_root(&unit, h, &path, &approved, approved_tag, false, true)
                else {
                    return;
                };
                unit.set_status(RootStatus::Done);
                list_and_spawn(shared, local, node);
            } else {
                entry_file(&unit, &path, &approved, approved_tag);
            }
        }
    }
}

/// Verifica la raíz de una unidad (identidad aprobada, tipo, ruta resuelta y protección) y crea su nodo.
/// `contents`: es la raíz de una categoría (no se borra). `deletable`: el directorio raíz se borra al final.
fn verify_root(
    unit: &Arc<Unit>,
    h: Handle,
    approved_path: &Path,
    approved: &FileIdentity,
    approved_tag: u32,
    contents: bool,
    deletable: bool,
) -> Option<Arc<DirNode>> {
    let env = &unit.env;
    let m = match sys::meta(&h) {
        Ok(m) => m,
        Err(c) => {
            root_open_error(unit, c);
            return None;
        }
    };
    // Un directorio con reparse point (junction, symlink, punto de montaje) nunca se acepta: no se entra en él.
    let _ = approved_tag;
    if !m.is_dir || m.identity != *approved || m.is_reparse() {
        unit.set_status(RootStatus::Changed);
        Stats::add(&unit.stats.changed, 1);
        emit(
            unit,
            approved_path.to_path_buf(),
            true,
            Some(m.identity),
            0,
            m.links,
            Outcome::Changed,
        );
        return None;
    }
    let final_path = match sys::final_path(&h) {
        Ok(p) => p,
        Err(c) => {
            root_open_error(unit, c);
            return None;
        }
    };
    if !volume::is_local_path(&final_path) {
        unit.set_status(RootStatus::NotLocal);
        Stats::add(&unit.stats.protected, 1);
        emit(
            unit,
            final_path,
            true,
            Some(m.identity),
            0,
            m.links,
            Outcome::Protected,
        );
        return None;
    }
    let key = PathKey::new(&final_path);
    // La ruta resuelta por el handle es la aprobada, y sigue siendo un destino permitido (re-validación de protección).
    let same_route = PathKey::new(approved_path) == key;
    // Para borrar el CONTENIDO de una raíz vale también una carpeta intocable como destino o una carpeta conocida
    // ("su contenido sí se puede limpiar", PLAN.md §2.7); solo lo denegado queda fuera. Un elemento suelto exige `Allowed`.
    let verdict = env.protection.target_verdict(&key);
    let verdict_ok = match verdict {
        Verdict::Allowed => true,
        Verdict::Untouchable | Verdict::ContentsOnly => contents,
        Verdict::Denied => false,
    };
    if !same_route || !verdict_ok {
        unit.set_status(RootStatus::Protected);
        Stats::add(&unit.stats.protected, 1);
        emit(
            unit,
            final_path,
            true,
            Some(m.identity),
            0,
            m.links,
            Outcome::Protected,
        );
        return None;
    }
    Some(Arc::new(DirNode {
        unit: unit.clone(),
        handle: h,
        path: final_path,
        key,
        parent: None,
        pending: AtomicUsize::new(1),
        deletable: deletable && !contents,
        kept: AtomicBool::new(false),
        old_enough: true,
        volume_serial: m.identity.volume_serial,
        contents_root: contents,
        acc: Acc::default(),
    }))
}

/// Un archivo suelto aprobado como elemento de la cesta.
fn entry_file(unit: &Arc<Unit>, path: &Path, approved: &FileIdentity, approved_tag: u32) {
    let env = &unit.env;
    let opened = if env.mode == Mode::Measure {
        sys::open_for_query(path)
    } else {
        sys::open_path_exclusive(path)
    };
    let h = match opened {
        Ok(h) => h,
        Err(c) => {
            if !is_gone(c) {
                let o = count_error(unit, c);
                emit(unit, path.to_path_buf(), false, None, 0, 0, o);
            }
            return root_open_error(unit, c);
        }
    };
    let m = match sys::meta(&h) {
        Ok(m) => m,
        Err(c) => return root_open_error(unit, c),
    };
    // Un archivo de la nube (reparse con etiqueta de nube) solo vale si es el que se aprobó, con su aviso; cualquier otro
    // reparse point se rechaza.
    let reparse_ok =
        !m.is_reparse() || (m.reparse_tag == approved_tag && sys::is_cloud_tag(m.reparse_tag));
    if m.is_dir || m.identity != *approved || !reparse_ok {
        unit.set_status(RootStatus::Changed);
        Stats::add(&unit.stats.changed, 1);
        emit(
            unit,
            path.to_path_buf(),
            false,
            Some(m.identity),
            m.alloc,
            m.links,
            Outcome::Changed,
        );
        return;
    }
    let final_path = match sys::final_path(&h) {
        Ok(p) => p,
        Err(c) => return root_open_error(unit, c),
    };
    if !volume::is_local_path(&final_path) {
        unit.set_status(RootStatus::NotLocal);
        Stats::add(&unit.stats.protected, 1);
        emit(
            unit,
            path.to_path_buf(),
            false,
            Some(m.identity),
            m.alloc,
            m.links,
            Outcome::Protected,
        );
        return;
    }
    let key = PathKey::new(&final_path);
    if PathKey::new(path) != key || env.protection.target_verdict(&key) != Verdict::Allowed {
        unit.set_status(RootStatus::Protected);
        Stats::add(&unit.stats.protected, 1);
        emit(
            unit,
            path.to_path_buf(),
            false,
            Some(m.identity),
            m.alloc,
            m.links,
            Outcome::Protected,
        );
        return;
    }
    if m.links > 1 {
        Stats::add(&unit.stats.shared_files, 1);
        Stats::add(&unit.stats.shared_bytes, m.alloc);
    } else {
        Stats::add(&unit.stats.bytes, m.alloc);
    }
    Stats::add(&unit.stats.logical, m.logical);
    match env.mode {
        Mode::Measure => {
            Stats::add(&unit.stats.files, 1);
            unit.set_status(RootStatus::Done);
        }
        Mode::DryRun => {
            Stats::add(&unit.stats.files, 1);
            emit(
                unit,
                path.to_path_buf(),
                false,
                Some(m.identity),
                m.alloc,
                m.links,
                Outcome::WouldDelete,
            );
            unit.set_status(RootStatus::Done);
        }
        Mode::Delete => match sys::delete_on_handle(&h) {
            Ok(()) => {
                Stats::add(&unit.stats.files, 1);
                *unit.root_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(TopResult {
                    name: path.file_name().map(OsString::from).unwrap_or_default(),
                    is_dir: false,
                    removed: true,
                    bytes: if m.links > 1 { 0 } else { m.alloc },
                    logical: m.logical,
                    files: 1,
                    dirs: 0,
                });
                emit(
                    unit,
                    path.to_path_buf(),
                    false,
                    Some(m.identity),
                    m.alloc,
                    m.links,
                    Outcome::Deleted,
                );
                unit.set_status(RootStatus::Done);
            }
            Err(c) => {
                let o = count_error(unit, c);
                emit(
                    unit,
                    path.to_path_buf(),
                    false,
                    Some(m.identity),
                    m.alloc,
                    m.links,
                    o,
                );
                root_open_error(unit, c);
            }
        },
    }
}

/// Devuelve `true` si la tarea terminó (quien llama descuenta una tarea pendiente del padre) y `false` si era un
/// directorio: entonces su propio nodo descuenta del padre cuando se finalice (tras todos sus descendientes).
fn process_child(
    shared: &Shared,
    local: &Worker<Task>,
    parent: &Arc<DirNode>,
    entry: DirEntry,
) -> bool {
    let unit = &parent.unit;
    let env = parent.env();
    if env.cancel.load(Ordering::Relaxed) {
        parent.kept.store(true, Ordering::Release);
        return true;
    }
    env.progress.fetch_add(1, Ordering::Relaxed);
    let name = &entry.name;
    let ckey = parent.key.join(&lossy(name));
    let child_path = join_name(&parent.path, name);
    #[cfg(test)]
    test_hooks::before_open(&child_path);
    let action = env.protection.child_action(&ckey);
    // Protegido: denegado (o un archivo que sería "solo descender", que no tiene sentido): se respeta.
    if action == ChildAction::Skip || (action == ChildAction::Descend && !entry.is_dir()) {
        Stats::add(&unit.stats.protected, 1);
        Stats::add(&unit.stats.protected_bytes, entry.alloc);
        parent.kept.store(true, Ordering::Release);
        emit(
            unit,
            child_path,
            entry.is_dir(),
            None,
            entry.alloc,
            0,
            Outcome::Protected,
        );
        return true;
    }
    if entry.is_reparse() {
        Stats::add(&unit.stats.reparse, 1);
        if sys::is_cloud_tag(entry.reparse_tag) {
            Stats::add(&unit.stats.cloud, 1);
        }
        parent.kept.store(true, Ordering::Release);
        emit(
            unit,
            child_path,
            entry.is_dir(),
            None,
            entry.alloc,
            0,
            Outcome::Reparse,
        );
        return true;
    }
    if entry.is_dir() {
        let opened = if env.mode == Mode::Measure {
            sys::open_child_dir_for_listing(&parent.handle, name)
        } else {
            sys::open_dir_for_delete(&parent.handle, name)
        };
        let h = match opened {
            Ok(h) => h,
            Err(c) if is_gone(c) => return true,
            Err(c) => {
                let o = count_error(unit, c);
                parent.kept.store(true, Ordering::Release);
                emit(unit, child_path, true, None, 0, 0, o);
                return true;
            }
        };
        let m = match sys::meta(&h) {
            Ok(m) => m,
            Err(c) => {
                let o = count_error(unit, c);
                parent.kept.store(true, Ordering::Release);
                emit(unit, child_path, true, None, 0, 0, o);
                return true;
            }
        };
        if m.identity.file_id != entry.file_id
            || m.identity.volume_serial != parent.volume_serial
            || !m.is_dir
            || m.is_reparse()
        {
            Stats::add(&unit.stats.changed, 1);
            parent.kept.store(true, Ordering::Release);
            emit(
                unit,
                child_path,
                true,
                Some(m.identity),
                0,
                m.links,
                Outcome::Changed,
            );
            return true;
        }
        let old_enough = env.min_age_ft == 0 || env.now_ft - m.newest_filetime >= env.min_age_ft;
        let node = Arc::new(DirNode {
            unit: unit.clone(),
            handle: h,
            path: child_path,
            key: ckey,
            parent: Some(parent.clone()),
            pending: AtomicUsize::new(1),
            deletable: action == ChildAction::Delete,
            kept: AtomicBool::new(false),
            old_enough,
            volume_serial: parent.volume_serial,
            contents_root: false,
            acc: Acc::default(),
        });
        list_and_spawn(shared, local, node);
        return false;
    }
    // Un archivo.
    let young = |newest: i64| env.min_age_ft > 0 && env.now_ft - newest < env.min_age_ft;
    if young(entry.newest_filetime) {
        Stats::add(&unit.stats.young, 1);
        parent.kept.store(true, Ordering::Release);
        emit(
            unit,
            child_path,
            false,
            None,
            entry.alloc,
            0,
            Outcome::Young,
        );
        return true;
    }
    let opened = if env.mode == Mode::Measure {
        sys::open_child_for_query(&parent.handle, name, Expect::File)
    } else {
        sys::open_file_exclusive(&parent.handle, name)
    };
    let h = match opened {
        Ok(h) => h,
        Err(c) if is_gone(c) => return true,
        Err(c) => {
            let o = count_error(unit, c);
            parent.kept.store(true, Ordering::Release);
            emit(unit, child_path, false, None, entry.alloc, 0, o);
            return true;
        }
    };
    let m = match sys::meta(&h) {
        Ok(m) => m,
        Err(c) => {
            let o = count_error(unit, c);
            parent.kept.store(true, Ordering::Release);
            emit(unit, child_path, false, None, entry.alloc, 0, o);
            return true;
        }
    };
    if m.identity.file_id != entry.file_id
        || m.identity.volume_serial != parent.volume_serial
        || m.is_dir
        || m.is_reparse()
    {
        Stats::add(&unit.stats.changed, 1);
        parent.kept.store(true, Ordering::Release);
        emit(
            unit,
            child_path,
            false,
            Some(m.identity),
            m.alloc,
            m.links,
            Outcome::Changed,
        );
        return true;
    }
    // La fecha puede haber cambiado entre el listado y la apertura (alguien escribió): se vuelve a mirar en el handle.
    if young(m.newest_filetime) {
        Stats::add(&unit.stats.young, 1);
        parent.kept.store(true, Ordering::Release);
        emit(
            unit,
            child_path,
            false,
            Some(m.identity),
            m.alloc,
            m.links,
            Outcome::Young,
        );
        return true;
    }
    let account = |stats: &Stats| {
        if m.links > 1 {
            Stats::add(&stats.shared_files, 1);
            Stats::add(&stats.shared_bytes, m.alloc);
        } else {
            Stats::add(&stats.bytes, m.alloc);
        }
        Stats::add(&stats.logical, m.logical);
        Stats::add(&stats.files, 1);
    };
    match env.mode {
        Mode::Measure => account(&unit.stats),
        Mode::DryRun => {
            account(&unit.stats);
            emit(
                unit,
                child_path,
                false,
                Some(m.identity),
                m.alloc,
                m.links,
                Outcome::WouldDelete,
            );
        }
        Mode::Delete => match sys::delete_on_handle(&h) {
            Ok(()) => {
                account(&unit.stats);
                let bytes = if m.links > 1 { 0 } else { m.alloc };
                parent.acc.add_file(bytes, m.logical);
                if parent.contents_root {
                    unit.tops
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(TopResult {
                            name: OsString::from_wide(name),
                            is_dir: false,
                            removed: true,
                            bytes,
                            logical: m.logical,
                            files: 1,
                            dirs: 0,
                        });
                }
                emit(
                    unit,
                    child_path,
                    false,
                    Some(m.identity),
                    m.alloc,
                    m.links,
                    Outcome::Deleted,
                );
            }
            Err(c) => {
                let o = count_error(unit, c);
                parent.kept.store(true, Ordering::Release);
                emit(
                    unit,
                    child_path,
                    false,
                    Some(m.identity),
                    m.alloc,
                    m.links,
                    o,
                );
            }
        },
    }
    true
}

/// Ganchos de las pruebas de carreras: se ejecutan justo ANTES de abrir cada entrada (ya listada y aprobada), que es
/// donde un atacante (o un programa) podría sustituirla. Solo existen en las pruebas y solo actúan sobre rutas que
/// cuelgan de la carpeta de la prueba que los instaló (las demás pruebas corren en paralelo).
#[cfg(test)]
pub(crate) mod test_hooks {
    use crate::paths::PathKey;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    type Hook = Arc<dyn Fn(&Path) + Send + Sync>;
    /// (id, etapa, raíz, gancho). Etapa 0: justo antes de abrir una entrada; etapa 1: justo antes de entregar una ruta a
    /// la shell (con el handle ya soltado: la ventana que el diseño de la Papelera acota por el destino).
    static HOOKS: Mutex<Vec<(u64, u8, PathKey, Hook)>> = Mutex::new(Vec::new());
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

    /// Quita el gancho al soltarla.
    pub struct HookGuard(u64);

    /// Instala un gancho que solo se ejecuta para rutas bajo `root`, antes de abrirlas.
    pub fn install(root: PathBuf, f: impl Fn(&Path) + Send + Sync + 'static) -> HookGuard {
        install_at(0, root, f)
    }

    /// Instala un gancho que solo se ejecuta para rutas bajo `root`, justo antes de entregarlas a la shell.
    pub fn install_before_shell(
        root: PathBuf,
        f: impl Fn(&Path) + Send + Sync + 'static,
    ) -> HookGuard {
        install_at(1, root, f)
    }

    fn install_at(
        stage: u8,
        root: PathBuf,
        f: impl Fn(&Path) + Send + Sync + 'static,
    ) -> HookGuard {
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        HOOKS.lock().unwrap_or_else(|e| e.into_inner()).push((
            id,
            stage,
            PathKey::new(&root),
            Arc::new(f),
        ));
        HookGuard(id)
    }

    impl Drop for HookGuard {
        fn drop(&mut self) {
            HOOKS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|h| h.0 != self.0);
        }
    }

    fn run(stage: u8, p: &Path) {
        let key = PathKey::new(p);
        let hooks: Vec<Hook> = HOOKS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(_, st, root, _)| *st == stage && key.is_same_or_under(root))
            .map(|(_, _, _, h)| h.clone())
            .collect();
        for h in hooks {
            h(p);
        }
    }

    pub fn before_open(p: &Path) {
        run(0, p);
    }

    pub fn before_shell(p: &Path) {
        run(1, p);
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
