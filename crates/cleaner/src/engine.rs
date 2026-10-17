//! Traversal: the SAME code measures, simulates, and deletes (PLAN.md §3.7.3 and §3.7.8).
//!
//! # Guarantees
//! * Everything is opened RELATIVE to the directory handle being traversed (`NtCreateFile` with `RootDirectory`): no
//!   path is resolved again from the root, so replacing an ancestor with a junction cannot redirect anything. This is the pattern
//!   used to rewrite `std::fs::remove_dir_all` after CVE-2022-21658.
//! * Each child is opened with `FILE_OPEN_REPARSE_POINT` and the TYPE reported by the listing (file or directory); if it changed, the
//!   system rejects it. Reparse points are never traversed or deleted (they are skipped and counted).
//! * The handle's `FileId` must match the listing and its volume must match the root; otherwise it "changed since
//!   confirmation" and is skipped. Files are opened EXCLUSIVELY (`dwShareMode = 0`): `SHARING_VIOLATION` means in use.
//! * Actions use that same handle (`FileDispositionInfoEx`), bottom-up: a directory is deleted only after its
//!   children finish, through its own handle (opened with full sharing so it can be listed even if Explorer or
//!   antivirus software has it open; identity is still verified).
//! * Protection is checked for EVERY child ([`Protection::child_action`]), not only when the basket is approved).
//! * Dedicated thread pool (`crossbeam-deque`, with per-thread LIFO stacks: depth-first traversal, so open handles
//!   are proportional to traversal depth per thread) and cancellation between entries.

use crate::paths::PathKey;
use crate::protect::{ChildAction, Protection, Verdict};
use crate::sys::{self, DirEntry, Expect, FileIdentity, Handle, Win32};
use crate::volume;
use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the traversal does with the entries it finds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Counts only (shared, read-only opens): used to estimate during approval.
    Measure,
    /// Does EVERYTHING except marking entries for deletion: opens exclusively, verifies identities, counts, and records what it would delete.
    DryRun,
    /// Deletes.
    Delete,
}

/// Counters for a work unit (atomic: updated by the pool threads and read by the interface).
#[derive(Debug, Default)]
pub struct Stats {
    /// On-disk bytes deleted (or that would be deleted), EXCLUDING shared hard links.
    pub bytes: AtomicU64,
    /// Logical size of deleted items (for checking fit in the Recycle Bin).
    pub logical: AtomicU64,
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    /// Files with more than one link: one name is deleted, but no space is freed.
    pub shared_files: AtomicU64,
    pub shared_bytes: AtomicU64,
    /// `ERROR_SHARING_VIOLATION`: in use; skipped and counted.
    pub in_use: AtomicU64,
    /// `ERROR_ACCESS_DENIED`.
    pub denied: AtomicU64,
    /// Identity or type differs from what was listed or approved.
    pub changed: AtomicU64,
    /// Protected entries that were preserved.
    pub protected: AtomicU64,
    pub protected_bytes: AtomicU64,
    /// Too recent (`min_age`).
    pub young: AtomicU64,
    /// Reparse points: never traversed or deleted.
    pub reparse: AtomicU64,
    /// Other system errors.
    pub other_errors: AtomicU64,
    /// Directories that could not be deleted because they were not empty (expected when something was skipped).
    pub not_empty: AtomicU64,
    /// Cloud files inside (skipped because they are reparse points).
    pub cloud: AtomicU64,
    /// Entries that were gone when opened (listed, then disappeared before they were reached). This is normal during deletion;
    /// during MEASURE it means the directory changed during traversal (see [`StatsSnapshot::is_complete`]).
    pub gone: AtomicU64,
    /// FINGERPRINT of the observed contents, independent of ORDER (A-M1): wrapping sum and XOR of a 64-bit hash per entry
    /// (128-bit `FileId` identity, type, sizes, and links). O(1) memory. Traversals of the same tree produce the same
    /// fingerprint even if threads visit it in a different order; removing a child and creating another of the same size changes it (different `FileId`).
    pub fp_sum: AtomicU64,
    pub fp_xor: AtomicU64,
    /// Length (UTF-16 units) of the longest path inside (GM-M3): the shell does not support paths of 260 or more units,
    /// so a folder containing such a descendant cannot be moved to the Recycle Bin as a whole.
    pub max_path: AtomicU64,
    /// On-disk bytes and logical size of reparse points inside (GM-B1): the folder is moved WHOLE and the shell counts
    /// against capacity even items that are not traversed (for example, hydrated cloud files). These are not included in
    /// `bytes` or `logical`, which represent space freed by deletion: deletion skips reparse points.
    pub reparse_bytes: AtomicU64,
    pub reparse_logical: AtomicU64,
    /// Informational Outlook data, counted by extension independently of deletion.
    pub mail_local_files: AtomicU64,
    pub mail_local_bytes: AtomicU64,
    pub mail_cache_files: AtomicU64,
    pub mail_cache_bytes: AtomicU64,
}

impl Stats {
    fn add(a: &AtomicU64, n: u64) {
        a.fetch_add(n, Ordering::Relaxed);
    }
    fn get(a: &AtomicU64) -> u64 {
        a.load(Ordering::Relaxed)
    }
    /// Adds an entry to the fingerprint (see [`Stats::fp_sum`]). `class` distinguishes how it was handled (counted, protected,
    /// reparse, recent...): if an entry changes class, the fingerprint changes.
    fn add_fp(&self, class: u8, file_id: u128, a: u64, b: u64, links: u32) {
        let (h1, h2) = entry_fingerprint(class, file_id, a, b, links);
        self.fp_sum.fetch_add(h1, Ordering::Relaxed);
        self.fp_xor.fetch_xor(h2, Ordering::Relaxed);
    }
    /// Copy of the values.
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
            gone: Self::get(&self.gone),
            fp_sum: Self::get(&self.fp_sum),
            fp_xor: Self::get(&self.fp_xor),
            max_path: Self::get(&self.max_path),
            reparse_bytes: Self::get(&self.reparse_bytes),
            reparse_logical: Self::get(&self.reparse_logical),
            mail_local_files: Self::get(&self.mail_local_files),
            mail_local_bytes: Self::get(&self.mail_local_bytes),
            mail_cache_files: Self::get(&self.mail_cache_files),
            mail_cache_bytes: Self::get(&self.mail_cache_bytes),
        }
    }
}

fn has_mail_extension(name: &[u16], ascii: &[u8; 3]) -> bool {
    let Some(dot) = name.iter().rposition(|&c| c == u16::from(b'.')) else {
        return false;
    };
    let extension = &name[dot + 1..];
    extension.len() == 3
        && extension
            .iter()
            .zip(ascii)
            .all(|(&c, &a)| c == u16::from(a) || c == u16::from(a.to_ascii_uppercase()))
}

/// Count mail names before opening children, including locked files. No path is followed.
fn count_mail(stats: &Stats, name: &[u16], bytes: u64) {
    let local = has_mail_extension(name, b"pst");
    let cache = has_mail_extension(name, b"ost") || has_mail_extension(name, b"nst");
    if local {
        Stats::add(&stats.mail_local_files, 1);
        Stats::add(&stats.mail_local_bytes, bytes);
    }
    if cache {
        Stats::add(&stats.mail_cache_files, 1);
        Stats::add(&stats.mail_cache_bytes, bytes);
    }
}

/// 64-bit mix (the `splitmix64` one): good diffusion in three operations.
fn mix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The two 64-bit hashes of an entry for the fingerprint ([`Stats::fp_sum`], [`Stats::fp_xor`]): all fields are chained
/// together and two values are derived with different constants (sums and XORs of many hashes degrade differently, and
/// together they form a 128-bit fingerprint).
pub(crate) fn entry_fingerprint(
    class: u8,
    file_id: u128,
    a: u64,
    b: u64,
    links: u32,
) -> (u64, u64) {
    let mut h = mix64(u64::from(class) ^ (u64::from(links) << 8));
    for part in [file_id as u64, (file_id >> 64) as u64, a, b] {
        h = mix64(h ^ part);
    }
    (h, mix64(h ^ 0xA076_1D64_78BD_642F))
}

// How an entry was handled (its fingerprint class): if it changes, the fingerprint changes.
const FP_COUNTED: u8 = 1;
const FP_PROTECTED: u8 = 2;
const FP_REPARSE: u8 = 3;
const FP_YOUNG: u8 = 4;

fn fp_class(base: u8, is_dir: bool) -> u8 {
    base | if is_dir { 0x80 } else { 0 }
}

/// Immutable copy of [`Stats`].
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
    pub gone: u64,
    /// Contents fingerprint (see [`Stats::fp_sum`]).
    pub fp_sum: u64,
    pub fp_xor: u64,
    /// Longest path inside (UTF-16 units; see [`Stats::max_path`]).
    pub max_path: u64,
    /// On-disk bytes and logical size of reparse points inside (see [`Stats::reparse_bytes`]).
    pub reparse_bytes: u64,
    pub reparse_logical: u64,
    pub mail_local_files: u64,
    pub mail_local_bytes: u64,
    pub mail_cache_files: u64,
    pub mail_cache_bytes: u64,
}

impl StatsSnapshot {
    /// Amount charged against Recycle Bin capacity (GM-M3, GM-B1): `bytes` (single-link files), `shared_bytes` (files with
    /// multiple links), and `reparse_bytes` (reparse points inside) are DISJOINT sets that the shell counts together, so
    /// they are ADDED (taking the maximum of the first two underestimated folders containing both types, and reparse points
    /// were not counted at all). Logical size (including reparse points) is compared separately because a sparse or
    /// compressed file can have a larger logical size than its on-disk size. The larger value determines the charge.
    ///
    /// Reparse points inside: their OWN size as reported by the listing is counted (a hydrated cloud file uses its size;
    /// a junction, symlink, or mount point uses practically nothing). Their targets are never measured: moving the folder
    /// moves the link, not the target, and they are never traversed.
    pub fn recycle_charge(&self) -> u64 {
        self.bytes
            .saturating_add(self.shared_bytes)
            .saturating_add(self.reparse_bytes)
            .max(self.logical.saturating_add(self.reparse_logical))
    }

    /// Is a measurement complete (mode `Measure`)? Only if traversal saw EVERYTHING present: no errors opening or listing
    /// (in use, access denied, or other errors: a subfolder that could not be listed leaves its contents uncounted), no entry
    /// whose identity or type changed during traversal, and no entry that disappeared between listing and opening. An
    /// incomplete measurement cannot be used to estimate Recycle Bin fit or to conclude that nothing changed.
    pub fn is_complete(&self) -> bool {
        self.in_use == 0
            && self.denied == 0
            && self.changed == 0
            && self.other_errors == 0
            && self.not_empty == 0
            && self.gone == 0
    }

    /// Adds another snapshot.
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
        self.gone += o.gone;
        self.fp_sum = self.fp_sum.wrapping_add(o.fp_sum);
        self.fp_xor ^= o.fp_xor;
        self.max_path = self.max_path.max(o.max_path);
        self.reparse_bytes += o.reparse_bytes;
        self.reparse_logical += o.reparse_logical;
        self.mail_local_files += o.mail_local_files;
        self.mail_local_bytes += o.mail_local_bytes;
        self.mail_cache_files += o.mail_cache_files;
        self.mail_cache_bytes += o.mail_cache_bytes;
    }
}

/// What happened to an entry (for the local log).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Deleted,
    /// Dry run: would have been deleted.
    WouldDelete,
    /// Sent to the Recycle Bin (verified).
    Recycled,
    /// Dry run: would have been sent to the Recycle Bin.
    WouldRecycle,
    /// Still in place after the attempt: it was not sent.
    NotRecycled,
    /// Disappeared without reaching the Recycle Bin: permanently deleted.
    DeletedUnexpectedly,
    /// An object other than the approved one was moved to the Recycle Bin (recoverable).
    RecycledOther,
    /// What happened could not be verified.
    Unverified,
    InUse,
    Denied,
    Changed,
    Protected,
    /// A `.pst` was kept because permanent deletion was not explicitly acknowledged.
    MailConsentRequired,
    Young,
    Reparse,
    Failed(Win32),
    /// A directory that was not empty.
    NotEmpty,
}

/// A processed entry for the log (only constructed when there is a listener).
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

/// Event receiver (the local log or a test).
pub type EventSink = dyn Fn(Event) + Send + Sync;

/// Parameters shared by a traversal.
pub struct Env {
    pub protection: Arc<Protection>,
    pub mode: Mode,
    pub allow_local_mail_permanent: bool,
    /// Minimum age in 100 ns intervals (0 = no limit).
    pub min_age_ft: i64,
    /// "Now" (FILETIME).
    pub now_ft: i64,
    pub cancel: Arc<AtomicBool>,
    pub sink: Option<Arc<EventSink>>,
    /// Processed entries (progress for the interface).
    pub progress: Arc<AtomicU64>,
}

/// FILETIME for "now".
pub fn now_filetime() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    (secs + 11_644_473_600) * 10_000_000
}

/// An age in FILETIME intervals.
pub fn duration_to_ft(d: Duration) -> i64 {
    (d.as_secs() as i64).saturating_mul(10_000_000)
}

/// What to traverse.
#[derive(Clone, Debug)]
pub enum UnitSpec {
    /// Deletes the CHILDREN of this directory (which is not deleted): roots of a rule-based category.
    Contents {
        path: PathBuf,
        approved: FileIdentity,
    },
    /// Deletes this entire entry (file or directory) with the approved identity.
    Entry {
        path: PathBuf,
        approved: FileIdentity,
        /// Approved reparse tag (0 except for cloud files, which are accepted only as standalone entries).
        approved_tag: u32,
        /// EXACT `NumberOfLinks` at approval: if it differs during execution (someone added or removed names), the entry is
        /// rejected as "changed since approval".
        approved_links: u32,
        is_dir: bool,
    },
}

/// How processing of a unit's ROOT ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootStatus {
    Done,
    /// The root no longer exists.
    Missing,
    /// The root's identity is not the approved one.
    Changed,
    /// The root's resolved path is not the approved one, or it is now protected.
    Protected,
    /// An individual `.pst` was kept because permanent deletion was not acknowledged.
    MailConsentRequired,
    /// The root's resolved path is not on a local drive-letter volume (UNC, including a local disk, or a network path).
    NotLocal,
    /// The root is a file in use.
    InUse,
    Denied,
    Failed(Win32),
    Cancelled,
}

/// Items actually deleted beneath a unit's top-level entry: the interface uses this to update the tree without rescanning
/// (overlay, PLAN.md §3.6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopResult {
    /// Entry name (a child of a `Contents` root, or the `Entry` item itself).
    pub name: OsString,
    pub is_dir: bool,
    /// The entire entry disappeared (otherwise, only part of its contents did).
    pub removed: bool,
    /// On-disk bytes deleted inside it (excluding shared hard links).
    pub bytes: u64,
    /// Logical size deleted.
    pub logical: u64,
    /// Files deleted.
    pub files: u64,
    /// Directories deleted (including the entry itself if `removed`).
    pub dirs: u64,
}

/// Accumulator for items deleted beneath a directory.
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

/// A work unit with its counters and result.
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
    /// Items deleted beneath each top-level entry (only for a `Contents` unit executed in `Delete` mode).
    pub fn tops(&self) -> Vec<TopResult> {
        self.tops.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    /// Items deleted from the entry itself (an `Entry` unit executed in `Delete` mode).
    pub fn root_result(&self) -> Option<TopResult> {
        self.root_result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// How processing of the root ended (`None` if it was never processed).
    pub fn status(&self) -> Option<RootStatus> {
        *self.status.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn set_status(&self, s: RootStatus) {
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
    }
}

/// A directory opened during traversal.
struct DirNode {
    unit: Arc<Unit>,
    handle: Handle,
    path: PathBuf,
    /// Length of `path` in UTF-16 units (for the longest path in the measurement; see [`Stats::max_path`]).
    path_len: usize,
    key: PathKey,
    parent: Option<Arc<DirNode>>,
    /// Pending tasks for this directory, plus one guard while its children are being distributed.
    pending: AtomicUsize,
    /// Can this directory be deleted when finished? (Not a `Contents` root or a protected directory.)
    deletable: bool,
    /// Something inside was preserved or failed: the directory will not be empty.
    kept: AtomicBool,
    /// The directory's own latest timestamp already meets `min_age` when opened.
    old_enough: bool,
    volume_serial: u64,
    /// This is the root of a `Contents` unit (its children are the top-level entries).
    contents_root: bool,
    /// Items deleted beneath this directory so far.
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

/// Entry name used to build comparison keys, without loss (see `paths::wide_to_string`).
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

/// Does this Windows code mean "no longer exists"?
fn is_gone(code: Win32) -> bool {
    matches!(code, 2 | 3)
}

/// Increments the appropriate system-error counter and returns the outcome for the log.
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

/// Runs the units in a pool of `threads` threads and returns when all finish (or cancellation occurs).
pub(crate) fn run(units: &[Arc<Unit>], threads: usize) {
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
                // A panic in one task must not leave the other threads waiting forever: cancel all work.
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

/// A directory task finished: if it was the last one, finalize the directory and notify its parent.
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

/// Adds the items deleted beneath `node` to its parent and, if it is a top-level entry of a `Contents` unit (or the item
/// of an `Entry` unit), records it for the interface overlay.
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

/// Deletes the (empty) directory through ITS own handle. Items that are not deleted keep their ancestors in place.
fn finalize(node: &DirNode) {
    let unit = &node.unit;
    let real = node.env().mode == Mode::Delete;
    if !node.deletable
        || node.env().cancel.load(Ordering::Relaxed)
        || node.kept.load(Ordering::Acquire)
    {
        // It remains (protected, cancelled, or containing something preserved or failed): its parent cannot be empty either.
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

/// Distributes `node`'s children as tasks and releases the guard.
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
    // Update counters before adding tasks: `outstanding` must not reach 0 while work remains to be distributed.
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
            let Some(node) = verify_root(&unit, h, &path, &approved, None, true, false) else {
                return;
            };
            unit.set_status(RootStatus::Done);
            list_and_spawn(shared, local, node);
        }
        UnitSpec::Entry {
            path,
            approved,
            approved_tag,
            approved_links,
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
                let Some(node) = verify_root(
                    &unit,
                    h,
                    &path,
                    &approved,
                    Some(approved_links),
                    false,
                    true,
                ) else {
                    return;
                };
                unit.set_status(RootStatus::Done);
                list_and_spawn(shared, local, node);
            } else {
                entry_file(&unit, &path, &approved, approved_tag, approved_links);
            }
        }
    }
}

/// Verifies a unit's root (approved identity, type, resolved path, and protection) and creates its node.
/// `contents`: this is a category root (not deleted). `deletable`: delete the root directory at the end.
fn verify_root(
    unit: &Arc<Unit>,
    h: Handle,
    approved_path: &Path,
    approved: &FileIdentity,
    approved_links: Option<u32>,
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
    // A directory with a reparse point (junction, symlink, mount point) is never accepted: it is not traversed.
    let links_changed = approved_links.is_some_and(|l| l != m.links);
    if !m.is_dir || m.identity != *approved || m.is_reparse() || links_changed {
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
    // The path resolved from the handle is the approved one and remains an allowed target (protection revalidation).
    let same_route = PathKey::new(approved_path) == key;
    // To delete a root's CONTENTS, an untouchable destination or known folder is also allowed ("its contents can be cleaned",
    // PLAN.md §2.7); only denied paths are excluded. A standalone entry requires `Allowed`.
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
        path_len: final_path.as_os_str().encode_wide().count(),
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

/// A standalone file approved as a basket item.
fn entry_file(
    unit: &Arc<Unit>,
    path: &Path,
    approved: &FileIdentity,
    approved_tag: u32,
    approved_links: u32,
) {
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
    // A cloud file (reparse point with a cloud tag) is accepted only if it is the one approved, with its warning; any other
    // reparse points are rejected.
    let reparse_ok =
        !m.is_reparse() || (m.reparse_tag == approved_tag && sys::is_cloud_tag(m.reparse_tag));
    // Same identity but a different link count (someone added or removed names): it changed since approval.
    if m.is_dir || m.identity != *approved || !reparse_ok || m.links != approved_links {
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
    let name: Vec<u16> = path.file_name().unwrap_or_default().encode_wide().collect();
    if env.mode == Mode::Measure {
        count_mail(&unit.stats, &name, m.alloc);
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
    // The approval snapshot can become stale. An execution without explicit local-mail consent
    // must never permanently delete a .pst, including one discovered after approval.
    if env.mode != Mode::Measure
        && !env.allow_local_mail_permanent
        && has_mail_extension(&name, b"pst")
    {
        unit.set_status(RootStatus::MailConsentRequired);
        Stats::add(&unit.stats.protected, 1);
        Stats::add(&unit.stats.protected_bytes, m.alloc);
        emit(
            unit,
            path.to_path_buf(),
            false,
            Some(m.identity),
            m.alloc,
            m.links,
            Outcome::MailConsentRequired,
        );
        return;
    }
    unit.stats.add_fp(
        fp_class(FP_COUNTED, false),
        m.identity.file_id,
        m.alloc,
        m.logical,
        m.links,
    );
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

/// Returns `true` if the task finished (the caller decrements the parent's pending-task count) and `false` if it was a
/// directory: its node decrements the parent's count when it is finalized, after all descendants finish.
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
    if env.mode == Mode::Measure && !entry.is_dir() {
        count_mail(&unit.stats, name, entry.alloc);
    }
    let ckey = parent.key.join(&lossy(name));
    let child_path = join_name(&parent.path, name);
    #[cfg(test)]
    test_hooks::before_open(&child_path);
    // GM-M3: the longest path inside (any entry, including protected entries and reparse points: they move with the folder).
    // Recorded in every mode so measurement, simulation, and deletion count the same items; used by measurement.
    unit.stats
        .max_path
        .fetch_max((parent.path_len + 1 + name.len()) as u64, Ordering::Relaxed);
    let action = env.protection.child_action(&ckey);
    // Protected: denied (or a file that would be "descend only," which makes no sense); preserve it.
    if action == ChildAction::Skip || (action == ChildAction::Descend && !entry.is_dir()) {
        Stats::add(&unit.stats.protected, 1);
        Stats::add(&unit.stats.protected_bytes, entry.alloc);
        unit.stats.add_fp(
            fp_class(FP_PROTECTED, entry.is_dir()),
            entry.file_id,
            entry.alloc,
            entry.logical,
            0,
        );
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
        // GM-B1: this space is NOT freed: the whole folder is moved and the shell counts it (a hydrated cloud file uses its
        // full size). It is not included in `bytes`/`logical` (space freed by deletion; this entry is skipped), but has its own counter.
        Stats::add(&unit.stats.reparse_bytes, entry.alloc);
        Stats::add(&unit.stats.reparse_logical, entry.logical);
        unit.stats.add_fp(
            fp_class(FP_REPARSE, entry.is_dir()),
            entry.file_id,
            entry.alloc,
            entry.logical,
            0,
        );
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
            Err(c) if is_gone(c) => {
                Stats::add(&unit.stats.gone, 1);
                return true;
            }
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
        unit.stats
            .add_fp(fp_class(FP_COUNTED, true), entry.file_id, 0, 0, m.links);
        let old_enough = env.min_age_ft == 0 || env.now_ft - m.newest_filetime >= env.min_age_ft;
        let node = Arc::new(DirNode {
            unit: unit.clone(),
            handle: h,
            path_len: parent.path_len + 1 + name.len(),
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
    // A file.
    let young = |newest: i64| env.min_age_ft > 0 && env.now_ft - newest < env.min_age_ft;
    if young(entry.newest_filetime) {
        Stats::add(&unit.stats.young, 1);
        unit.stats.add_fp(
            fp_class(FP_YOUNG, false),
            entry.file_id,
            entry.alloc,
            entry.logical,
            0,
        );
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
        Err(c) if is_gone(c) => {
            Stats::add(&unit.stats.gone, 1);
            return true;
        }
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
    // The timestamp may have changed between listing and opening (someone wrote to the file): check it again through the handle.
    if young(m.newest_filetime) {
        Stats::add(&unit.stats.young, 1);
        unit.stats.add_fp(
            fp_class(FP_YOUNG, false),
            m.identity.file_id,
            m.alloc,
            m.logical,
            m.links,
        );
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
    // The directory may now contain a .pst that was absent from the approval estimate. Keep it
    // unless the user explicitly acknowledged permanent deletion of local mail.
    if env.mode != Mode::Measure
        && !env.allow_local_mail_permanent
        && has_mail_extension(name, b"pst")
    {
        Stats::add(&unit.stats.protected, 1);
        Stats::add(&unit.stats.protected_bytes, m.alloc);
        parent.kept.store(true, Ordering::Release);
        emit(
            unit,
            child_path,
            false,
            Some(m.identity),
            m.alloc,
            m.links,
            Outcome::MailConsentRequired,
        );
        return true;
    }
    let account = |stats: &Stats| {
        stats.add_fp(
            fp_class(FP_COUNTED, false),
            m.identity.file_id,
            m.alloc,
            m.logical,
            m.links,
        );
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

    /// Instala un gancho que solo se ejecuta para rutas bajo `root`, justo antes de que el receptor de la shell las compruebe.
    pub fn install_before_pre_delete(
        root: PathBuf,
        f: impl Fn(&Path) + Send + Sync + 'static,
    ) -> HookGuard {
        install_at(2, root, f)
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

    /// Etapa 2: justo ANTES de que el receptor de la shell compruebe un elemento (`PreDeleteItem`), ya con la operación en
    /// marcha (A3-M1: la ventana entre la última comprobación del ejecutor y la del receptor).
    pub fn before_pre_delete(p: &Path) {
        run(2, p);
    }

    pub fn before_shell(p: &Path) {
        run(1, p);
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;

#[cfg(test)]
mod mail_tests {
    use super::*;
    #[test]
    fn mail_extensions_are_case_insensitive_and_not_content_fingerprints() {
        let stats = Stats::default();
        for (name, bytes) in [
            ("a.PST", 12),
            ("b.ost", 20),
            ("c.NsT", 30),
            ("x.pst.bak", 99),
            ("pst", 99),
        ] {
            count_mail(&stats, &name.encode_utf16().collect::<Vec<_>>(), bytes);
        }
        let snap = stats.snapshot();
        assert_eq!((snap.mail_local_files, snap.mail_local_bytes), (1, 12));
        assert_eq!((snap.mail_cache_files, snap.mail_cache_bytes), (2, 50));
        assert_eq!(
            (snap.fp_sum, snap.fp_xor, snap.bytes, snap.recycle_charge()),
            (0, 0, 0, 0)
        );
        let mut merged = StatsSnapshot::default();
        merged.merge(&snap);
        assert_eq!(merged, snap);
    }
}
