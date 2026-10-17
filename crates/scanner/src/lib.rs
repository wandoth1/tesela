//! Parallel NTFS/ReFS volume scanner for Windows (PLAN.md §3.3). READ-ONLY: it never writes or deletes anything.
//!
//! * Enumeration with `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)` (logical size, `AllocationSize`, and
//!   batched `FileId`s), with `FindFirstFileExW` as a fallback (estimated allocation, no hard-link deduplication, and no
//!   priority queue because there are no `FileId`s to adopt).
//! * Work stealing with `crossbeam-deque`, one task per directory; send one `Batch` per directory to a collector
//!   thread that feeds the `TreeBuilder` in `tesela-model`.
//! * Priority queue for known paths (temporary files, caches, the user's Recycle Bin, Downloads), with adoption by
//!   `FileId` along the ancestor chain.
//! * Reparse points are not followed. The root is not followed either: if the root is a junction, directory symlink, or mount point,
//!   reject it with an error (open it without following and classify it BEFORE resolving its target); also discard a priority path
//!   of this kind without opening its target. Cloud placeholders carry the `CLOUD` flag;
//!   paths use the `\\?\` prefix.
//! * Cancellation ("Stop and view"): build the tree from the entries received, and mark untraversed directories
//!   `INCOMPLETE`.
//!
//! ```no_run
//! use tesela_scanner::{ScanOptions, scan};
//! let result = scan(ScanOptions::new(r"C:\")).unwrap();
//! println!("{} archivos, {} bytes en disco", result.tree.stats().files, result.tree.node(0).alloc);
//! ```

mod browser;
mod classify;
mod diagnostic;
mod known;
mod paths;
mod priority;
mod progress;
mod sys;
mod walker;

pub use browser::{
    BrowserFamily, BrowserPath, BrowserRoots, browser_cache_dirs, browser_cache_dirs_verified,
    classify_browser_path,
};
pub use diagnostic::{
    CRASH_DUMPS_REL, WER_REPORT_DIRS, WER_ZONE_REL, diagnostic_dirs, diagnostic_dirs_verified,
};
pub use known::{
    KnownKind, current_user_sid, downloads_dir, known_folder, known_paths, known_paths_labeled,
};
pub use progress::{PriorityTarget, Progress, TopLevel};

use crossbeam_deque::{Stealer, Worker};
use paths::{drive_of, drive_root};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tesela_model::{Batch, BuildError, EntryData, FinishOptions, Tree, TreeBuilder};
use walker::{NO_PRIO, Shared, Task, WorkerOut, WorkerState};

/// Default thread limit: phase 0 measurements showed gains level off between 8 and 16 threads
/// (docs/fase-0-conclusiones.md).
pub const MAX_DEFAULT_THREADS: usize = 16;

/// Default thread count: `min(16, 2 × available logical cores)`.
pub fn default_threads() -> usize {
    let cores = std::thread::available_parallelism().map_or(2, std::num::NonZero::get);
    MAX_DEFAULT_THREADS.min(2 * cores)
}

/// Scan options.
#[derive(Clone, Debug)]
pub struct ScanOptions {
    /// Folder or volume to scan.
    pub root: PathBuf,
    /// Scanner thread count (defaults to [`default_threads`]).
    pub threads: usize,
    /// Use the priority queue for known paths (enabled by default). Only applies to paths under `root`.
    pub known_paths: bool,
    /// Replace the entire priority path list (ignores `known_paths`). For tests and tools.
    pub priority_paths: Option<Vec<PathBuf>>,
    /// Force `FindFirstFileExW` for the entire scan (to test the fallback).
    pub force_find: bool,
    /// Deduplicate hard links by `FileId` (enabled by default; `FindFirstFileExW` never provides `FileId`).
    pub dedup_hard_links: bool,
    /// Calculate "unexplained space" for a completed scan of a full volume (enabled by default).
    pub volume_gap: bool,
    /// "Now" in Unix seconds for age ranges (defaults to the system clock).
    pub now_unix: Option<u32>,
    /// External cancellation flag (for example, one from a Ctrl+C handler or one the interface creates BEFORE
    /// starting the startup thread). Checked between startup phases (resolving known paths) and by worker
    /// threads. If already set at startup, only the root is listed. [`ScanHandle::cancel`] sets the same flag.
    pub cancel: Option<Arc<AtomicBool>>,
    /// For tests and tools only: substitute the volume space and treat the path as a full volume,
    /// allowing end-to-end tests of "unexplained space" calculations on a synthetic tree.
    pub volume_override: Option<VolumeSpace>,
}

impl ScanOptions {
    /// Default options for scanning `root`.
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

/// Scan errors are TYPED and contain no user-facing text (the caller translates them); `Display` is a
/// neutral technical string for logs and tools, while system details are in the `io::Error`.
#[derive(Debug)]
pub enum ScanError {
    /// The root does not exist or cannot be opened.
    Root(std::io::Error),
    /// The root is not a directory.
    NotADirectory(PathBuf),
    /// The root is a link (junction, directory symlink, or mount point), and links are not followed: select the target
    /// folder instead. Contains the link's own path.
    RootIsLink(PathBuf),
    /// A thread panicked (a program error).
    Panicked,
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::Root(e) => write!(f, "cannot open the scan root: {e}"),
            ScanError::NotADirectory(_) => write!(f, "the scan root is not a directory"),
            ScanError::RootIsLink(_) => write!(
                f,
                "the scan root is a link (junction, symbolic link or mount point) and links are not followed"
            ),
            ScanError::Panicked => write!(f, "a scanner thread panicked"),
        }
    }
}

impl std::error::Error for ScanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ScanError::Root(e) => Some(e),
            _ => None,
        }
    }
}

/// Enumeration API used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiUsed {
    /// `FileIdExtdDirectoryInfo` for every directory.
    Extended,
    /// `FindFirstFileExW` for the entire scan (estimated allocation, no deduplication or priority queue).
    FindFallback,
    /// The extended API, except for directories where it was unsupported.
    Mixed,
}

/// Space information for the scanned path's volume, expressed as three values from distinct scopes (never mixed):
/// volume capacity and free space, and space available to the USER (disk quota). "Unexplained space" uses only the two
/// volume values (`total - free`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeSpace {
    /// Capacity in bytes: the volume's when `volume_scope`; otherwise, the user's quota.
    pub total: u64,
    /// Free bytes: for the entire volume when `volume_scope`; otherwise, those remaining for the user.
    pub free: u64,
    /// Bytes available to the scanning user (user scope): less than `free` when a disk quota applies.
    pub available: u64,
    /// Whether `total` and `free` describe the VOLUME (and can be compared with the scan). False when a quota applies and
    /// Windows cannot provide the actual capacity (`FileFsFullSizeInformationEx` is unsupported): then `total` and `free`
    /// are user-scoped and "unexplained space" is NOT calculated (subtracting different scopes would invent a residual).
    pub volume_scope: bool,
}

impl VolumeSpace {
    /// For a volume without a quota, all free space is available to the user.
    pub fn new(total: u64, free: u64) -> VolumeSpace {
        VolumeSpace {
            total,
            free,
            available: free,
            volume_scope: true,
        }
    }

    /// Whether a disk quota limits what the user can use.
    pub fn quota_limited(&self) -> bool {
        !self.volume_scope || self.available < self.free
    }

    /// Used space on the volume (`total - free`), the only value comparable with the scan.
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }
}

impl From<sys::VolumeSizes> for VolumeSpace {
    fn from(v: sys::VolumeSizes) -> VolumeSpace {
        VolumeSpace {
            total: v.total,
            free: v.free,
            available: v.available,
            volume_scope: v.volume_scope,
        }
    }
}

/// Space on the volume containing `path` (volume capacity and free space, plus space available to the user). READ-ONLY.
/// Network drives or unresponsive media may block: call from a thread with a timeout.
pub fn volume_space(path: &Path) -> Result<VolumeSpace, std::io::Error> {
    sys::volume_space(&sys::wide(path))
        .map(VolumeSpace::from)
        .map_err(io_err)
}

/// Result for a priority path.
#[derive(Clone, Debug)]
pub struct PriorityResult {
    /// Canonical path.
    pub path: PathBuf,
    /// Whether the entire path was traversed.
    pub done: bool,
    /// Observed allocated bytes (hard links are not deduplicated).
    pub alloc: u64,
    /// Archivos vistos.
    pub files: u64,
}

/// Scan statistics that are not part of the tree.
#[derive(Clone, Debug)]
pub struct ScanSummary {
    /// Scanned path as displayed (without `\\?\` when unnecessary).
    pub root: PathBuf,
    /// Scan was cancelled ("Stop and view"): the tree contains `INCOMPLETE` directories and unexplained space is unavailable.
    pub cancelled: bool,
    /// Time from start until enumeration finished (including listing the root and resolving priority paths).
    pub elapsed_scan: Duration,
    /// Tiempo de `TreeBuilder::finish`.
    pub elapsed_build: Duration,
    /// Hilos usados.
    pub threads: usize,
    /// Enumeration API.
    pub api: ApiUsed,
    /// Volume file system (`NTFS`, `ReFS`, `exFAT`, etc.).
    pub fs_name: String,
    /// Directories that could not be listed, grouped by Windows error code.
    pub errors_by_code: BTreeMap<u32, u64>,
    /// First directories that failed (error code and path), for diagnostics.
    pub error_samples: Vec<(u32, PathBuf)>,
    /// Volume space information, when provided by Windows.
    pub volume: Option<VolumeSpace>,
    /// Whether the scanned path is a drive root.
    pub full_volume: bool,
    /// Rutas prioritarias recorridas.
    pub priority: Vec<PriorityResult>,
    /// Provisional IDs were exhausted (more than 4 billion entries): the tree is incomplete.
    pub ids_exhausted: bool,
    /// Builder error (not expected: it would mean more than 4 billion entries or 4 GiB of names).
    pub build_error: Option<BuildError>,
}

impl ScanSummary {
    /// Total number of directories with errors.
    pub fn errors(&self) -> u64 {
        self.errors_by_code.values().sum()
    }
}

/// Tree and statistics for a scan.
pub struct ScanResult {
    /// The final tree.
    pub tree: Tree,
    /// Statistics not included in the tree.
    pub summary: ScanSummary,
}

/// A scan in progress.
pub struct ScanHandle {
    progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
    join: JoinHandle<ScanResult>,
}

impl ScanHandle {
    /// Live counters (atomic).
    pub fn progress(&self) -> &Arc<Progress> {
        &self.progress
    }

    /// Requests cancellation ("Stop and view"): workers finish their current directory and the tree is built from received data.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Whether the coordinator thread has finished.
    pub fn is_finished(&self) -> bool {
        self.join.is_finished()
    }

    /// Waits for the result.
    pub fn wait(self) -> Result<ScanResult, ScanError> {
        self.join.join().map_err(|_| ScanError::Panicked)
    }
}

/// Scan and wait for the result.
pub fn scan(opts: ScanOptions) -> Result<ScanResult, ScanError> {
    start(opts)?.wait()
}

fn io_err(code: u32) -> std::io::Error {
    std::io::Error::from_raw_os_error(code as i32)
}

/// Start a scan. Preparation (opening and listing the root, resolving priority paths) is synchronous, so

/// root errors are returned here; the rest runs on worker threads and is accessed through [`ScanHandle`].
///
/// The user-provided path is opened WITHOUT following its final reparse point and classified with `classify::root_plan`
/// BEFORE resolving anything: a root that is a junction, directory symlink, or mount point is REJECTED
/// (`ScanError::RootIsLink`) instead of traversing its target, just as links beneath the root are not followed. Only then
/// is the canonical path obtained FROM THAT SAME handle (without a second open that follows the link and without a race
/// between checking and resolving). Cloud folders are traversed through their filter as usual; the system resolves any
/// intermediate link components (this is a path, not a selected link). A volume mounted in a folder is scanned by its drive
/// letter or `\\?\Volume{...}\` path, not through the mount folder.
pub fn start(opts: ScanOptions) -> Result<ScanHandle, ScanError> {
    let t0 = Instant::now();
    let sys::Opened {
        handle: attr_h,
        meta: root_meta,
        path: root,
    } = sys::open_resolved(&opts.root).map_err(|c| ScanError::Root(io_err(c)))?;
    if !classify::is_dir(root_meta.attrs) {
        return Err(ScanError::NotADirectory(root));
    }
    let plan = classify::root_plan(root_meta.attrs, root_meta.reparse_tag);
    if plan.zero_size {
        return Err(ScanError::RootIsLink(root));
    }
    let root_w = sys::wide(&root);
    let display_root = dunce::simplified(&root).to_path_buf();
    let full_volume = drive_root(&root).is_some() || opts.volume_override.is_some();

    let fs_name = sys::fs_name(&attr_h).unwrap_or_default();
    drop(attr_h);
    // Only NTFS and ReFS have stable FileIds and hard links: on other file systems, do not store FileIds.
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

    // Root node (id 0) and root listing, on this thread.
    let root_id = sh.reserve_ids(1);
    debug_assert_eq!(root_id, 0);
    // The root uses the SAME classification as descendants (`plan`, computed above): only regular and cloud folders
    // reach this point; links have already been rejected.
    let mut root_batch = Batch::detached(root_id);
    root_batch.push_utf16(
        0,
        &sys::wide(&display_root),
        EntryData {
            flags: plan.flags,
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
        cloud: plan.open_cloud,
    };
    let mut st = WorkerState::new();
    // A root that must not be listed (an unpopulated cloud folder) gets no listing batch: the builder marks it
    // `INCOMPLETE`, as it does for such a descendant.
    let root_stats = if plan.walk {
        walker::process_dir(&sh, &tx, &root_task, true, &mut st)
            .map_err(|c| ScanError::Root(io_err(c)))?
    } else {
        walker::DirStats::default()
    };
    let mut top = vec![TopLevel::new(OsString::new())];
    top.extend(st.top_names.drain(..).map(TopLevel::new));
    if st.top_overflow {
        top.push(TopLevel::new(OsString::from("(otras carpetas)")));
    }
    let _ = progress.top.set(top);
    walker::account(&sh, &root_task, &root_stats);
    let mut children = std::mem::take(&mut st.children);

    // Priority queue (requires FileId; adoption is not possible with FindFirstFileExW). This is the most expensive startup
    // phase (canonicalizes and queries each known path): if cancellation was requested, skip it and continue with received data.
    let find_mode = sh.find_mode.load(Ordering::Relaxed);
    let mut resolved = priority::Resolved::default();
    // Without stored `FileId`s (neither NTFS nor ReFS), adoption is impossible: the priority queue relies on them.
    if store_ids
        && !find_mode
        && !cancel.load(Ordering::SeqCst)
        && (opts.known_paths || opts.priority_paths.is_some())
    {
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
                    cloud: t.cloud,
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

/// Everything the coordinator thread needs.
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

/// Coordinator thread: starts the collector and workers, waits, and builds the tree.
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

    // "Stop and view": cancellation occurred AND work remained. `pending == 0` is not enough: cancellation discards newly
    // discovered subdirectories without counting them (`dropped`).
    let ids_exhausted = sh.id_overflow.load(Ordering::SeqCst);
    let cancel_flag = cancel.load(Ordering::SeqCst);
    let unfinished = sh.pending.load(Ordering::SeqCst) != 0 || sh.dropped.load(Ordering::SeqCst);
    let mut cancelled = ids_exhausted || (cancel_flag && unfinished);
    let volume = volume_override.or_else(|| sys::volume_space(&root_w).ok().map(VolumeSpace::from));
    let mut fo = FinishOptions::new(now_unix);
    fo.dedup_hard_links = dedup;
    if full_volume && volume_gap && !cancelled && build_err.is_none() {
        // Use volume-scoped values only (with a quota and no Ex information class, `total` and `free` are user-scoped: no residual).
        fo.volume_used = volume
            .filter(|v| v.volume_scope)
            .map(|v| v.total.saturating_sub(v.free));
    }
    let t1 = Instant::now();
    let tree = builder.finish(&fo);
    let elapsed_build = t1.elapsed();
    // Second safeguard: if cancellation left directories unlisted in the tree, the scan also counts as stopped.
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
