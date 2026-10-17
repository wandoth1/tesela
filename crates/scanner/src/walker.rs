//! Scan engine: work stealing with one task per directory, a priority queue, and one `Batch` per directory sent to the
//! collector thread (PLAN.md §3.3). No `unsafe` or shared arena: the only shared state is atomic counters and
//! `crossbeam-deque` queues.

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
use tesela_model::{Batch, EntryData};

/// Marker that this task does not belong to a priority path.
pub(crate) const NO_PRIO: u16 = u16::MAX;

/// Windows error codes indicating that `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)` is unsupported by the system or
/// file system (87 = invalid parameter, 1 = invalid function, 50 = not supported,
/// 124 = invalid level).
const UNSUPPORTED_CODES: [u32; 4] = [87, 1, 50, 124];

/// `ERROR_ARITHMETIC_OVERFLOW`: provisional ID counter exhausted.
const ERROR_ARITHMETIC_OVERFLOW: u32 = 534;

/// A directory to process.
pub(crate) struct Task {
    /// Path with the `\\?\` prefix and no trailing NUL.
    pub path: Vec<u16>,
    /// Reserved provisional directory ID.
    pub id: u32,
    /// Top-level folder this belongs to (0 = files in the root).
    pub tag: u16,
    /// Index of the priority path this belongs to, or [`NO_PRIO`].
    pub prio: u16,
    /// Initial task for a priority path: claim it before traversal (general traversal may

    /// have reached and adopted this directory already).
    pub claim: bool,
    /// Cloud folder (reparse point with a cloud tag): open it and let its filter handle access.
    pub cloud: bool,
}

/// Kind of directory whose `FileId` is known in advance (an ancestor chain of priority paths).
#[derive(Clone, Copy)]
pub(crate) enum AdoptKind {
    /// Ancestor: general traversal adopts AND traverses it.
    Ancestor,
    /// Priority path `idx`: adopt it and traverse only if no one has claimed it yet.
    Target(u16),
}

#[derive(Clone, Copy)]
pub(crate) struct Adopt {
    pub id: u32,
    pub tag: u16,
    pub kind: AdoptKind,
}

/// State shared between threads.
pub(crate) struct Shared {
    /// Next provisional ID (reserved in blocks with `fetch_add`).
    pub ids: AtomicU32,
    pub injector: Injector<Task>,
    pub prio: Injector<Task>,
    /// Upper bound on tasks in `prio` (avoids checking the queue when it is empty).
    pub prio_hint: AtomicUsize,
    /// Created tasks that have not finished (the scan ends when this reaches 0).
    pub pending: AtomicUsize,
    pub cancel: Arc<AtomicBool>,
    pub progress: Arc<Progress>,
    pub adopt: OnceLock<HashMap<u128, Adopt>>,
    /// One flag per priority path, indicating who is traversing it.
    pub claimed: OnceLock<Vec<AtomicBool>>,
    /// `FindFirstFileExW` is used for the whole scan (the extended API is unsupported or was forced).
    pub find_mode: AtomicBool,
    /// At least one directory was listed with `FindFirstFileExW`.
    pub any_find: AtomicBool,
    pub cluster: u64,
    /// Store file `FileId`s (NTFS/ReFS with the extended API).
    pub store_ids: bool,
    /// Provisional IDs were exhausted.
    pub id_overflow: AtomicBool,
    /// Pending subdirectories were dropped on cancellation: the tree remains incomplete even if `pending` reaches 0.
    pub dropped: AtomicBool,
    /// A worker thread panicked.
    pub panicked: AtomicBool,
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
            dropped: AtomicBool::new(false),
            panicked: AtomicBool::new(false),
        }
    }

    /// Reserve `n` consecutive provisional IDs and return the first.
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

/// Entry already read and converted, waiting to receive its ID.
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

/// Statistics for a newly processed directory.
#[derive(Default)]
pub(crate) struct DirStats {
    pub files: u64,
    pub dirs: u64,
    pub alloc: u64,
}

/// Per-thread private state.
pub(crate) struct WorkerState {
    buf: Vec<u64>,
    entries: Vec<Staged>,
    names: Vec<u16>,
    /// Subdirectories discovered by the last call to `process_dir`.
    pub children: Vec<Task>,
    /// Populated only when processing the root: names of top-level folders (index = label - 1).
    pub top_names: Vec<OsString>,
    /// There were more top-level folders than `MAX_TOP_NAMES`: the remaining folders share the last label.
    pub top_overflow: bool,
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
            top_overflow: false,
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

/// Top-level folders with their own accumulator. Tags are `u16`: if there are too many folders, the remaining ones share
/// a final "other" tag rather than wrapping around and being attributed to another folder.
pub(crate) const MAX_TOP_NAMES: usize = 65_000;

/// Tag for top-level folder `name` (index + 1; 0 means "files in the root").
fn top_tag(names: &mut Vec<OsString>, overflow: &mut bool, cap: usize, name: &[u16]) -> u16 {
    if names.len() < cap {
        names.push(OsString::from_wide(name));
        names.len() as u16
    } else {
        *overflow = true;
        (cap + 1) as u16
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

/// Convert a read entry and add it to the staging list.
fn stage(entries: &mut Vec<Staged>, names: &mut Vec<u16>, cluster: u64, e: &RawEntry) {
    let off = names.len() as u32;
    names.extend_from_slice(e.name);
    let is_dir = classify::is_dir(e.attrs);
    let alloc = match e.alloc {
        Some(a) => a,
        // Fallback: without AllocationSize, round EOF up to a cluster (directories have no estimate).
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

/// Lists a directory into `entries`/`names`. Returns the Windows error code on failure.
fn list_dir(
    sh: &Shared,
    path: &[u16],
    cloud: bool,
    is_root: bool,
    st: &mut WorkerState,
) -> Result<(), u32> {
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
        let h: DirHandle = sys::open_dir(path, FILE_LIST_DIRECTORY, cloud)?;
        match sys::enum_dir_extd(&h, buf, &mut |e| stage(entries, names, cluster, e)) {
            Ok(()) => return Ok(()),
            Err(code) if UNSUPPORTED_CODES.contains(&code) => {
                // The extended information class is unavailable here. If this is the root, it applies to the whole scan;
                // otherwise it applies only to this directory (an unusual file system beneath the root).
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

/// Process a directory: list it, send its `Batch` to the collector, and put its subdirectories in `st.children`.
/// Always send a batch (even if the directory is empty or listing fails) so the builder can distinguish
/// an empty directory from one that could not be listed.
pub(crate) fn process_dir(
    sh: &Shared,
    tx: &Sender<Batch>,
    task: &Task,
    is_root: bool,
    st: &mut WorkerState,
) -> Result<DirStats, u32> {
    st.children.clear();
    if let Err(code) = list_dir(sh, &task.path, task.cloud, is_root, st) {
        if !is_root {
            st.note_error(code, &task.path);
            sh.progress.errors.fetch_add(1, Ordering::Relaxed);
            let _ = tx.send(Batch::failed(task.id, code));
        }
        return Err(code);
    }
    let n = st.entries.len();
    let base = sh.reserve_ids(n as u32);
    if sh.id_overflow.load(Ordering::SeqCst) {
        // Provisional IDs are exhausted (the scan is already cancelled and will be marked accordingly): this directory
        // returns no batch because its IDs would wrap and collide with other nodes.
        return Err(ERROR_ARITHMETIC_OVERFLOW);
    }
    let mut batch = Batch::listing_with_capacity(task.id, base, n, st.names.len());
    let mut stats = DirStats::default();
    let adopt = sh.adopt.get().filter(|m| !m.is_empty());
    let WorkerState {
        entries,
        names,
        children,
        top_names,
        top_overflow,
        ..
    } = st;
    for e in entries.iter() {
        let name = &names[e.name_off as usize..e.name_off as usize + e.name_len as usize];
        let is_dir = classify::is_dir(e.attrs);
        let action = classify::dir_action(e.attrs, e.reparse_tag);
        let fl = if is_dir {
            classify::dir_flags(e.attrs, e.reparse_tag)
        } else {
            classify::entry_flags(e.attrs, e.reparse_tag)
        };
        // Populated cloud folders (OneDrive, etc.) are reparse points but ARE traversed: their filter allows listing.
        // Unpopulated folders are not (listing them could trigger network downloads).
        let cloud_dir = is_dir && action == classify::DirAction::WalkCloud;
        if is_dir && action == classify::DirAction::SkipReparse {
            // Directory junction, symlink, or mount point: do not follow; size is zero.
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
        if is_dir && action == classify::DirAction::SkipUnpopulatedCloud {
            // Not traversed: count its AllocationSize and let the builder mark it INCOMPLETE (its batch is never delivered).
            batch.push_utf16(
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
            stats.dirs += 1;
            stats.alloc += e.alloc;
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
        // Regular directory (or cloud folder).
        stats.dirs += 1;
        stats.alloc += e.alloc;
        let child_path = join(&task.path, name);
        let adopted = adopt.and_then(|m| (e.file_id != 0).then(|| m.get(&e.file_id)).flatten());
        let child_tag = if is_root {
            top_tag(top_names, top_overflow, MAX_TOP_NAMES, name)
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
                    cloud: cloud_dir,
                });
            }
            Some(a) => {
                // This directory already has a node (precreated by the priority queue): leave a placeholder in its position.
                batch.push_hole();
                match a.kind {
                    AdoptKind::Ancestor => {
                        children.push(Task {
                            path: child_path,
                            id: a.id,
                            tag: a.tag,
                            prio: NO_PRIO,
                            claim: false,
                            cloud: false,
                        });
                    }
                    AdoptKind::Target(idx) => {
                        // Count BEFORE claiming: if the initial task for this path fails to claim and finishes in between,
                        // the counter cannot reach 0 and mark the path complete too early.
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
                                cloud: false,
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

/// Adds a directory's statistics to the progress counters.
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

/// Increments the pending-task count for priority path `idx`.
fn prio_inc(sh: &Shared, idx: u16) {
    if let Some(t) = sh.progress.priority.get().and_then(|v| v.get(idx as usize)) {
        t.pending.fetch_add(1, Ordering::AcqRel);
    }
}

/// Decrements the task count for priority path `idx`; if it was the last one, the path is complete.
fn prio_dec(sh: &Shared, idx: u16) {
    if let Some(t) = sh.progress.priority.get().and_then(|v| v.get(idx as usize))
        && t.pending.fetch_sub(1, Ordering::AcqRel) == 1
    {
        t.done.store(true, Ordering::Release);
    }
}

/// Marks a task complete (and its priority path complete if it was the last task).
fn finish_task(sh: &Shared, task: &Task) {
    if task.prio != NO_PRIO {
        prio_dec(sh, task.prio);
    }
    sh.pending.fetch_sub(1, Ordering::SeqCst);
}

/// Publishes child tasks: they are counted BEFORE the parent task finishes, so `pending == 0` occurs only when no work
/// actually remains. Priority-path tasks go to the shared priority queue; the rest go to the local queue.
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

/// Data returned by each worker when it finishes.
pub(crate) struct WorkerOut {
    pub errors: BTreeMap<u32, u64>,
    pub error_samples: Vec<(u32, Vec<u16>)>,
}

/// Waits when a worker has no work: first spin, then yield, then sleep with an increasing delay
/// (from 50 µs to 3.2 ms) to avoid burning CPU while other workers finish large directories.
#[derive(Default)]
struct Backoff {
    step: u32,
}

impl Backoff {
    fn reset(&mut self) {
        self.step = 0;
    }

    fn wait(&mut self) {
        self.step = self.step.saturating_add(1);
        match self.step {
            1..=32 => std::hint::spin_loop(),
            33..=256 => std::thread::yield_now(),
            s => {
                let exp = (s - 256).min(6);
                std::thread::sleep(std::time::Duration::from_micros(50 << exp));
            }
        }
    }
}

/// Cancels the scan if dropped during a panic (unwinding).
struct PanicGuard<'a>(&'a Shared);

impl Drop for PanicGuard<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.panicked.store(true, Ordering::SeqCst);
            self.0.cancel.store(true, Ordering::SeqCst);
        }
    }
}

/// Bucle de un hilo de escaneo.
pub(crate) fn worker_loop(
    sh: &Shared,
    tx: Sender<Batch>,
    local: Worker<Task>,
    stealers: &[Stealer<Task>],
    me: usize,
) -> WorkerOut {
    // If this thread panics, cancel the scan so the others exit and the coordinator does not hang.
    // waiting for a task that no worker will finish.
    let _guard = PanicGuard(sh);
    let mut st = WorkerState::new();
    let mut backoff = Backoff::default();
    loop {
        if sh.cancel.load(Ordering::Relaxed) {
            break;
        }
        match next_task(sh, &local, stealers, me) {
            Some(task) => {
                backoff.reset();
                if task.claim && sh.target_claimed(task.prio).swap(true, Ordering::AcqRel) {
                    // The general traversal reached this folder first and is scanning it.
                    finish_task(sh, &task);
                    continue;
                }
                if let Ok(stats) = process_dir(sh, &tx, &task, false, &mut st) {
                    account(sh, &task, &stats);
                    if sh.cancel.load(Ordering::Relaxed) {
                        // Subdirectories are dropped: even if `pending` reaches 0, the tree remains incomplete.
                        if !st.children.is_empty() {
                            sh.dropped.store(true, Ordering::SeqCst);
                        }
                    } else {
                        push_children(sh, &local, &mut st.children);
                    }
                }
                finish_task(sh, &task);
            }
            None => {
                if sh.pending.load(Ordering::SeqCst) == 0 {
                    break;
                }
                backoff.wait();
            }
        }
    }
    WorkerOut {
        errors: st.errors,
        error_samples: st.error_samples,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> Shared {
        Shared::new(
            Arc::new(AtomicBool::new(false)),
            Arc::new(Progress::default()),
            4096,
            true,
        )
    }

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn las_etiquetas_de_primer_nivel_no_dan_la_vuelta_al_pasar_del_tope() {
        let (mut names, mut overflow) = (Vec::new(), false);
        let tags: Vec<u16> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|n| top_tag(&mut names, &mut overflow, 3, &w(n)))
            .collect();
        assert_eq!(
            tags,
            [1, 2, 3, 4, 4],
            "las sobrantes comparten la etiqueta cap + 1"
        );
        assert!(overflow);
        assert_eq!(names.len(), 3);
        // With the real limit, the highest label fits in `u16` without wrapping.
        assert!(MAX_TOP_NAMES < u16::MAX as usize);
    }

    #[test]
    fn agotar_los_ids_cancela_el_escaneo_y_lo_marca() {
        let sh = shared();
        assert_eq!(sh.reserve_ids(5), 0);
        assert!(!sh.id_overflow.load(Ordering::SeqCst));
        sh.ids.store(u32::MAX - 5, Ordering::SeqCst);
        sh.reserve_ids(10);
        assert!(sh.id_overflow.load(Ordering::SeqCst));
        assert!(sh.cancel.load(Ordering::SeqCst));
    }

    /// GM-m3: when IDs are exhausted, `process_dir` returns no batch (its IDs would wrap and collide with others).
    #[test]
    fn con_los_ids_agotados_no_se_entrega_ningun_lote() {
        let dir = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        std::fs::write(dir.join("b.txt"), b"y").unwrap();
        let sh = shared();
        let (tx, rx) = crossbeam_channel::unbounded::<Batch>();
        let task = Task {
            path: sys::wide(&dir),
            id: 0,
            tag: 0,
            prio: NO_PRIO,
            claim: false,
            cloud: false,
        };
        let mut st = WorkerState::new();
        // Con ids de sobra entrega su lote.
        assert!(process_dir(&sh, &tx, &task, true, &mut st).is_ok());
        assert!(rx.try_recv().is_ok());
        // At the counter limit: error, exhaustion flag, cancellation, and no batch.
        sh.ids.store(u32::MAX - 1, Ordering::SeqCst);
        let mut st = WorkerState::new();
        assert_eq!(
            process_dir(&sh, &tx, &task, true, &mut st).err(),
            Some(ERROR_ARITHMETIC_OVERFLOW)
        );
        assert!(sh.id_overflow.load(Ordering::SeqCst) && sh.cancel.load(Ordering::SeqCst));
        assert!(rx.try_recv().is_err(), "no se entrega ningún lote");
        assert!(st.children.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn un_panico_en_un_hilo_cancela_para_que_los_demas_salgan() {
        let sh = shared();
        let r = std::thread::scope(|s| {
            s.spawn(|| {
                let _guard = PanicGuard(&sh);
                panic!("pánico provocado por el test");
            })
            .join()
        });
        assert!(r.is_err());
        assert!(sh.panicked.load(Ordering::SeqCst));
        assert!(sh.cancel.load(Ordering::SeqCst));
        // Without a panic, dropping the guard does not cancel anything.
        let sh = shared();
        drop(PanicGuard(&sh));
        assert!(!sh.cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn la_espera_crece_hasta_un_tope_y_se_reinicia_al_encontrar_trabajo() {
        let mut b = Backoff::default();
        for _ in 0..300 {
            b.wait();
        }
        assert_eq!(b.step, 300);
        let t = std::time::Instant::now();
        b.wait(); // ya en la fase de sueño: 50 µs << 6 como mucho
        assert!(t.elapsed() < std::time::Duration::from_millis(500));
        b.reset();
        assert_eq!(b.step, 0);
    }
}
