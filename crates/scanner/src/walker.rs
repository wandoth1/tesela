//! Motor del escaneo: work-stealing con una tarea por directorio, cola prioritaria y un `Batch` por directorio hacia el
//! hilo colector (PLAN.md §3.3). Sin `unsafe` ni arena compartida: lo único compartido son contadores atómicos y las
//! colas de `crossbeam-deque`.

use crate::classify;
use crate::progress::Progress;
use crate::sys::{self, DirHandle, FILE_LIST_DIRECTORY, RawEntry};
use crossbeam_channel::Sender;
use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use tesela_model::{Batch, EntryData, flags};

/// Marca de "esta tarea no pertenece a ninguna ruta prioritaria".
pub(crate) const NO_PRIO: u16 = u16::MAX;

/// Códigos de Windows con los que `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)` indica que el sistema o
/// el sistema de archivos no soportan esa clase (87 = parámetro inválido, 1 = función inválida, 50 = no soportado,
/// 124 = nivel inválido).
const UNSUPPORTED_CODES: [u32; 4] = [87, 1, 50, 124];

/// Un directorio por procesar.
pub(crate) struct Task {
    /// Ruta con prefijo `\\?\` y sin NUL final.
    pub path: Vec<u16>,
    /// Id provisional del directorio (ya reservado).
    pub id: u32,
    /// Carpeta de primer nivel a la que pertenece (0 = archivos de la raíz).
    pub tag: u16,
    /// Índice de la ruta prioritaria a la que pertenece, o [`NO_PRIO`].
    pub prio: u16,
    /// Tarea inicial de una ruta prioritaria: hay que reclamarla antes de recorrerla (el recorrido general puede
    /// haber llegado antes y adoptado ya ese directorio).
    pub claim: bool,
}

/// Qué es un directorio cuyo `FileId` conocemos de antemano (cadena de ancestros de las rutas prioritarias).
#[derive(Clone, Copy)]
pub(crate) enum AdoptKind {
    /// Ancestro: el recorrido general lo adopta Y lo recorre.
    Ancestor,
    /// Ruta prioritaria `idx`: se adopta y solo se recorre si nadie la ha reclamado ya.
    Target(u16),
}

#[derive(Clone, Copy)]
pub(crate) struct Adopt {
    pub id: u32,
    pub tag: u16,
    pub kind: AdoptKind,
}

/// Estado compartido entre hilos.
pub(crate) struct Shared {
    /// Siguiente id provisional (se reserva por bloques con `fetch_add`).
    pub ids: AtomicU32,
    pub injector: Injector<Task>,
    pub prio: Injector<Task>,
    /// Cota superior de las tareas en `prio` (evita consultar la cola cuando está vacía).
    pub prio_hint: AtomicUsize,
    /// Tareas creadas y aún no terminadas (el escaneo acaba cuando llega a 0).
    pub pending: AtomicUsize,
    pub cancel: Arc<AtomicBool>,
    pub progress: Arc<Progress>,
    pub adopt: OnceLock<HashMap<u128, Adopt>>,
    /// Una bandera por ruta prioritaria: quién la recorre.
    pub claimed: OnceLock<Vec<AtomicBool>>,
    /// Se usa `FindFirstFileExW` para todo el escaneo (la extendida no está soportada o se forzó).
    pub find_mode: AtomicBool,
    /// Algún directorio se listó con `FindFirstFileExW`.
    pub any_find: AtomicBool,
    pub cluster: u64,
    /// Guardar el `FileId` de los archivos (NTFS/ReFS con la API extendida).
    pub store_ids: bool,
    /// Los ids provisionales se agotaron.
    pub id_overflow: AtomicBool,
}

impl Shared {
    pub fn new(
        cancel: Arc<AtomicBool>,
        progress: Arc<Progress>,
        cluster: u64,
        store_ids: bool,
    ) -> Shared {
        Shared {
            ids: AtomicU32::new(0),
            injector: Injector::new(),
            prio: Injector::new(),
            prio_hint: AtomicUsize::new(0),
            pending: AtomicUsize::new(0),
            cancel,
            progress,
            adopt: OnceLock::new(),
            claimed: OnceLock::new(),
            find_mode: AtomicBool::new(false),
            any_find: AtomicBool::new(false),
            cluster,
            store_ids,
            id_overflow: AtomicBool::new(false),
        }
    }

    /// Reserva `n` ids provisionales consecutivos y devuelve el primero.
    pub fn reserve_ids(&self, n: u32) -> u32 {
        let base = self.ids.fetch_add(n, Ordering::SeqCst);
        if base.checked_add(n).is_none_or(|end| end >= u32::MAX - 1) {
            self.id_overflow.store(true, Ordering::SeqCst);
            self.cancel.store(true, Ordering::SeqCst);
        }
        base
    }

    fn target_claimed(&self, idx: u16) -> &AtomicBool {
        &self
            .claimed
            .get()
            .expect("claimed inicializado antes de los hilos")[idx as usize]
    }
}

/// Entrada ya leída y convertida, a la espera de recibir su id.
struct Staged {
    name_off: u32,
    name_len: u16,
    attrs: u32,
    logical: u64,
    alloc: u64,
    file_id: u128,
    reparse_tag: u32,
    mtime: u32,
}

/// Cifras de un directorio recién procesado.
#[derive(Default)]
pub(crate) struct DirStats {
    pub files: u64,
    pub dirs: u64,
    pub alloc: u64,
}

/// Estado privado de un hilo.
pub(crate) struct WorkerState {
    buf: Vec<u64>,
    entries: Vec<Staged>,
    names: Vec<u16>,
    /// Subdirectorios descubiertos por la última llamada a `process_dir`.
    pub children: Vec<Task>,
    /// Solo se rellena al procesar la raíz: nombres de las carpetas de primer nivel (índice = etiqueta - 1).
    pub top_names: Vec<OsString>,
    pub errors: BTreeMap<u32, u64>,
    pub error_samples: Vec<(u32, Vec<u16>)>,
}

impl WorkerState {
    pub fn new() -> WorkerState {
        WorkerState {
            buf: vec![0u64; sys::BUF_BYTES / 8],
            entries: Vec::new(),
            names: Vec::new(),
            children: Vec::new(),
            top_names: Vec::new(),
            errors: BTreeMap::new(),
            error_samples: Vec::new(),
        }
    }

    fn note_error(&mut self, code: u32, path: &[u16]) {
        *self.errors.entry(code).or_default() += 1;
        if self.error_samples.len() < 10 {
            self.error_samples.push((code, path.to_vec()));
        }
    }
}

fn join(parent: &[u16], name: &[u16]) -> Vec<u16> {
    let mut v = Vec::with_capacity(parent.len() + name.len() + 1);
    v.extend_from_slice(parent);
    if v.last() != Some(&(b'\\' as u16)) {
        v.push(b'\\' as u16);
    }
    v.extend_from_slice(name);
    v
}

/// Convierte una entrada leída y la deja en la lista de espera.
fn stage(entries: &mut Vec<Staged>, names: &mut Vec<u16>, cluster: u64, e: &RawEntry) {
    let off = names.len() as u32;
    names.extend_from_slice(e.name);
    let is_dir = classify::is_dir(e.attrs);
    let alloc = match e.alloc {
        Some(a) => a,
        // Respaldo: sin AllocationSize, EOF redondeado a cluster (los directorios no tienen estimación).
        None if is_dir => 0,
        None => classify::estimate_alloc(e.logical, cluster),
    };
    entries.push(Staged {
        name_off: off,
        name_len: e.name.len() as u16,
        attrs: e.attrs,
        logical: e.logical,
        alloc,
        file_id: e.file_id,
        reparse_tag: e.reparse_tag,
        mtime: classify::filetime_to_unix(e.mtime_filetime),
    });
}

/// Lista un directorio y deja las entradas en `entries`/`names`. Devuelve el código de error de Windows si falla.
fn list_dir(sh: &Shared, path: &[u16], is_root: bool, st: &mut WorkerState) -> Result<(), u32> {
    let WorkerState {
        buf,
        entries,
        names,
        ..
    } = st;
    entries.clear();
    names.clear();
    let cluster = sh.cluster;
    if !sh.find_mode.load(Ordering::Relaxed) {
        let h: DirHandle = sys::open_dir(path, FILE_LIST_DIRECTORY)?;
        match sys::enum_dir_extd(&h, buf, &mut |e| stage(entries, names, cluster, e)) {
            Ok(()) => return Ok(()),
            Err(code) if UNSUPPORTED_CODES.contains(&code) => {
                // La clase extendida no sirve aquí. Si es la raíz, vale para todo el escaneo; si no, solo para este
                // directorio (un sistema de archivos raro bajo la raíz).
                if is_root {
                    sh.find_mode.store(true, Ordering::Relaxed);
                }
                sh.any_find.store(true, Ordering::Relaxed);
                entries.clear();
                names.clear();
            }
            Err(code) => return Err(code),
        }
    } else {
        sh.any_find.store(true, Ordering::Relaxed);
    }
    sys::enum_dir_find(path, &mut |e| stage(entries, names, cluster, e))
}

/// Procesa un directorio: lo lista, entrega su `Batch` al colector y deja en `st.children` los subdirectorios.
/// Siempre se entrega un lote (también si el directorio está vacío o falla), para que el builder distinga un
/// directorio vacío de uno que no se llegó a listar.
pub(crate) fn process_dir(
    sh: &Shared,
    tx: &Sender<Batch>,
    task: &Task,
    is_root: bool,
    st: &mut WorkerState,
) -> Result<DirStats, u32> {
    st.children.clear();
    if let Err(code) = list_dir(sh, &task.path, is_root, st) {
        if !is_root {
            st.note_error(code, &task.path);
            sh.progress.errors.fetch_add(1, Ordering::Relaxed);
            let _ = tx.send(Batch::failed(task.id, code));
        }
        return Err(code);
    }
    let n = st.entries.len();
    let base = sh.reserve_ids(n as u32);
    let mut batch = Batch::listing_with_capacity(task.id, base, n, st.names.len());
    let mut stats = DirStats::default();
    let adopt = sh.adopt.get().filter(|m| !m.is_empty());
    let WorkerState {
        entries,
        names,
        children,
        top_names,
        ..
    } = st;
    for e in entries.iter() {
        let name = &names[e.name_off as usize..e.name_off as usize + e.name_len as usize];
        let mut fl = classify::entry_flags(e.attrs, e.reparse_tag);
        let is_dir = fl & flags::DIR != 0;
        if is_dir && fl & flags::REPARSE != 0 {
            // Junction, symlink o punto de montaje de directorio: no se sigue y mide 0.
            batch.push_utf16(
                task.id,
                name,
                EntryData {
                    flags: fl,
                    mtime: e.mtime,
                    ..EntryData::default()
                },
            );
            continue;
        }
        if !is_dir {
            let file_id = if sh.store_ids { e.file_id } else { 0 };
            batch.push_utf16(
                task.id,
                name,
                EntryData {
                    flags: fl,
                    alloc: e.alloc,
                    logical: e.logical,
                    mtime: e.mtime,
                    file_id,
                },
            );
            stats.files += 1;
            stats.alloc += e.alloc;
            continue;
        }
        // Directorio real.
        fl &= !flags::REPARSE;
        stats.dirs += 1;
        stats.alloc += e.alloc;
        let child_path = join(&task.path, name);
        let adopted = adopt.and_then(|m| (e.file_id != 0).then(|| m.get(&e.file_id)).flatten());
        let child_tag = if is_root {
            top_names.push(OsString::from_wide(name));
            top_names.len() as u16
        } else {
            task.tag
        };
        match adopted {
            None => {
                let id = batch.push_utf16(
                    task.id,
                    name,
                    EntryData {
                        flags: fl,
                        alloc: e.alloc,
                        logical: e.logical,
                        mtime: e.mtime,
                        file_id: 0,
                    },
                );
                if task.prio != NO_PRIO {
                    prio_inc(sh, task.prio);
                }
                children.push(Task {
                    path: child_path,
                    id,
                    tag: child_tag,
                    prio: task.prio,
                    claim: false,
                });
            }
            Some(a) => {
                // Este directorio ya tiene nodo (precreado por la cola prioritaria): se deja un hueco en su lugar.
                batch.push_hole();
                match a.kind {
                    AdoptKind::Ancestor => {
                        children.push(Task {
                            path: child_path,
                            id: a.id,
                            tag: a.tag,
                            prio: NO_PRIO,
                            claim: false,
                        });
                    }
                    AdoptKind::Target(idx) => {
                        // Se cuenta ANTES de reclamar: si la tarea inicial de esta ruta falla su reclamación y
                        // termina entre medias, el contador no puede llegar a 0 y marcarla como hecha antes de tiempo.
                        prio_inc(sh, idx);
                        if sh.target_claimed(idx).swap(true, Ordering::AcqRel) {
                            prio_dec(sh, idx);
                        } else {
                            children.push(Task {
                                path: child_path,
                                id: a.id,
                                tag: a.tag,
                                prio: idx,
                                claim: false,
                            });
                        }
                    }
                }
            }
        }
    }
    if tx.send(batch).is_err() {
        sh.cancel.store(true, Ordering::SeqCst); // el colector ha muerto
    }
    Ok(stats)
}

/// Suma las cifras de un directorio a los contadores de progreso.
pub(crate) fn account(sh: &Shared, task: &Task, s: &DirStats) {
    let p = &sh.progress;
    p.files.fetch_add(s.files, Ordering::Relaxed);
    p.dirs.fetch_add(s.dirs, Ordering::Relaxed);
    p.alloc.fetch_add(s.alloc, Ordering::Relaxed);
    if let Some(t) = p.top.get().and_then(|v| v.get(task.tag as usize)) {
        t.files.fetch_add(s.files, Ordering::Relaxed);
        t.alloc.fetch_add(s.alloc, Ordering::Relaxed);
    }
    if task.prio != NO_PRIO
        && let Some(t) = p.priority.get().and_then(|v| v.get(task.prio as usize))
    {
        t.files.fetch_add(s.files, Ordering::Relaxed);
        t.alloc.fetch_add(s.alloc, Ordering::Relaxed);
    }
}

/// Cuenta una tarea más pendiente en la ruta prioritaria `idx`.
fn prio_inc(sh: &Shared, idx: u16) {
    if let Some(t) = sh.progress.priority.get().and_then(|v| v.get(idx as usize)) {
        t.pending.fetch_add(1, Ordering::AcqRel);
    }
}

/// Descuenta una tarea de la ruta prioritaria `idx`; si era la última, la ruta está hecha.
fn prio_dec(sh: &Shared, idx: u16) {
    if let Some(t) = sh.progress.priority.get().and_then(|v| v.get(idx as usize))
        && t.pending.fetch_sub(1, Ordering::AcqRel) == 1
    {
        t.done.store(true, Ordering::Release);
    }
}

/// Marca una tarea como terminada (y su ruta prioritaria como hecha si era la última).
fn finish_task(sh: &Shared, task: &Task) {
    if task.prio != NO_PRIO {
        prio_dec(sh, task.prio);
    }
    sh.pending.fetch_sub(1, Ordering::SeqCst);
}

/// Publica las tareas hijas: se cuentan ANTES de terminar la tarea padre, así `pending == 0` solo ocurre cuando de
/// verdad no queda trabajo. Las de rutas prioritarias van a la cola prioritaria compartida; el resto, a la cola local.
fn push_children(sh: &Shared, local: &Worker<Task>, children: &mut Vec<Task>) {
    sh.pending.fetch_add(children.len(), Ordering::SeqCst);
    for c in children.drain(..) {
        if c.prio != NO_PRIO {
            sh.prio_hint.fetch_add(1, Ordering::Relaxed);
            sh.prio.push(c);
        } else {
            local.push(c);
        }
    }
}

fn next_task(
    sh: &Shared,
    local: &Worker<Task>,
    stealers: &[Stealer<Task>],
    me: usize,
) -> Option<Task> {
    if sh.prio_hint.load(Ordering::Relaxed) > 0 {
        loop {
            match sh.prio.steal() {
                Steal::Success(t) => {
                    sh.prio_hint.fetch_sub(1, Ordering::Relaxed);
                    return Some(t);
                }
                Steal::Empty => break,
                Steal::Retry => continue,
            }
        }
    }
    local.pop().or_else(|| {
        std::iter::repeat_with(|| {
            sh.injector.steal_batch_and_pop(local).or_else(|| {
                stealers
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != me)
                    .map(|(_, s)| s.steal())
                    .collect::<Steal<Task>>()
            })
        })
        .find(|s| !s.is_retry())
        .and_then(Steal::success)
    })
}

/// Lo que devuelve cada hilo al terminar.
pub(crate) struct WorkerOut {
    pub errors: BTreeMap<u32, u64>,
    pub error_samples: Vec<(u32, Vec<u16>)>,
}

/// Bucle de un hilo de escaneo.
pub(crate) fn worker_loop(
    sh: &Shared,
    tx: Sender<Batch>,
    local: Worker<Task>,
    stealers: &[Stealer<Task>],
    me: usize,
) -> WorkerOut {
    let mut st = WorkerState::new();
    let mut idle = 0u32;
    loop {
        if sh.cancel.load(Ordering::Relaxed) {
            break;
        }
        match next_task(sh, &local, stealers, me) {
            Some(task) => {
                idle = 0;
                if task.claim && sh.target_claimed(task.prio).swap(true, Ordering::AcqRel) {
                    // El recorrido general llegó antes a esta carpeta y la está escaneando él.
                    finish_task(sh, &task);
                    continue;
                }
                if let Ok(stats) = process_dir(sh, &tx, &task, false, &mut st) {
                    account(sh, &task, &stats);
                    if !sh.cancel.load(Ordering::Relaxed) {
                        push_children(sh, &local, &mut st.children);
                    }
                }
                finish_task(sh, &task);
            }
            None => {
                if sh.pending.load(Ordering::SeqCst) == 0 {
                    break;
                }
                idle += 1;
                if idle < 64 {
                    std::hint::spin_loop();
                } else if idle < 2048 {
                    std::thread::yield_now();
                } else {
                    std::thread::sleep(std::time::Duration::from_micros(100));
                }
            }
        }
    }
    WorkerOut {
        errors: st.errors,
        error_samples: st.error_samples,
    }
}
