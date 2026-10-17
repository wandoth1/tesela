//! Plan execution (PLAN.md §2.5 and §3.7.2–§3.7.9).
//!
//! Order: first empty the Recycle Bin (using the list captured at confirmation), then permanently delete items and move
//! items to the Recycle Bin, so items the user wants to keep recoverable are not deleted in the same operation.
//!
//! * **Permanent**: `engine` handle traversal (exclusive open, identity check, `FileDispositionInfoEx`).
//! * **Recycle Bin** (without tombstone renaming): the item is opened exclusively, its identity and resolved path (local and allowed)
//!   are verified, the handle is CLOSED, and the ORIGINAL PATH is given to `IFileOperation` with a dedicated event receiver.
//!   For each item, the shell reports the outcome and the actual path of the item in the Recycle Bin; that path is opened
//!   and its IDENTITY compared with the approved one: same = sent; different = "a different object from the approved one
//!   was moved to the Recycle Bin" (recoverable: it is in the Recycle Bin); no item in the Recycle Bin = permanently deleted
//!   (it did not fit, or a replacement did not fit); error = skipped with its reason, only if the approved object remains
//!   intact in place (A-M1). A directory with anything open inside fails safely (moving is a rename) and is reported as
//!   "in use". The receiver monitors the operation (returns `E_ABORT`): `PreDeleteItem` compares the identity at the path
//!   with the approved identity and vetoes a replacement (M1); `PostDeleteItem` stops after the first permanent deletion
//!   or the first path that cannot be verified (M2). The rest of the batch is not processed and stays in place.
//!   A3-M1: a DIRECTORY has a change monitor (`ReadDirectoryChangesW` on a second verified handle) opened BEFORE its
//!   measurement traversal and kept alive until its `PreDeleteItem`: if anything inside changes, or cannot be monitored,
//!   the measurement is invalid and the item is not sent ("changed since approval"). A3-M2: Recycle Bin occupancy is read
//!   before and after traversal, and the larger value is used; this is a heuristic, not a guarantee (the shell decides
//!   independently whether an item fits).
//! * **Three counters**: "permanently deleted" and "moved to the Recycle Bin" are sums of sizes; "freed now" is the
//!   MEASURED difference in each volume's free space before and after.
//! * **Simulation** (`dry_run`): performs everything except destructive actions and records the identities it would affect.

use crate::engine::{self, Event, Mode, Outcome, RootStatus, StatsSnapshot, Unit, UnitSpec};
use crate::log::LogWriter;
use crate::paths::PathKey;
use crate::plan::{
    Cleaner, Fit, Plan, PlanItem, RejectReason, bin_big, bin_cap, fit_for, fit_for_deep,
};
use crate::protect::Verdict;
use crate::recycle::{self, BinItem, RecycleMode};
use crate::rules::{CategoryId, Destination};
use crate::sys::{self, FileIdentity};
use crate::volume;
use std::collections::VecDeque;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use windows::core::HRESULT;

/// How many plan rejections are recorded in the log (the rest are summarized on one line).
const MAX_REJECTED_NOTES: usize = 200;

/// Execution options.
#[derive(Clone, Default)]
pub struct ExecOptions {
    /// Dry run: nothing is touched.
    pub dry_run: bool,
    /// The user checked the amber-group box: items that do not fit in the Recycle Bin are permanently deleted.
    pub allow_unfit_permanent: bool,
    /// Explicit acknowledgment that local Outlook `.pst` files may be permanently deleted.
    /// The engine also checks each file discovered after approval.
    pub allow_local_mail_permanent: bool,
    /// Local execution log. REQUIRED except in a dry run (M4): no destructive action is allowed without a usable log
    /// ([`ExecError::LogRequired`]).
    pub log: Option<Arc<LogWriter>>,
}

/// Current phase (for the interface).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    Preparing = 0,
    EmptyingBin = 1,
    Deleting = 2,
    Recycling = 3,
    Verifying = 4,
    Done = 5,
}

/// Progress shared with the interface and cancellation state.
pub struct Progress {
    /// Setting this to `true` cancels between entries: work already started finishes, and nothing else is touched.
    cancel: Arc<AtomicBool>,
    /// The USER requested cancellation (`cancel()`). `cancel` is also set if the log fails (M4): this field distinguishes
    /// the two cases so the report can state both (H6.5).
    user_cancelled: AtomicBool,
    /// Entries processed by the traversal.
    pub entries: Arc<AtomicU64>,
    phase: AtomicU8,
    /// Completed and total plan items.
    pub items_done: AtomicUsize,
    pub items_total: AtomicUsize,
}

impl Progress {
    pub fn new() -> Arc<Progress> {
        Arc::new(Progress {
            cancel: Arc::new(AtomicBool::new(false)),
            user_cancelled: AtomicBool::new(false),
            entries: Arc::new(AtomicU64::new(0)),
            phase: AtomicU8::new(Phase::Preparing as u8),
            items_done: AtomicUsize::new(0),
            items_total: AtomicUsize::new(0),
        })
    }
    pub fn phase(&self) -> Phase {
        match self.phase.load(Ordering::Relaxed) {
            0 => Phase::Preparing,
            1 => Phase::EmptyingBin,
            2 => Phase::Deleting,
            3 => Phase::Recycling,
            4 => Phase::Verifying,
            _ => Phase::Done,
        }
    }
    /// Sets the current phase (used by the executor and the demo's simulated backend).
    pub fn set_phase(&self, p: Phase) {
        self.phase.store(p as u8, Ordering::Relaxed);
    }
    /// Sets the number of items in the execution.
    pub fn set_items_total(&self, n: usize) {
        self.items_total.store(n, Ordering::Relaxed);
    }
    /// Marks one more item as complete.
    pub fn add_item_done(&self) {
        self.items_done.fetch_add(1, Ordering::Relaxed);
    }
    /// Was cancellation requested (by the user or by a log failure, which also stops cleanup)?
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
    /// Cancels between entries: work already started finishes, and nothing else is touched. This is the ONLY way to cancel
    /// from outside, and it always records that the user requested it (`user_cancelled`), which distinguishes the report (R3).
    pub fn cancel(&self) {
        self.user_cancelled.store(true, Ordering::SeqCst);
        self.cancel.store(true, Ordering::SeqCst);
    }
    /// Did the user cancel (even if the log also failed)?
    pub fn user_cancelled(&self) -> bool {
        self.user_cancelled.load(Ordering::SeqCst)
    }
}

/// Number of listed items that changed since approval: those that did not match during enumeration (`not_matching`) plus
/// items associated with a veto (`Veto::id`: the owning `$R`, not the vetoed path, which may be a child).
pub(crate) fn bin_changed(
    not_matching: usize,
    vetoes: &[recycle::Veto],
    wanted: &HashSet<&OsString>,
) -> u64 {
    let mut vetoed: Vec<&OsString> = vetoes
        .iter()
        .map(recycle::Veto::id)
        .filter(|id| wanted.contains(id))
        .collect();
    vetoed.sort();
    vetoed.dedup();
    (not_matching + vetoed.len()) as u64
}

/// What failed when sending an item to the Recycle Bin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecycleFail {
    /// The item itself or something inside it is in use (failed safely: moving is a rename).
    InUse,
    /// Access denied.
    Denied,
    /// The shell did not process it; it remains in place.
    NotProcessed,
    /// The shell reported that it aborted the current operation, and the item remains in place.
    Aborted,
    /// There is no room in the Recycle Bin now (capacity minus current occupancy): sending it would make the shell delete it
    /// or evict items already there. It is not touched.
    NoRoom,
    /// Sending to the Recycle Bin stopped for safety (another item was permanently deleted or could not be verified): this
    /// item was not touched.
    Stopped,
    /// The shell rejected it with this `HRESULT` (it remains in place).
    Shell(i32),
}

/// How processing of an item ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemStatus {
    /// Permanent: the root was processed (the results are in the counters).
    Done,
    /// Verified in the Recycle Bin.
    Recycled,
    /// Dry run: would have been sent to the Recycle Bin.
    WouldRecycle,
    /// Not sent.
    NotRecycled(RecycleFail),
    /// Disappeared without reaching the Recycle Bin (the shell permanently deleted it because it did not fit) or, for a
    /// DIRECTORY, its contents no longer match just before sending (a complete measurement has a different fingerprint:
    /// something was lost or changed, for example because another process wrote inside). For the directory, this does not
    /// claim deletion; it reports the difference (H2).
    DeletedUnexpectedly,
    /// An object DIFFERENT from the approved one was moved to the Recycle Bin (someone replaced it between the check and
    /// the shell operation). It is in the Recycle Bin and can be restored.
    RecycledOther,
    /// What happened could not be verified (the shell did not provide the item's Recycle Bin path, or it could not be opened).
    Unverified,
    /// The root could not be processed (missing, changed, protected, in use, cancelled...).
    Root(RootStatus),
    /// Not touched: it does not fit in the Recycle Bin and permanent deletion was not selected, or it is NEVER permanently
    /// deleted ([`PlanItem::never_permanent`]: error reports and dumps).
    SkippedUnfit,
}

/// Result for a plan item.
#[derive(Clone, Debug)]
pub struct ItemReport {
    pub id: usize,
    pub category: Option<CategoryId>,
    pub path: PathBuf,
    /// Destination used for execution (an item that did not fit and was confirmed is moved to `Permanent`).
    pub destination: Destination,
    pub status: ItemStatus,
    pub stats: StatsSnapshot,
    /// What actually disappeared (complete paths): the interface uses this to update the tree without rescanning. Empty
    /// during a dry run and for items that were not touched.
    pub removed: Vec<Removed>,
}

/// An entry that disappeared from disk, or part of its contents (see [`ItemReport::removed`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removed {
    pub path: PathBuf,
    pub is_dir: bool,
    /// The entire entry disappeared (otherwise, only part of its contents was deleted).
    pub removed: bool,
    /// On-disk bytes deleted (excluding shared links), logical size, and file count.
    pub bytes: u64,
    pub logical: u64,
    pub files: u64,
}

/// Result of emptying the Recycle Bin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BinReport {
    /// Items in the captured list.
    pub requested: u64,
    /// Items from the list that are no longer in the Recycle Bin.
    pub purged: u64,
    /// Items from the list that remain.
    pub still_there: u64,
    /// Of these, items that were NOT touched because their `$R` or `$I` no longer matched enumeration (they changed since
    /// approval) or could not be verified.
    pub changed: u64,
    /// Items the shell reported as purged even though the approved object was NOT deleted: between the final check and
    /// deletion, its `$R` was replaced and the object at that name was deleted (A-M4). These do not count as purged. This
    /// could not be prevented because of the remaining race window between the receiver and the shell.
    pub replaced: u64,
    /// Items not touched because their file system does not provide identity (FAT, exFAT), so they cannot be verified.
    /// This does not mean they changed (H6.9).
    pub unverifiable: u64,
    /// Items NOT purged because their `$R` contains reparse points (junction, symlink, WSL link, cloud file) or could not
    /// be checked completely (M2): they are excluded BEFORE submission and do not stop the batch.
    pub with_links: u64,
    /// Recycle Bin items not listed at approval because their metadata (`$I`) was unreadable: they are not emptied, and
    /// their size is not included in the promised amount (m1).
    pub unlisted: u64,
    /// Approximate Recycle Bin size at approval.
    pub bytes_hint: u64,
    /// The shell could not perform the operation.
    pub failed: bool,
}

/// A volume's free space before and after execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeDelta {
    pub root: PathBuf,
    pub free_before: u64,
    pub free_after: u64,
}

/// Execution summary.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub dry_run: bool,
    pub cancelled: bool,
    pub items: Vec<ItemReport>,
    /// Permanently deleted (sum of sizes, excluding shared links). In a dry run, the amount that would have been deleted.
    pub permanent_bytes: u64,
    /// Moved to the Recycle Bin (verified). In a dry run, the amount that would have been moved.
    pub recycled_bytes: u64,
    pub bin: Option<BinReport>,
    /// Counters summed across all items (in use, access denied, changed, protected, shared links...).
    pub totals: StatsSnapshot,
    /// "Freed now": MEASURED difference in volume free space (signed; `None` during a dry run).
    pub freed_now: Option<i64>,
    pub volumes: Vec<VolumeDelta>,
    /// Items that disappeared without reaching the Recycle Bin.
    pub deleted_unexpectedly: u64,
    /// Items whose Recycle Bin destination is an object other than the approved one (they are in the Recycle Bin and can be restored).
    pub recycled_other: u64,
    /// Items whose outcome could not be verified.
    pub unverified: u64,
    /// First error (`HRESULT`) returned by the shell or COM while sending items to the Recycle Bin, if any. Items that
    /// could not be sent remain unprocessed and stay where they were.
    pub shell_error: Option<i32>,
    /// Sending to the Recycle Bin STOPPED for safety (see [`StopReason`]): remaining items were not touched.
    pub recycle_stopped: bool,
    /// Why sending stopped (the first reason), if `recycle_stopped`.
    pub stop_reason: Option<StopReason>,
    /// Items not sent because there was no room in the Recycle Bin.
    pub no_room: u64,
    /// Items not sent because of the safety stop.
    pub stopped_items: u64,
    /// Contents of the ENTIRE Recycle Bin after execution (according to the shell): what "Empty now" would remove. `None`
    /// if nothing was sent to the Recycle Bin, during a dry run, or if the query failed.
    pub bin_total_after: Option<u64>,
    /// Items that could not be sent.
    pub not_recycled: u64,
    /// Items that do not fit in the Recycle Bin and were not confirmed.
    pub skipped_unfit: u64,
    /// Path to the local log.
    pub log_path: Option<PathBuf>,
    /// The local log FAILED while writing or flushing (M4): cleanup stopped at the next safe point and nothing else was
    /// touched. Completed work remains in effect (and may not be fully recorded in the log).
    pub log_failed: bool,
    /// The log reached its limit (lines or bytes) and stopped recording: cleanup continued, but subsequent work is NOT
    /// recorded in the log (H5).
    pub log_truncated: bool,
}

/// Why sending to the Recycle Bin stopped (safety stop).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// The shell permanently deleted an item (it did not fit in the Recycle Bin), or a directory's contents no longer match
    /// their state just before sending (H2: they may have changed without anything being lost).
    DeletedUnexpectedly,
    /// An item's outcome could not be verified (the shell did not say where it ended up, or it could not be opened).
    Unverified,
    /// The shell reported that it "deleted" the item without sending it to the Recycle Bin (it did not fit), but did NOT
    /// touch it: it remains in place; or the operation failed, the item was verified intact, and the receiver cancelled
    /// the rest of its batch (A2-M1). Sending stops anyway (the shell behaved unexpectedly), but this does not claim that
    /// anything was deleted.
    ShellDeclined,
    /// The item at the path just before the shell acted was not the approved one (`PreDeleteItem` vetoed it): sending
    /// stopped, and nothing else was submitted.
    Changed,
}

/// Why execution did not proceed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecError {
    /// Recycle Bin fit changed since approval: confirmation is required again (PLAN.md §2.5). Contains the item IDs.
    NeedsReconfirm(Vec<usize>),
    /// No usable local log exists (it could not be created or had already failed), and this is not a dry run: no destructive
    /// action is allowed without a log (M4). Nothing was touched.
    LogRequired,
    /// The process is ELEVATED (running as administrator): actual cleanup and emptying the Recycle Bin are not allowed
    /// (PLAN.md §3.9: the application never cleans while elevated). Analysis and dry runs are allowed. Nothing was touched.
    Elevated,
}

/// Is the process elevated? A trait that allows both branches to be tested without elevating (CI cannot elevate).
pub(crate) trait Elevation: Send + Sync {
    fn is_elevated(&self) -> bool;
}

/// Queries the actual process token. FAILS CLOSED: if the query fails, the process is treated as elevated (no cleanup).
pub(crate) struct RealElevation;

impl Elevation for RealElevation {
    fn is_elevated(&self) -> bool {
        elevated_from(sys::process_elevation())
    }
}

/// SOLO PRUEBAS: "no elevado", el valor POR DEFECTO de todo `Cleaner` en las pruebas. Los ejecutores de integración continua
/// (`windows-latest`) corren ELEVADOS (UAC desactivado) y, con la consulta real, toda prueba que limpia daría `Elevated`: las
/// pruebas no dependen del proceso que las lanza. Solo las pruebas de E1 inyectan "elevado" (`Cleaner::with_elevation`). En
/// producción el valor por defecto es siempre [`RealElevation`].
#[cfg(test)]
pub(crate) struct NotElevated;

#[cfg(test)]
impl Elevation for NotElevated {
    fn is_elevated(&self) -> bool {
        false
    }
}

/// Fails closed: if the token cannot be queried, the process is treated as elevated.
fn elevated_from(state: Option<bool>) -> bool {
    state.unwrap_or(true)
}

impl Cleaner {
    /// Executes the plan. Blocks (the interface runs it on its own thread); progress and cancellation are reported through `progress`.
    pub fn execute(
        &self,
        plan: &Plan,
        opts: &ExecOptions,
        progress: &Arc<Progress>,
    ) -> Result<Report, ExecError> {
        progress.set_phase(Phase::Preparing);
        // E1: DO NOT CLEAN WHILE ELEVATED (or empty the Recycle Bin). Dry runs may continue. Check this BEFORE anything else
        // and here in `cleaner` (not only in the interface): the manifest is `asInvoker`, so an elevated process launching
        // Tesela passes its privileges to it.
        if !opts.dry_run && self.elevation.is_elevated() {
            if let Some(l) = &opts.log {
                let _ = l.discard();
            }
            return Err(ExecError::Elevated);
        }
        // M4: NO DESTRUCTIVE ACTION WITHOUT A USABLE LOG. A dry run may continue without one.
        if !opts.dry_run && opts.log.as_ref().is_none_or(|l| !l.ok()) {
            if let Some(l) = &opts.log {
                // Do not discard it if it already contains operations (`discard` prevents this).
                let _ = l.discard();
            }
            return Err(ExecError::LogRequired);
        }
        // A log write error triggers cancellation: cleanup stops at the next safe point.
        if let Some(l) = &opts.log {
            l.cancel_on_error(progress.cancel.clone());
            let ((local_n, local_b), (cache_n, cache_b)) = plan.mail_totals();
            if local_n + cache_n > 0 {
                l.write_note("mail", &format!("pst_files={local_n} pst_bytes={local_b} ost_nst_files={cache_n} ost_nst_bytes={cache_b}"));
            }
            // Items that did not make it into the plan are logged with their reason (for example, a root rejected because of a link).
            // Apply a limit so a plan with thousands of rejections cannot inflate the log (R4).
            // Put "too recent" items last: there may be many, and they must not hide a safety rejection.
            let ordered = plan
                .rejected
                .iter()
                .filter(|r| r.reason != RejectReason::Young)
                .chain(
                    plan.rejected
                        .iter()
                        .filter(|r| r.reason == RejectReason::Young),
                );
            for r in ordered.take(MAX_REJECTED_NOTES) {
                l.write_rejected(&r.path, &r.reason.log_text());
            }
            if plan.rejected.len() > MAX_REJECTED_NOTES {
                l.write_rejected(
                    Path::new("..."),
                    &format!(
                        "{} más sin anotar",
                        plan.rejected.len() - MAX_REJECTED_NOTES
                    ),
                );
            }
        }
        // 0. Recalculate Recycle Bin fit AT EXECUTION; if it changed, request confirmation again BEFORE touching anything.
        let changed: Vec<usize> = plan
            .items
            .iter()
            .filter(|i| i.destination == Destination::Recycle)
            .filter(|i| {
                // GM-M3: add bytes, shared bytes (disjoint sets), and logical size; the larger value determines fit. Include
                // the longest path inside, as during approval.
                let size = i.estimate.recycle_charge();
                self.fit_deep(&i.path, size, i.estimate.max_path as usize) != i.fit
            })
            .map(|i| i.id)
            .collect();
        if !changed.is_empty() {
            // Nothing was executed: the log (only its startup line) must not take up a retention slot.
            if let Some(l) = &opts.log {
                let _ = l.discard();
            }
            return Err(ExecError::NeedsReconfirm(changed));
        }

        let mut report = Report {
            dry_run: opts.dry_run,
            log_path: opts.log.as_ref().map(|l| l.path().to_path_buf()),
            ..Report::default()
        };
        let cancel = &progress.cancel;
        let mode = if opts.dry_run {
            Mode::DryRun
        } else {
            Mode::Delete
        };

        // Determine what to do with each item.
        let mut perm: Vec<&PlanItem> = Vec::new();
        let mut recyc: Vec<&PlanItem> = Vec::new();
        for it in &plan.items {
            // Fail closed: items prohibited by `never_permanent` (error reports and dumps, or a standalone item with a minimum
            // age whose permanent traversal is not all-or-nothing) never reach the permanent-deletion branch, regardless of
            // destination or checkbox.
            match (it.destination, it.is_unfit()) {
                (Destination::Permanent, _)
                    if !it.never_permanent() && !self.roots.is_diagnostic_path(&it.path) =>
                {
                    perm.push(it)
                }
                (Destination::Recycle, false) => recyc.push(it),
                (Destination::Recycle, true)
                    if opts.allow_unfit_permanent
                        && !it.never_permanent()
                        && !self.roots.is_diagnostic_path(&it.path) =>
                {
                    perm.push(it)
                }
                (Destination::Permanent | Destination::Recycle, _) => {
                    report.skipped_unfit += 1;
                    report.items.push(ItemReport {
                        id: it.id,
                        category: it.category,
                        path: it.path.clone(),
                        destination: it.destination,
                        status: ItemStatus::SkippedUnfit,
                        stats: StatsSnapshot::default(),
                        removed: Vec::new(),
                    });
                }
            }
        }
        let bin_items = plan
            .bin
            .as_ref()
            .map_or(0, |b| usize::from(!b.items.is_empty()));
        progress
            .items_total
            .store(perm.len() + recyc.len() + bin_items, Ordering::Relaxed);

        // Volumes: free space before execution.
        let mut roots: Vec<PathBuf> = Vec::new();
        let mut seen: HashSet<PathKey> = HashSet::new();
        let mut add_vol = |r: Option<PathBuf>| {
            if let Some(r) = r
                && seen.insert(PathKey::new(&r))
            {
                roots.push(r);
            }
        };
        for it in perm.iter().chain(&recyc) {
            add_vol(it.volume.clone());
        }
        if let Some(b) = &plan.bin {
            for bi in &b.items {
                add_vol(volume::volume_root(&bi.original_parent));
            }
        }
        let free = |r: &Path| tesela_scanner::volume_space(r).map(|v| v.free).ok();
        let before: Vec<Option<u64>> = roots.iter().map(|r| free(r)).collect();

        let sink = opts.log.as_ref().map(|l| l.sink());

        // 1. Empty the Recycle Bin using the list enumerated at approval.
        if let Some(bp) = plan.bin.as_ref().filter(|b| !b.items.is_empty()) {
            progress.set_phase(Phase::EmptyingBin);
            // m1: items that could not be listed are recorded in the log (and in the report as `BinReport::unlisted`).
            if plan.bin_unlisted > 0
                && let Some(l) = &opts.log
            {
                l.write_note(
                    "bin-unlisted",
                    &format!(
                        "{} elementos de la Papelera con los metadatos ilegibles no se listaron y no se vacían",
                        plan.bin_unlisted
                    ),
                );
            }
            report.bin = Some(self.empty_bin(
                &bp.items,
                bp.bytes_hint,
                opts.dry_run,
                cancel,
                sink.as_deref(),
                opts.log.as_deref(),
            ));
            if let Some(b) = report.bin.as_mut() {
                b.unlisted = plan.bin_unlisted;
            }
            progress.items_done.fetch_add(1, Ordering::Relaxed);
        }

        // 2. Permanent deletion: handle-based traversal. First write and flush the INTENT (the operation's items); if that
        // fails, nothing is deleted.
        let can_delete = !perm.is_empty()
            && !cancel.load(Ordering::Relaxed)
            && opts.log.as_ref().is_none_or(|l| {
                let roots: Vec<PathBuf> = perm.iter().map(|it| it.path.clone()).collect();
                l.write_intent("delete-begin", &roots).is_ok()
            });
        if can_delete {
            progress.set_phase(Phase::Deleting);
            let units: Vec<Arc<Unit>> = perm
                .iter()
                .map(|it| {
                    Unit::new(
                        it.id,
                        it.spec.clone(),
                        self.env(
                            mode,
                            it.min_age,
                            cancel,
                            &progress.entries,
                            sink.clone(),
                            opts.allow_local_mail_permanent,
                        ),
                    )
                })
                .collect();
            engine::run(&units, self.threads);
            for (it, u) in perm.iter().zip(&units) {
                let st = u.status().unwrap_or(RootStatus::Cancelled);
                // Record each root's result after its intent (individual entries are already logged).
                if let Some(l) = &opts.log {
                    l.write_note("delete-end", &format!("{} -> {:?}", it.path.display(), st));
                }
                report.items.push(ItemReport {
                    id: it.id,
                    category: it.category,
                    path: it.path.clone(),
                    destination: Destination::Permanent,
                    status: match st {
                        RootStatus::Done => ItemStatus::Done,
                        other => ItemStatus::Root(other),
                    },
                    stats: u.stats.snapshot(),
                    removed: if opts.dry_run {
                        Vec::new()
                    } else {
                        removed_of(it, u)
                    },
                });
                progress.items_done.fetch_add(1, Ordering::Relaxed);
            }
            if let Some(l) = &opts.log {
                l.flush();
            }
        } else {
            for it in &perm {
                report.items.push(ItemReport {
                    id: it.id,
                    category: it.category,
                    path: it.path.clone(),
                    destination: Destination::Permanent,
                    status: ItemStatus::Root(RootStatus::Cancelled),
                    stats: StatsSnapshot::default(),
                    removed: Vec::new(),
                });
            }
        }

        // 3. Items going to the Recycle Bin: verify, release the handle, call IFileOperation with a receiver, then judge by identity.
        if !recyc.is_empty() {
            progress.set_phase(Phase::Recycling);
            self.recycle_items(&recyc, opts, progress, sink.clone(), &mut report);
        }

        progress.set_phase(Phase::Verifying);
        // 4. Counters and measured free space.
        report.cancelled = cancel.load(Ordering::Relaxed);
        for ir in &report.items {
            report.totals.merge(&ir.stats);
            match (ir.destination, ir.status) {
                (Destination::Permanent, ItemStatus::Done) => {
                    report.permanent_bytes += ir.stats.bytes
                }
                (Destination::Recycle, ItemStatus::Recycled | ItemStatus::WouldRecycle) => {
                    report.recycled_bytes += ir.stats.bytes;
                }
                (_, ItemStatus::DeletedUnexpectedly) => report.deleted_unexpectedly += 1,
                (_, ItemStatus::RecycledOther) => report.recycled_other += 1,
                (_, ItemStatus::Unverified) => report.unverified += 1,
                (_, ItemStatus::NotRecycled(RecycleFail::NoRoom)) => {
                    report.no_room += 1;
                    report.not_recycled += 1;
                }
                (_, ItemStatus::NotRecycled(RecycleFail::Stopped)) => {
                    report.stopped_items += 1;
                    report.not_recycled += 1;
                }
                (_, ItemStatus::NotRecycled(_)) => report.not_recycled += 1,
                _ => {}
            }
        }
        if !opts.dry_run && (report.recycled_bytes > 0 || report.recycled_other > 0) {
            report.bin_total_after = recycle::bin_totals().ok().map(|t| t.1);
        }
        if !opts.dry_run {
            // NTFS may take a moment to reflect freed space.
            std::thread::sleep(Duration::from_millis(250));
            let mut delta: i64 = 0;
            let mut all = true;
            for (r, b) in roots.iter().zip(&before) {
                match (b, free(r)) {
                    (Some(b), Some(a)) => {
                        delta += a as i64 - *b as i64;
                        report.volumes.push(VolumeDelta {
                            root: r.clone(),
                            free_before: *b,
                            free_after: a,
                        });
                    }
                    _ => all = false,
                }
            }
            report.freed_now = (all && !roots.is_empty()).then_some(delta);
        }
        if let Some(l) = &opts.log {
            l.write_note(
                "summary",
                &format!(
                    "definitivo {} B; papelera {} B; liberado ahora {:?}; en uso {}; sin permiso {}",
                    report.permanent_bytes, report.recycled_bytes, report.freed_now, report.totals.in_use, report.totals.denied
                ),
            );
            // At the end, call `sync_all` to flush to disk (once; see the `log` module).
            let _ = l.sync();
        }
        report.log_failed = opts.log.as_ref().is_some_and(|l| !l.ok());
        report.log_truncated = opts.log.as_ref().is_some_and(|l| l.truncated());
        // `cancelled` means ONLY user cancellation; a log failure is reported separately (both can occur).
        report.cancelled = progress.user_cancelled();
        progress.set_phase(Phase::Done);
        Ok(report)
    }

    /// Empties the Recycle Bin using the captured list. Only THAT list is purged; items added later are untouched.
    fn empty_bin(
        &self,
        items: &[BinItem],
        bytes_hint: u64,
        dry: bool,
        cancel: &Arc<AtomicBool>,
        sink: Option<&engine::EventSink>,
        log: Option<&LogWriter>,
    ) -> BinReport {
        let mut rep = BinReport {
            requested: items.len() as u64,
            bytes_hint,
            ..BinReport::default()
        };
        if dry {
            if let Some(s) = sink {
                for it in items {
                    s(Event {
                        unit: usize::MAX,
                        path: it.original_path(),
                        is_dir: false,
                        identity: None,
                        bytes: 0,
                        links: 0,
                        outcome: Outcome::WouldDelete,
                    });
                }
            }
            rep.purged = rep.requested;
            return rep;
        }
        if cancel.load(Ordering::Relaxed) || self.elevation.is_elevated() {
            // Cancelled or elevated (E1: second line of defense; `execute` already rejects this): purge nothing.
            rep.still_there = rep.requested;
            rep.failed = !cancel.load(Ordering::Relaxed);
            return rep;
        }
        // Purge through the Recycle Bin shell folder (see `recycle::purge_items`): the shell deletes `$R` and `$I` together.
        // Results come from the shell's per-item notifications (the Recycle Bin is not enumerated again).
        // Log each item SEPARATELY: record each chunk's intent before submitting it to the shell, then each item's result
        // (purged, error, or no notification), including items skipped because they changed.
        let mut observe = |step: recycle::PurgeStep<'_>| {
            let Some(l) = log else { return };
            match step {
                recycle::PurgeStep::Begin(items) => {
                    let paths: Vec<PathBuf> = items.iter().map(|i| i.original_path()).collect();
                    // An error here marks the log as failed and triggers cancellation: the chunk is not submitted.
                    let _ = l.write_intent("bin-begin", &paths);
                }
                recycle::PurgeStep::Skipped(items) => {
                    for i in items {
                        l.write_bin_item(&i.original_path(), &i.id, i.size, "changed");
                    }
                    l.flush();
                }
                recycle::PurgeStep::Unverifiable(items) => {
                    for i in items {
                        l.write_bin_item(&i.original_path(), &i.id, i.size, "no-identity");
                    }
                    l.flush();
                }
                recycle::PurgeStep::Replaced(items) => {
                    // A-M4: the shell reported a successful purge, but the deleted object was not the approved one.
                    for i in items {
                        l.write_bin_item(&i.original_path(), &i.id, i.size, "replaced");
                    }
                    l.flush();
                }
                recycle::PurgeStep::Linked(items) => {
                    // M2: `$R` contains links (or could not be fully checked): it is not submitted to the shell.
                    for i in items {
                        l.write_bin_item(&i.original_path(), &i.id, i.size, "linked");
                    }
                    l.flush();
                }
                recycle::PurgeStep::Vetoed(items) => {
                    // A-M4: the shell was about to delete something that no longer matched the captured item, and the receiver vetoed it.
                    for i in items {
                        l.write_bin_item(&i.original_path(), &i.id, i.size, "vetoed");
                    }
                    l.flush();
                }
                recycle::PurgeStep::Done(done) => {
                    for (i, hr) in done {
                        let out = match hr {
                            Some(h) if *h >= 0 => "purged".to_string(),
                            Some(h) => format!("error-0x{:08X}", *h as u32),
                            None => "not-processed".to_string(),
                        };
                        l.write_bin_item(&i.original_path(), &i.id, i.size, &out);
                    }
                    l.flush();
                }
            }
        };
        match self.shell.purge(items, cancel, &mut observe) {
            Ok(out) => {
                let wanted: HashSet<&OsString> = items.iter().map(|i| &i.id).collect();
                let purged: HashSet<&OsString> = out
                    .events
                    .iter()
                    .filter(|e| e.succeeded() && wanted.contains(&e.item))
                    .filter(|e| !out.replaced_bin.contains(&e.item))
                    .map(|e| &e.item)
                    .collect();
                rep.replaced = out
                    .replaced_bin
                    .iter()
                    .filter(|i| wanted.contains(i))
                    .count() as u64;
                rep.purged = purged.len() as u64;
                rep.still_there = rep.requested - rep.purged;
                // Changed since approval: items that did not match during enumeration and those that changed LATER, just before
                // the shell deleted them (vetoed by `PreDeleteItem`, A-M4).
                rep.changed = bin_changed(out.changed_bin.len(), &out.vetoed, &wanted);
                rep.unverifiable = out.unverifiable_bin.len() as u64;
                rep.with_links =
                    out.linked_bin.iter().filter(|i| wanted.contains(i)).count() as u64;
                // The `E_ABORT` generated by the receiver's veto is not a shell failure.
                rep.failed = out.error.is_some() && !out.sink_aborted;
            }
            Err(_) => {
                rep.failed = true;
                rep.purged = 0;
                rep.still_there = rep.requested;
            }
        }
        if let Some(s) = sink {
            s(Event {
                unit: usize::MAX,
                path: PathBuf::from("<papelera>"),
                is_dir: false,
                identity: None,
                bytes: bytes_hint,
                links: 0,
                outcome: if rep.still_there == 0 {
                    Outcome::Deleted
                } else {
                    Outcome::NotEmpty
                },
            });
        }
        rep
    }

    /// Sends items to the Recycle Bin in chunks (see the module):
    /// 1. PREPARE each item (open exclusively to capture identity and detect "in use," resolve its path, and check protection),
    ///    then keep a GUARD handle open until after judging the item (see `prepare_recycle`);
    /// 2. group items by volume into shell OPERATIONS according to the remaining Recycle Bin space ([`take_op`]): the shell
    ///    decides whether items fit PER OPERATION, and one item that does not fit can affect all items in that operation.
    ///    Read occupancy before each operation, then remeasure each candidate JUST BEFORE submission (A-B1: a file through
    ///    its guard handle, a directory through a COMPLETE traversal, A-B2) and recheck the budget against those sizes and
    ///    the occupancy at that moment;
    /// 3. submit the ORIGINAL paths to the shell and judge each item using the shell's own notifications;
    /// 4. SAFETY STOP: if an item is permanently deleted (`DeletedUnexpectedly`), STOP sending the rest: subsequent items
    ///    become `NotRecycled(Stopped)`, as stated in the summary. Nothing is lost in a chain.
    ///
    /// Log and flush each item as soon as it finishes; record the paths before each operation.
    fn recycle_items(
        &self,
        items: &[&PlanItem],
        opts: &ExecOptions,
        progress: &Arc<Progress>,
        sink: Option<Arc<engine::EventSink>>,
        report: &mut Report,
    ) {
        let shell: &dyn RecycleShell = self.shell.as_ref();
        let mut results: HashMap<usize, (ItemStatus, StatsSnapshot)> = HashMap::new();
        let cancel = &progress.cancel;
        // Do not cancel checks performed AFTER the shell operation (for example, whether contents remain intact).
        let verify_cancel = Arc::new(AtomicBool::new(false));
        let emit = |it: &PlanItem, id: Option<FileIdentity>, links: u32, o: Outcome| {
            if let Some(s) = &sink {
                s(Event {
                    unit: it.id,
                    path: it.path.clone(),
                    is_dir: it.is_dir(),
                    identity: id,
                    bytes: it.estimate.bytes,
                    links,
                    outcome: o,
                });
            }
            // Flush the log after EACH item so it records how far execution got if the application closes partway through.
            if let Some(l) = &opts.log {
                l.flush();
            }
        };
        // A3-M1: did a watcher detect a change inside any operation directory since it was measured?
        let watch_changed = |entries: &[(&PlanItem, Pending)]| {
            entries
                .iter()
                .any(|(_, p)| p.watch.as_ref().is_some_and(|w| w.changed()))
        };
        // Do not submit the operation: record its items as "changed since approval".
        let reject_changed =
            |entries: &[(&PlanItem, Pending)],
             results: &mut HashMap<usize, (ItemStatus, StatsSnapshot)>| {
                for (it, _) in entries {
                    emit(it, None, 0, Outcome::Changed);
                    results.insert(
                        it.id,
                        (
                            ItemStatus::Root(RootStatus::Changed),
                            StatsSnapshot {
                                changed: 1,
                                ..StatsSnapshot::default()
                            },
                        ),
                    );
                }
            };
        let mut stopped = false;
        for chunk in items.chunks(RECYCLE_CHUNK) {
            if cancel.load(Ordering::Relaxed) {
                for it in chunk {
                    results.insert(
                        it.id,
                        (
                            ItemStatus::Root(RootStatus::Cancelled),
                            StatsSnapshot::default(),
                        ),
                    );
                }
                continue;
            }
            if stopped {
                for it in chunk {
                    emit(it, None, 0, Outcome::NotRecycled);
                    results.insert(
                        it.id,
                        (
                            ItemStatus::NotRecycled(RecycleFail::Stopped),
                            StatsSnapshot::default(),
                        ),
                    );
                }
                continue;
            }
            // 1. Prepare: verify the item and retain its guard handle.
            let mut slots: Vec<Option<(&PlanItem, Pending)>> = Vec::new();
            for &it in chunk {
                let mut st = StatsSnapshot::default();
                progress.entries.fetch_add(1, Ordering::Relaxed);
                match self.prepare_recycle(it, &mut st) {
                    Prepared::Ready(mut p) if opts.dry_run => {
                        // The dry run also measures what a real submission would measure.
                        if let Err((status, o)) = self.fresh_size(it, &mut p, &mut st, cancel) {
                            emit(it, None, 0, o);
                            results.insert(it.id, (status, st));
                            continue;
                        }
                        emit(it, Some(p.identity), p.links, Outcome::WouldRecycle);
                        st.bytes = p.disk;
                        st.logical = p.logical;
                        results.insert(it.id, (ItemStatus::WouldRecycle, st));
                    }
                    Prepared::Ready(p) => slots.push(Some((it, *p))),
                    Prepared::Rejected(status, o) => {
                        emit(it, None, 0, o);
                        results.insert(it.id, (status, st));
                    }
                }
            }
            // 2. Group by volume (in arrival order) and submit one operation at a time.
            let mut buckets: Vec<(Option<PathKey>, Option<PathBuf>, VecDeque<usize>)> = Vec::new();
            for (i, s) in slots.iter().enumerate() {
                let (it, _) = s.as_ref().expect("recién preparado");
                let key = it.volume.as_deref().map(PathKey::new);
                match buckets.iter_mut().find(|(k, ..)| *k == key) {
                    Some((.., q)) => q.push_back(i),
                    None => buckets.push((key, it.volume.clone(), VecDeque::from([i]))),
                }
            }
            for (_, vol, mut queue) in buckets {
                while let Some(&first) = queue.front() {
                    if cancel.load(Ordering::Relaxed) || stopped {
                        // Do not submit: prepared items not handed to the shell remain untouched.
                        for i in queue.drain(..) {
                            if let Some((it, _)) = slots[i].take() {
                                let fail = if stopped {
                                    emit(it, None, 0, Outcome::NotRecycled);
                                    ItemStatus::NotRecycled(RecycleFail::Stopped)
                                } else {
                                    ItemStatus::Root(RootStatus::Cancelled)
                                };
                                results.insert(it.id, (fail, StatsSnapshot::default()));
                            }
                        }
                        break;
                    }
                    let first_is_dir = slots[first].as_ref().is_some_and(|(it, _)| it.is_dir());
                    // A2-B1: a DIRECTORY ALWAYS runs alone in its operation. Traversal takes time; with multiple candidates
                    // in one operation, an item already measured could grow while the others were being measured and reach
                    // the shell with a stale size (`PreDeleteItem` checks identity, not size). Alone, the directory is
                    // measured and submitted with nothing in between except the log intent line (§3.7.13). A3-M2: read
                    // Recycle Bin occupancy BEFORE traversal and AGAIN afterward, and use the LARGER value (`BinRoom::worst`):
                    // with only the first reading, the bin could fill during measurement (Astra: capacity 100,000, empty at
                    // start, 99,000 at the end, then a 4,000-byte folder was submitted based on the stale reading). The
                    // second reading takes a few milliseconds and falls within the window where it is needed. Occupancy is
                    // a HEURISTIC, not a guarantee: a third party can change it before the shell acts, and the shell decides
                    // independently whether the item fits.
                    let entries: Vec<(&PlanItem, Pending)> = if first_is_dir {
                        queue.pop_front();
                        let Some((it, mut p)) = slots[first].take() else {
                            continue;
                        };
                        let room = vol
                            .as_deref()
                            .map_or(BinRoom::UNKNOWN, |r| shell.bin_room(r));
                        let mut st = StatsSnapshot::default();
                        if let Err((status, o)) = self.fresh_size(it, &mut p, &mut st, cancel) {
                            emit(it, None, 0, o);
                            results.insert(it.id, (status, st));
                            continue;
                        }
                        // Use the fresh measurement and the LARGER of the two occupancy readings: before and after traversal.
                        let room_after = vol
                            .as_deref()
                            .map_or(BinRoom::UNKNOWN, |r| shell.bin_room(r));
                        let room = room.worst(room_after);
                        if let Take::NoRoom = take_op(&[p.size], room) {
                            // There is no room now: submitting it would make the shell delete it or evict existing Recycle Bin
                            // items. Leave it untouched (the guard handle is released when this block exits).
                            emit(it, None, 0, Outcome::NotRecycled);
                            results.insert(
                                it.id,
                                (
                                    ItemStatus::NotRecycled(RecycleFail::NoRoom),
                                    StatsSnapshot::default(),
                                ),
                            );
                            continue;
                        }
                        vec![(it, p)]
                    } else {
                        // FILES can be submitted together (never with a directory). Measure the first queued file now, just
                        // before deciding its operation; if it grew, do not submit it.
                        if let Some((it, p)) = slots[first].as_mut() {
                            let mut st = StatsSnapshot::default();
                            if let Err((status, o)) = self.fresh_size(it, p, &mut st, cancel) {
                                emit(it, None, 0, o);
                                results.insert(it.id, (status, st));
                                slots[first] = None;
                                queue.pop_front();
                                continue;
                            }
                        }
                        // Stop the group at the first directory in the queue: the operation contains files only.
                        let sizes: Vec<u64> = queue
                            .iter()
                            .take_while(|&&i| slots[i].as_ref().is_some_and(|s| !s.0.is_dir()))
                            .map(|&i| slots[i].as_ref().map_or(0, |s| s.1.size))
                            .collect();
                        let room = vol
                            .as_deref()
                            .map_or(BinRoom::UNKNOWN, |r| shell.bin_room(r));
                        let n = match take_op(&sizes, room) {
                            Take::NoRoom => {
                                // There is no room now: submitting it would make the shell delete it or evict existing Recycle Bin
                                // items. Leave it untouched.
                                queue.pop_front();
                                if let Some((it, _)) = slots[first].take() {
                                    emit(it, None, 0, Outcome::NotRecycled);
                                    results.insert(
                                        it.id,
                                        (
                                            ItemStatus::NotRecycled(RecycleFail::NoRoom),
                                            StatsSnapshot::default(),
                                        ),
                                    );
                                }
                                continue;
                            }
                            Take::Send(n) => n,
                        };
                        // Remeasure the candidates NOW through their guard handles, and carry each file's measurement with it:
                        // `PreDeleteItem` reads it again just before the shell acts and vetoes any growth (A2-B1; without this,
                        // an item measured at the start of the batch could grow while the others were being measured).
                        let mut staged: Vec<(usize, &PlanItem, Pending)> = Vec::new();
                        let candidates: Vec<usize> = queue.drain(..n).collect();
                        for i in candidates {
                            let Some((it, mut p)) = slots[i].take() else {
                                continue;
                            };
                            let mut st = StatsSnapshot::default();
                            match self.fresh_size(it, &mut p, &mut st, cancel) {
                                Ok(()) => staged.push((i, it, p)),
                                Err((status, o)) => {
                                    emit(it, None, 0, o);
                                    results.insert(it.id, (status, st));
                                }
                            }
                        }
                        if staged.is_empty() {
                            continue;
                        }
                        // ...then recheck the budget using the CURRENT sizes and occupancy: items that no longer fit in the
                        // operation return to the front of the queue (they will be measured again when their turn comes).
                        let room_now = room.worst(
                            vol.as_deref()
                                .map_or(BinRoom::UNKNOWN, |r| shell.bin_room(r)),
                        );
                        let fresh_sizes: Vec<u64> = staged.iter().map(|s| s.2.size).collect();
                        let keep = match take_op(&fresh_sizes, room_now) {
                            Take::NoRoom => {
                                let (_, it, _guard) = staged.remove(0);
                                emit(it, None, 0, Outcome::NotRecycled);
                                results.insert(
                                    it.id,
                                    (
                                        ItemStatus::NotRecycled(RecycleFail::NoRoom),
                                        StatsSnapshot::default(),
                                    ),
                                );
                                0
                            }
                            Take::Send(k) => k.min(staged.len()),
                        };
                        for (i, it, p) in staged.drain(keep..).rev() {
                            slots[i] = Some((it, p));
                            queue.push_front(i);
                        }
                        staged.into_iter().map(|(_, it, p)| (it, p)).collect()
                    };
                    if entries.is_empty() {
                        continue;
                    }
                    let paths: Vec<PathBuf> = entries.iter().map(|(_, p)| p.path.clone()).collect();
                    // Carry the approved identity with the path: the receiver checks it in `PreDeleteItem`.
                    // A FILE also carries its guard handle and counted size: `PreDeleteItem` measures it again (A2-B1).
                    let send: Vec<recycle::SendItem> = entries
                        .iter()
                        .map(|(it, p)| recycle::SendItem {
                            path: p.path.clone(),
                            identity: p.identity,
                            // A3-M1: a DIRECTORY carries the change monitor opened before measurement.
                            watch: p.watch.clone(),
                            size_guard: (!it.is_dir()).then(|| recycle::SizeGuard {
                                handle: p.held.clone(),
                                budget: p.size,
                            }),
                        })
                        .collect();
                    #[cfg(test)]
                    for p in &paths {
                        crate::engine::test_hooks::before_shell(p);
                    }
                    // A3-M1 (audit S1): check AGAIN BEFORE writing the intent line. A folder that already changed must not leave
                    // a `recycle-begin` without a submission; check it again just after writing the line (the last step before
                    // the shell), because writing takes time and falls within the race window.
                    if watch_changed(&entries) {
                        reject_changed(&entries, &mut results);
                        drop(entries);
                        continue;
                    }
                    // Before submitting paths to the shell, record and flush the INTENT (the batch's path list): after a forced
                    // close, this shows what may have been moved. If it cannot be written, submit NOTHING to the shell.
                    if let Some(l) = &opts.log
                        && l.write_intent("recycle-begin", &paths).is_err()
                    {
                        for (it, _) in &entries {
                            emit(it, None, 0, Outcome::NotRecycled);
                            results.insert(
                                it.id,
                                (
                                    ItemStatus::Root(RootStatus::Cancelled),
                                    StatsSnapshot::default(),
                                ),
                            );
                        }
                        drop(entries);
                        continue;
                    }
                    // A3-M1: the final check before the shell. If the monitor saw a change inside the directory since measurement,
                    // the measurement is stale: do not submit it ("changed since approval"). A directory runs alone; in the
                    // shell operation, `PreDeleteItem` checks the same monitor again and stops it if the check passes.
                    if watch_changed(&entries) {
                        reject_changed(&entries, &mut results);
                        drop(entries);
                        continue;
                    }
                    let outcome = shell.recycle(&send);
                    // Do not lose shell or COM errors: record them in the local log and summary. The `E_ABORT` generated by the
                    // receiver to stop a batch is NOT a shell error; the stop reason is recorded in `stop_reason`.
                    if let Some(e) = &outcome.error {
                        let own_abort = outcome.sink_aborted
                            && e.hresult == windows::Win32::Foundation::E_ABORT.0;
                        if !own_abort {
                            report.shell_error.get_or_insert(e.hresult);
                        } else if let Some(f) = outcome.events.iter().find(|ev| {
                            !ev.succeeded() && ev.hr != windows::Win32::Foundation::E_ABORT.0
                        }) {
                            // A2-M1: the receiver stops after a FAILURE result, so this `E_ABORT` is its own; preserve the shell
                            // error from that result. Ignore the receiver's own `E_ABORT`: after a veto, the shell reports the
                            // vetoed item with `PostDeleteItem(E_ABORT)` (observed), which is not a Windows error.
                            report.shell_error.get_or_insert(f.hr);
                        }
                        if let Some(l) = &opts.log {
                            l.write_note(
                                "recycle-error",
                                &format!("hresult 0x{:08X}: {}", e.hresult as u32, e.detail),
                            );
                            l.flush();
                        }
                    }
                    // GM-M5: assign each event and veto to an operation item by its EXACT name; use the case-folded key only
                    // when unambiguous. In a case-sensitive directory, two names differing only by case were confused by a
                    // key-based table, causing the second to inherit the first one's result (or veto).
                    let names: Vec<&std::ffi::OsStr> =
                        entries.iter().map(|(_, p)| p.path.as_os_str()).collect();
                    let mut event_of: Vec<Option<&recycle::DeleteEvent>> = vec![None; names.len()];
                    for e in &outcome.events {
                        if let Some(i) = crate::paths::owner_index(&names, &e.item) {
                            event_of[i] = Some(e);
                        }
                    }
                    let mut veto_of: Vec<bool> = vec![false; names.len()];
                    for v in &outcome.vetoed {
                        if let Some(i) = crate::paths::owner_index(&names, v.id()) {
                            veto_of[i] = true;
                        }
                    }
                    // 3. Judge each item using the shell's notification (while its guard handle is still open).
                    for (idx, (it, p)) in entries.iter().enumerate() {
                        let ev = event_of[idx];
                        let (mut status, mut o) = if veto_of[idx] {
                            // `PreDeleteItem` found a different object at the approved path and vetoed it: nothing was submitted
                            // (the replacement remains in place), and the item is recorded as "changed since approval".
                            (ItemStatus::Root(RootStatus::Changed), Outcome::Changed)
                        } else {
                            judge(p, ev, shell, outcome.any_aborted, outcome.sink_aborted)
                        };
                        // R6 + A-M1: "not submitted and still in place" is true for a DIRECTORY only if its contents were also
                        // untouched: the folder object remaining intact says nothing about its children. Apply this to ANY
                        // result meaning "not submitted" after the shell operation (the shell reported "deleted" without
                        // touching it, failed, did not process it...): a failure can occur after a child was lost. Remeasure
                        // using the same COMPLETE relative traversal and compare the contents fingerprint with the one taken
                        // just before submission. There are THREE outcomes (A2-m1): equal means `Intact`; different in a
                        // complete measurement means `Changed` (permanent deletion, even if partial); an incomplete measurement
                        // (unread subfolder, failure, or cancellation) means `Unknown`: claim no loss, report `Unverified`, and
                        // stop sending. Do NOT interrupt this check on cancellation; it must finish so the report is accurate.
                        if matches!(status, ItemStatus::NotRecycled(_)) && it.is_dir() {
                            match self.dir_check(it, p, &verify_cancel) {
                                DirCheck::Intact => {}
                                DirCheck::Changed => {
                                    status = ItemStatus::DeletedUnexpectedly;
                                    o = Outcome::DeletedUnexpectedly;
                                }
                                DirCheck::Unknown => {
                                    status = ItemStatus::Unverified;
                                    o = Outcome::Unverified;
                                }
                            }
                        }
                        let mut st = StatsSnapshot::default();
                        match status {
                            ItemStatus::Recycled => {
                                st.bytes = p.disk;
                                st.logical = p.logical;
                                st.files = it.estimate.files;
                                st.dirs = it.estimate.dirs;
                            }
                            ItemStatus::NotRecycled(RecycleFail::InUse) => st.in_use += 1,
                            ItemStatus::NotRecycled(RecycleFail::Denied) => st.denied += 1,
                            ItemStatus::NotRecycled(_) => st.other_errors += 1,
                            ItemStatus::Root(RootStatus::Changed) => st.changed += 1,
                            _ => {}
                        }
                        // SAFETY STOP: after an unexpected deletion or an unverifiable result, submit nothing else.
                        if matches!(
                            status,
                            ItemStatus::DeletedUnexpectedly | ItemStatus::Unverified
                        ) {
                            stopped = true;
                            report.recycle_stopped = true;
                            report.stop_reason.get_or_insert(
                                if status == ItemStatus::DeletedUnexpectedly {
                                    StopReason::DeletedUnexpectedly
                                } else {
                                    StopReason::Unverified
                                },
                            );
                        }
                        // The shell reported "deleted" but the item remains in place (see `judge`): stop sending anyway.
                        if matches!(status, ItemStatus::NotRecycled(RecycleFail::Shell(_)))
                            && ev.is_some_and(|e| {
                                e.succeeded() && e.state == recycle::BinState::Deleted
                            })
                        {
                            stopped = true;
                            report.recycle_stopped = true;
                            report.stop_reason.get_or_insert(StopReason::ShellDeclined);
                        }
                        emit(it, Some(p.identity), p.links, o);
                        if let Some(l) = &opts.log {
                            l.write_note(
                                "recycle",
                                &format!(
                                    "{} -> {:?} (hresult 0x{:08X})",
                                    p.path.display(),
                                    status,
                                    ev.map_or(0, |e| e.hr as u32)
                                ),
                            );
                            l.flush();
                        }
                        results.insert(it.id, (status, st));
                    }
                    // A veto (or any other receiver stop) also stops the remaining submissions.
                    if outcome.sink_aborted && !report.recycle_stopped {
                        // H5 (conservative decision documented in PLAN): one failure for a FILE with pending items in its batch
                        // stops ALL submissions, even though a file cannot have partial data loss. Requeuing the remaining
                        // items in another operation was rejected for simplicity; this case is rare because the guard handle
                        // prevents an external process from locking a file after batch preparation.
                        // A2-M1: if the stop was caused only by a result FAILURE (not a veto or loss; those already stopped
                        // submission above), cancel the remaining items in the SAME batch (mark them `Stopped`, intact, and
                        // checked), then stop all other submissions. If the failure was the batch's last item (always true
                        // for a directory, which runs alone), nothing was pending, so subsequent operations continue.
                        let failure_only = outcome.vetoed.is_empty()
                            && outcome.events.iter().any(|e| !e.succeeded());
                        let cancelled_pending = event_of.iter().any(Option::is_none);
                        if !failure_only || cancelled_pending {
                            stopped = true;
                            report.recycle_stopped = true;
                            report.stop_reason.get_or_insert(if failure_only {
                                StopReason::ShellDeclined
                            } else {
                                StopReason::Changed
                            });
                        }
                    }
                    // The operation's guard handles are released here.
                    drop(entries);
                }
            }
        }
        for it in items {
            let (status, mut stats) = results.remove(&it.id).unwrap_or((
                ItemStatus::Root(RootStatus::Cancelled),
                StatsSnapshot::default(),
            ));
            // Counters for preserved items (protected, recent...) come from the approval measurement.
            if matches!(status, ItemStatus::Recycled | ItemStatus::WouldRecycle) {
                stats.shared_files = it.estimate.shared_files;
                stats.shared_bytes = it.estimate.shared_bytes;
                stats.protected = it.estimate.protected;
                stats.reparse = it.estimate.reparse;
            }
            let removed = if status == ItemStatus::Recycled {
                vec![Removed {
                    path: it.path.clone(),
                    is_dir: it.is_dir(),
                    removed: true,
                    bytes: stats.bytes,
                    logical: stats.logical,
                    files: it.estimate.files,
                }]
            } else {
                Vec::new()
            };
            report.items.push(ItemReport {
                id: it.id,
                category: it.category,
                path: it.path.clone(),
                destination: Destination::Recycle,
                status,
                stats,
                removed,
            });
            progress.items_done.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Preflight check for an item going to the Recycle Bin:
    /// * open it exclusively (an in-use file fails with `SHARING_VIOLATION`; a directory is opened with sharing, and the
    ///   shell itself fails to move it if anything inside is open), then verify its identity and resolved path (local,
    ///   approved, and allowed);
    /// * keep a GUARD handle (`DELETE | READ_ATTRIBUTES`, sharing all access) open throughout the shell operation and until
    ///   after the item is judged. While it is open, Windows does NOT allow renaming the item's parent or grandparent (error 5),
    ///   preventing an ancestor from being replaced with a junction to another folder between this check and the shell opening
    ///   the item. The shell can still recycle it (the handle shares `DELETE`): the guard does NOT prevent renaming the item
    ///   itself or placing a replacement at its path. `PreDeleteItem` monitors this by comparing identity just before the shell
    ///   acts. The guard also allows a final check that the approved object ended up in the Recycle Bin.
    fn prepare_recycle(&self, it: &PlanItem, st: &mut StatsSnapshot) -> Prepared {
        let UnitSpec::Entry {
            path,
            approved,
            approved_tag,
            approved_links,
            is_dir,
        } = &it.spec
        else {
            return Prepared::Rejected(ItemStatus::Root(RootStatus::Changed), Outcome::Changed);
        };
        #[cfg(test)]
        crate::engine::test_hooks::before_open(path);
        let opened = if *is_dir {
            sys::open_path_for_rename(path)
        } else {
            sys::open_path_exclusive(path)
        };
        let h = match opened {
            Ok(h) => h,
            Err(c) => return map_open_error(c, st),
        };
        let m = match sys::meta(&h) {
            Ok(m) => m,
            Err(c) => return map_open_error(c, st),
        };
        let reparse_ok = !m.is_reparse()
            || (!m.is_dir && m.reparse_tag == *approved_tag && sys::is_cloud_tag(m.reparse_tag));
        // Same identity but a different link count: someone added or removed names since approval.
        if m.identity != *approved
            || m.is_dir != *is_dir
            || !reparse_ok
            || m.links != *approved_links
        {
            st.changed += 1;
            return Prepared::Rejected(ItemStatus::Root(RootStatus::Changed), Outcome::Changed);
        }
        // M-1: a unit with a minimum age (reports, dumps) is submitted only if the object is STILL that old. Identity and size
        // are not enough: a dump touched after approval retains both.
        if too_young(it, m.newest_filetime) {
            st.changed += 1;
            return Prepared::Rejected(ItemStatus::Root(RootStatus::Changed), Outcome::Young);
        }
        let final_path = match sys::final_path(&h) {
            Ok(p) => p,
            Err(c) => return map_open_error(c, st),
        };
        if !volume::is_local_path(&final_path) {
            st.protected += 1;
            return Prepared::Rejected(ItemStatus::Root(RootStatus::NotLocal), Outcome::Protected);
        }
        let key = PathKey::new(&final_path);
        if PathKey::new(path) != key || self.protection.target_verdict(&key) != Verdict::Allowed {
            st.protected += 1;
            return Prepared::Rejected(ItemStatus::Root(RootStatus::Protected), Outcome::Protected);
        }
        // The guard handle. A directory was already opened sharing everything with `DELETE`; that handle is the guard. A file
        // was opened exclusively (to detect "in use"), so release it, open the shared guard handle, and verify again that it
        // is the same object at the same path.
        let held = if *is_dir {
            h
        } else {
            drop(h);
            let g = match sys::open_path_for_rename(&final_path) {
                Ok(g) => g,
                Err(c) => return map_open_error(c, st),
            };
            let same = sys::meta(&g).is_ok_and(|m2| {
                m2.identity == *approved
                    && !m2.is_dir
                    && m2.reparse_tag == m.reparse_tag
                    && m2.links == *approved_links
            }) && sys::final_path(&g).is_ok_and(|p| PathKey::new(&p) == key);
            if !same {
                st.changed += 1;
                return Prepared::Rejected(ItemStatus::Root(RootStatus::Changed), Outcome::Changed);
            }
            g
        };
        // Actual size compared with the approval measurement. Do NOT remeasure a directory here (a chunk can contain up to 32
        // items, and time passes before its turn): measure it JUST BEFORE submission, alone in its operation, in `fresh_size`
        // (H6.2, A2-B1). Until then, use the approved measurement.
        let estimate = it.estimate.recycle_charge();
        // For a file, compare its actual size with the approval measurement and use the larger value (provisional: the guard
        // measures it again JUST BEFORE the operation in `fresh_size`).
        let (size, last) = if m.is_dir {
            (estimate, None)
        } else {
            (
                estimate.max(m.alloc).max(m.logical),
                Some(m.alloc.max(m.logical)),
            )
        };
        Prepared::Ready(Box::new(Pending {
            path: final_path,
            identity: m.identity,
            links: m.links,
            held: Arc::new(held),
            size,
            last,
            // What to report if it reaches the Recycle Bin: the approved amount until `fresh_size` measures it again (GM-m6).
            disk: if m.is_dir || m.links > 1 {
                it.estimate.bytes
            } else {
                m.alloc
            },
            logical: if m.is_dir {
                it.estimate.logical
            } else {
                m.logical
            },
            snapshot: None,
            watch: None,
        }))
    }

    /// R6 + A2-m1: does the directory still match its state just before submission? Three outcomes:
    /// * `Intact`: a COMPLETE measurement with the same fingerprint (sizes, entry count, and identities);
    /// * `Changed`: a COMPLETE measurement that differs in any way (something was lost or changed);
    /// * `Unknown`: the measurement was incomplete (A-B2: an unlisted subfolder, an entry that changed or disappeared during
    ///   traversal, an open failure, or cancellation), or there is no earlier measurement to compare. This proves nothing:
    ///   neither that the directory is intact nor that anything was lost.
    fn dir_check(&self, it: &PlanItem, p: &Pending, cancel: &Arc<AtomicBool>) -> DirCheck {
        match (self.remeasure(it, cancel), p.snapshot) {
            (Remeasure::Now(after), Some(before)) => {
                if snap_of(&after) == before {
                    DirCheck::Intact
                } else {
                    DirCheck::Changed
                }
            }
            _ => DirCheck::Unknown,
        }
    }

    /// B2 + H6.2 + A-B1 + A2-B1: an item's size may have GROWN since approval, including while it waited for its turn (a shell
    /// operation can take time). Remeasure it JUST BEFORE submission (a directory alone in its operation; a file in its batch,
    /// where `PreDeleteItem` measures it again), rather than measuring the entire chunk during preparation or measuring only once:
    /// * a FILE, through its guard handle (`FileStandardInfo`: on-disk and logical sizes), without reopening the path, while
    ///   also checking that it is still the same object (identity, links, and not pending deletion);
    /// * a DIRECTORY, using the same traversal (relative, without entering reparse points), and only if the measurement is
    ///   COMPLETE (A-B2).
    ///
    /// If it grew (compared with approval or the latest measurement), do not submit it: the grouping is based on a size that
    /// is no longer valid, and `Err` carries the "changed since approval" status. If it shrank, use the new size. The caller
    /// rechecks the Recycle Bin budget (`take_op`) using these updated sizes.
    fn fresh_size(
        &self,
        it: &PlanItem,
        p: &mut Pending,
        st: &mut StatsSnapshot,
        cancel: &Arc<AtomicBool>,
    ) -> Result<(), (ItemStatus, Outcome)> {
        let changed = |st: &mut StatsSnapshot| {
            st.changed += 1;
            Err((ItemStatus::Root(RootStatus::Changed), Outcome::Changed))
        };
        // An item that no longer meets the minimum age has the same status ("changed since approval"), but the log records it
        // as "too recent" (S-7), not as a generic change.
        let young = |st: &mut StatsSnapshot| {
            st.changed += 1;
            Err((ItemStatus::Root(RootStatus::Changed), Outcome::Young))
        };
        if !it.is_dir() {
            // Use the guard handle to check the approved object, not whatever is currently at the path.
            let Ok(m) = sys::meta(&p.held) else {
                return changed(st);
            };
            if m.identity != p.identity || m.is_dir || m.delete_pending || m.links != p.links {
                return changed(st);
            }
            // M-1: check the minimum age again, now just before the operation.
            if too_young(it, m.newest_filetime) {
                return young(st);
            }
            let fresh = m.alloc.max(m.logical);
            // If it grew compared with the APPROVED size or the latest actual measurement (`prepare_recycle` and later checks),
            // do not submit it: operation grouping was decided using a size that is no longer valid.
            let approved = it.estimate.recycle_charge();
            if fresh > approved || p.last.is_some_and(|l| fresh > l) {
                return changed(st);
            }
            p.size = fresh;
            p.last = Some(fresh);
            // GM-m6: report its CURRENT size if it reaches the Recycle Bin (including a file that shrank).
            p.disk = if m.links > 1 { 0 } else { m.alloc };
            p.logical = m.logical;
            return Ok(());
        }
        // M-1: check the folder's own timestamp (the fingerprint checks its contents: a recent file changes that entry's
        // class). If the timestamp cannot be read or this is a different object, do not submit it.
        match sys::meta(&p.held) {
            Ok(m) if !too_young(it, m.newest_filetime) => {}
            Ok(_) => return young(st),
            Err(_) => return changed(st),
        }
        // A3-M1: open the change monitor BEFORE traversal (changes before that would not be seen) and keep it alive until the
        // operation's `PreDeleteItem`: a folder can grow WHILE it is being measured (the traversal may have counted the first
        // file before it grew, without noticing). Use a second directory handle with verified identity; if it cannot be opened
        // or monitoring cannot start, do NOT submit the folder (fail closed).
        let watch = match sys::DirWatch::start(&p.path, p.identity) {
            Ok(w) => w,
            Err(sys::WatchError::Changed) => return changed(st),
            Err(sys::WatchError::Os(c)) => {
                st.other_errors += 1;
                return Err((ItemStatus::Root(RootStatus::Failed(c)), Outcome::Failed(c)));
            }
        };
        match self.remeasure(it, cancel) {
            Remeasure::Now(_) if watch.changed() => changed(st),
            Remeasure::Now(fresh) => {
                // GM-B3: submit a directory only if its contents are the SAME as approved: identical fingerprint (identity,
                // sizes, links, and class of every entry, independent of order), identical counts, and no MORE protected items,
                // reparse points, or cloud files. Previously, only growth was checked: if a file disappeared and a smaller one
                // appeared, the folder was submitted with unapproved contents. Any change to a folder's contents, including
                // shrinkage, requires confirmation again.
                let est = &it.estimate;
                let same = snap_of(&fresh) == snap_of(est)
                    && fresh.reparse <= est.reparse
                    && fresh.cloud <= est.cloud
                    && fresh.protected <= est.protected;
                let size = fresh.recycle_charge();
                if !same || p.last.is_some_and(|l| size > l) {
                    return changed(st);
                }
                p.size = size;
                p.last = Some(size);
                p.disk = fresh.bytes;
                p.logical = fresh.logical;
                p.snapshot = Some(snap_of(&fresh));
                p.watch = Some(Arc::new(watch));
                Ok(())
            }
            Remeasure::Cancelled => Err((
                ItemStatus::Root(RootStatus::Cancelled),
                Outcome::NotRecycled,
            )),
            Remeasure::Incomplete | Remeasure::Failed => changed(st),
        }
    }
}

/// What is known about the contents of a directory the shell did not submit (see [`Cleaner::dir_check`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirCheck {
    /// Complete measurement, identical to the one taken before submission.
    Intact,
    /// Complete measurement, but different: something was lost or changed.
    Changed,
    /// Measurement was incomplete: nothing can be concluded.
    Unknown,
}

/// Result of remeasuring a directory just before submission.
enum Remeasure {
    /// Measurement from a COMPLETE traversal (see [`StatsSnapshot::is_complete`]).
    Now(Box<StatsSnapshot>),
    Cancelled,
    /// Traversal finished but did NOT see everything (an unlisted subfolder, an entry that changed or disappeared...): its
    /// result is invalid. Treat it as "changed since approval" when submitting and "NOT intact" when judging (R6).
    Incomplete,
    /// Could not measure (changed, no longer exists, protected...): treat it as "changed since approval".
    Failed,
}

impl Cleaner {
    /// Remeasures the approved directory for `it` using the same traversal as approval (`Measure` mode: relative opening,
    /// verified identity, and no traversal into reparse points).
    fn remeasure(&self, it: &PlanItem, cancel: &Arc<AtomicBool>) -> Remeasure {
        let progress = Arc::new(AtomicU64::new(0));
        let unit = Unit::new(
            it.id,
            it.spec.clone(),
            self.env(Mode::Measure, it.min_age, cancel, &progress, None, false),
        );
        engine::run(std::slice::from_ref(&unit), self.threads);
        // A traversal interrupted by cancellation (or a thread panic, which also triggers it) did not see everything.
        if cancel.load(Ordering::Relaxed) {
            return Remeasure::Cancelled;
        }
        match unit.status() {
            // A-B2: `Done` only means the ROOT was opened and listed. The measurement is valid only if the entire traversal
            // completed: an unlisted subfolder, an entry that changed or disappeared, an open error... all leave contents uncounted.
            Some(RootStatus::Done) => {
                let s = unit.stats.snapshot();
                if s.is_complete() {
                    Remeasure::Now(Box::new(s))
                } else {
                    Remeasure::Incomplete
                }
            }
            Some(RootStatus::Cancelled) => Remeasure::Cancelled,
            _ => Remeasure::Failed,
        }
    }
}

/// Items per chunk (and per cancellation check).
const RECYCLE_CHUNK: usize = 32;

/// An item checked and ready to submit to the shell using its original path.
struct Pending {
    path: PathBuf,
    identity: FileIdentity,
    links: u32,
    /// Guard handle (see `prepare_recycle`): kept open until after the item is judged. Shared (`Arc`) because the shell
    /// receiver uses it to remeasure a file in `PreDeleteItem` (A2-B1).
    held: Arc<sys::Handle>,
    /// Size used to group items into operations: provisional during preparation (the larger of the approved size and current
    /// size), then the CURRENT size after each [`Cleaner::fresh_size`] check.
    size: u64,
    /// Latest actual measurement (a file through its guard handle; a directory through traversal), or `None` if not measured
    /// yet. If the current measurement is GREATER, the item grew while waiting for its turn and is not submitted.
    last: Option<u64>,
    /// Directory measurement (size and entry count) taken just before submission to the shell: `dir_check` compares it later
    /// if the shell reports "deleted" without providing a Recycle Bin item (R6).
    snapshot: Option<DirSnap>,
    /// What is reported if the item reaches the Recycle Bin: current on-disk bytes (excluding shared links) and logical size
    /// (GM-m6: a file that shrank since approval is not reported using its approved size).
    disk: u64,
    /// A3-M1: for a DIRECTORY only. Change monitor opened BEFORE the measurement traversal and kept alive through the
    /// operation's `PreDeleteItem` (or until the item is released): any change inside invalidates the measurement.
    watch: Option<Arc<sys::DirWatch>>,
    logical: u64,
}

/// Directory values compared before and after: on-disk bytes, logical size, files, directories, shared items, and the
/// contents FINGERPRINT (order-independent: identity, sizes, and links of every entry). Totals alone are insufficient:
/// removing one child and creating another of the same size leaves them unchanged (A-M1).
type DirSnap = (u64, u64, u64, u64, u64, u64, u64, u64, u64, u64, u64);

fn snap_of(s: &StatsSnapshot) -> DirSnap {
    (
        s.bytes,
        s.logical,
        s.files,
        s.dirs,
        s.shared_files,
        s.shared_bytes,
        s.fp_sum,
        s.fp_xor,
        // Longest path inside (GM-M3): renaming an item to a longer name can change whether it fits in the Recycle Bin.
        s.max_path,
        // Space used by reparse points inside (GM-B1).
        s.reparse_bytes,
        s.reparse_logical,
    )
}

/// Recycle Bin space on a volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BinRoom {
    /// Configured maximum size (`MaxCapacity`); `None` if the registry does not provide it.
    pub max: Option<u64>,
    /// Current Recycle Bin occupancy on that volume; `None` if it could not be read.
    pub used: Option<u64>,
}

impl BinRoom {
    pub const UNKNOWN: BinRoom = BinRoom {
        max: None,
        used: None,
    };

    /// Conservative result from two readings of the same volume (A3-M2): the HIGHER occupancy and LOWER capacity. If either
    /// reading lacks either value, the result omits it and `take_op` fails closed (`NoRoom`).
    pub fn worst(self, other: BinRoom) -> BinRoom {
        let pick = |a: Option<u64>, b: Option<u64>, f: fn(u64, u64) -> u64| match (a, b) {
            (Some(x), Some(y)) => Some(f(x, y)),
            _ => None,
        };
        BinRoom {
            max: pick(self.max, other.max, u64::min),
            used: pick(self.used, other.used, u64::max),
        }
    }
}

/// Shell operations required by the executor to send items to the Recycle Bin. Production uses `IFileOperation`; tests
/// provide a mock to exercise operation grouping and safety stops without a real operation deleting or evicting unrelated items.
pub(crate) trait RecycleShell: Send + Sync {
    /// Sends items (original path + approved identity) in ONE operation and reports the shell's notification for each item.
    /// The event receiver vetoes any item that is no longer the approved object.
    fn recycle(&self, items: &[recycle::SendItem]) -> recycle::BatchOutcome;
    /// Purges (permanently deletes) the Recycle Bin items enumerated at approval. Production uses the real shell; tests use
    /// a mock to exercise grouping and exclusions without touching anyone's Recycle Bin (M2).
    fn purge(
        &self,
        items: &[recycle::BinItem],
        cancel: &AtomicBool,
        obs: &mut dyn FnMut(recycle::PurgeStep<'_>),
    ) -> Result<recycle::BatchOutcome, recycle::ShellError> {
        recycle::purge_items_observed(items, cancel, obs)
    }
    /// Capacity and occupancy of the Recycle Bin on the volume rooted at `root`.
    fn bin_room(&self, root: &Path) -> BinRoom;
    /// Does an item of `bytes` at `path` fit in its volume's Recycle Bin? Decided at approval and checked again at execution
    /// (the production shell implementation queries the registry and policies; tests with a mock shell do not depend on the host).
    fn fit(&self, path: &Path, bytes: u64) -> Fit;
    /// Like [`RecycleShell::fit`], but also receives the measured length of the longest path inside the item (UTF-16 units).
    /// The default implementation ignores it (mock shells do not depend on path length).
    fn fit_deep(&self, path: &Path, bytes: u64, _deepest: usize) -> Fit {
        self.fit(path, bytes)
    }
    /// Is `p` inside a Recycle Bin (`<drive>:\$Recycle.Bin\...`)?
    fn is_bin_path(&self, p: &Path) -> bool;
}

/// The real shell implementation.
pub(crate) struct RealShell;

impl RecycleShell for RealShell {
    fn recycle(&self, items: &[recycle::SendItem]) -> recycle::BatchOutcome {
        match recycle::recycle_paths(items, RecycleMode::Normal) {
            Ok(o) => o,
            // Do not swallow a COM initialization or operation failure: without events, each item remains unprocessed (or
            // unverified), and the error is included in the result.
            Err(e) => recycle::BatchOutcome {
                error: Some(e),
                ..recycle::BatchOutcome::default()
            },
        }
    }
    fn bin_room(&self, root: &Path) -> BinRoom {
        room_from(volume::bin_limit(root), recycle::bin_size_on(root).ok())
    }
    fn fit(&self, path: &Path, bytes: u64) -> Fit {
        fit_for(path, bytes)
    }
    fn fit_deep(&self, path: &Path, bytes: u64, deepest: usize) -> Fit {
        fit_for_deep(path, bytes, deepest)
    }
    fn is_bin_path(&self, p: &Path) -> bool {
        recycle::is_recycle_bin_path(p)
    }
}

/// Recycle Bin capacity for grouping: ONLY the configured value (`MaxCapacity`). If there is no Recycle Bin, it is disabled,
/// its size is indeterminate, or the registry has no value (the shell uses an undocumented default percentage), there is no
/// capacity that can be guaranteed: `max = None` and [`take_op`] fails closed (`NoRoom`). No percentage of the volume is
/// assumed (B2).
pub(crate) fn room_from(limit: volume::BinLimit, used: Option<u64>) -> BinRoom {
    let max = match limit {
        volume::BinLimit::Max(m) => Some(m),
        _ => None,
    };
    BinRoom { max, used }
}

/// What to do with the head of a volume's queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Take {
    /// Send the first `n` items in ONE operation.
    Send(usize),
    /// There is no room for the first item: submit nothing.
    NoRoom,
}

/// Groups items into operations: each operation can use at most 90% of Recycle Bin capacity MINUS its current occupancy.
/// * FAIL CLOSED (B2): without known capacity (`max`) or occupancy (`used`), fit cannot be guaranteed, and the shell
///   permanently deletes items that do not fit: return `NoRoom`, never `Send`.
/// * An item larger than 10% of capacity goes ALONE.
/// * If the first item does not fit even by itself in the remaining space, return `NoRoom`: submitting it could make the
///   shell delete it or evict items already in the Recycle Bin.
pub(crate) fn take_op(sizes: &[u64], room: BinRoom) -> Take {
    let Some(&first) = sizes.first() else {
        return Take::Send(0);
    };
    let (Some(max), Some(used)) = (room.max, room.used) else {
        return Take::NoRoom;
    };
    let cap = bin_cap(max);
    let budget = cap.saturating_sub(used);
    if first > budget {
        return Take::NoRoom;
    }
    let big = bin_big(max);
    if first > big {
        return Take::Send(1);
    }
    let mut n = 1;
    let mut sum = first;
    while n < sizes.len() && sizes[n] <= big && sum.saturating_add(sizes[n]) <= budget {
        sum += sizes[n];
        n += 1;
    }
    Take::Send(n)
}

/// What happened to an item according to the shell. Compare by IDENTITY (volume + FileId), never by name: `.lnk`, `.url`,
/// known extensions, and `HideFileExt` do not matter, and the Recycle Bin is not enumerated. Identity alone is insufficient
/// (another name for the same object, such as a hard link, shares it): `Recycled` also requires that
/// * the approved NAME no longer exists at its original path;
/// * the guard handle, which follows the approved object, is now INSIDE a Recycle Bin; and
/// * the object retains its link count (renaming it into the Recycle Bin does not change that count).
///
/// `aborted`: the shell reported that some operation was aborted (without saying which one), so an item with no notification
/// is counted as aborted. `sink_aborted`: the receiver stopped the batch (M2), so an item with no notification that remains
/// in place was not processed because of the stop (`Stopped`).
fn judge(
    p: &Pending,
    ev: Option<&recycle::DeleteEvent>,
    shell: &dyn RecycleShell,
    aborted: bool,
    sink_aborted: bool,
) -> (ItemStatus, Outcome) {
    use recycle::BinState;
    let Some(ev) = ev else {
        // The shell did not process the item. Report "not sent" only if the approved OBJECT remains intact in place (A-M1).
        // If its guard is pending deletion, the shell deleted it without notification; if the state is unknown, claim nothing.
        return match in_place(p) {
            InPlace::Intact => {
                let why = if sink_aborted {
                    RecycleFail::Stopped
                } else if aborted {
                    RecycleFail::Aborted
                } else {
                    RecycleFail::NotProcessed
                };
                (ItemStatus::NotRecycled(why), Outcome::NotRecycled)
            }
            InPlace::Deleted => (
                ItemStatus::DeletedUnexpectedly,
                Outcome::DeletedUnexpectedly,
            ),
            InPlace::Unknown => (ItemStatus::Unverified, Outcome::Unverified),
        };
    };
    if !ev.succeeded() {
        let fail = classify_hr(ev.hr);
        let o = match fail {
            RecycleFail::InUse => Outcome::InUse,
            RecycleFail::Denied => Outcome::Denied,
            _ => Outcome::NotRecycled,
        };
        // A-M1: a FAILURE result does not prove the shell left the item untouched (it may have lost a child before failing).
        // Apply the same judgment as for deletion: is the approved object intact in place? (For a directory, the caller also
        // checks its contents.)
        return match in_place(p) {
            InPlace::Intact => (ItemStatus::NotRecycled(fail), o),
            InPlace::Deleted => (
                ItemStatus::DeletedUnexpectedly,
                Outcome::DeletedUnexpectedly,
            ),
            InPlace::Unknown => (ItemStatus::Unverified, Outcome::Unverified),
        };
    }
    match &ev.state {
        BinState::Deleted => {
            // A successful result with null `psiNewlyCreated` usually means permanent deletion, but the shell also reports
            // it without touching the item (observed: a folder with a huge sparse file returns `hr = 0x00270003`, null, and
            // the folder remains on disk). Decide by OBJECT, not name: with the guard handle still open, a file the shell DID
            // delete keeps its name visible as "delete pending" until the guard is released (observed: `hr = 0x00270008`,
            // the name exists, opening it is denied, and `DeletePending` is true). Only if the approved object is NOT pending
            // deletion AND reopening the path returns the SAME identity did the shell leave it untouched. Every other state
            // (delete pending, different identity, unreadable path, unreadable handle) means permanent deletion.
            if untouched_by_shell(p) {
                (
                    ItemStatus::NotRecycled(RecycleFail::Shell(ev.hr)),
                    Outcome::NotRecycled,
                )
            } else {
                (
                    ItemStatus::DeletedUnexpectedly,
                    Outcome::DeletedUnexpectedly,
                )
            }
        }
        BinState::InBinUnknownPath => (ItemStatus::Unverified, Outcome::Unverified),
        BinState::InBin(bin_path) => {
            let found = sys::open_for_query(bin_path)
                .and_then(|h| sys::meta(&h))
                .map(|m| m.identity);
            match found {
                Ok(id) if id == p.identity => {
                    let name_gone = std::fs::symlink_metadata(&p.path).is_err();
                    let held_in_bin = sys::final_path(&p.held)
                        .ok()
                        .is_some_and(|f| shell.is_bin_path(&f));
                    // The object's link count cannot have changed since the check: a legitimate recycle is a rename and
                    // preserves it. If it changed, someone added or removed names during the race window.
                    let same_links = sys::meta(&p.held).is_ok_and(|m| m.links == p.links);
                    if name_gone && held_in_bin && same_links {
                        (ItemStatus::Recycled, Outcome::Recycled)
                    } else {
                        // Same object under another name, or the approved object is not the one in the Recycle Bin.
                        (ItemStatus::RecycledOther, Outcome::RecycledOther)
                    }
                }
                Ok(_) => (ItemStatus::RecycledOther, Outcome::RecycledOther),
                Err(_) => (ItemStatus::Unverified, Outcome::Unverified),
            }
        }
    }
}

/// Is the approved object still intact in place? `true` only if the guard handle is NOT pending deletion and reopening the
/// path returns the same identity. This is `judge`'s object-based check (R1).
fn untouched_by_shell(p: &Pending) -> bool {
    let not_pending = sys::meta(&p.held).is_ok_and(|m| !m.delete_pending);
    let same_object = sys::open_for_query(&p.path)
        .and_then(|h| sys::meta(&h))
        .is_ok_and(|m| m.identity == p.identity);
    not_pending && same_object
}

/// State of the approved object after a shell operation that did not send it (A-M1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InPlace {
    /// The guard handle is not pending deletion AND reopening the path returns the same identity: the item remains in place.
    Intact,
    /// The guard handle is pending deletion: the shell deleted the item (its name remains visible until the guard is released).
    Deleted,
    /// Neither intact nor pending deletion (the path no longer refers to the object, or could not be read): nothing can be concluded.
    Unknown,
}

/// Object-based judgment for an item that did not reach the Recycle Bin (see [`InPlace`]).
fn in_place(p: &Pending) -> InPlace {
    match sys::meta(&p.held) {
        Ok(m) if m.delete_pending => InPlace::Deleted,
        Ok(_) if untouched_by_shell(p) => InPlace::Intact,
        _ => InPlace::Unknown,
    }
}

/// Translates the `HRESULT` from a failed shell deletion.
fn classify_hr(hr: i32) -> RecycleFail {
    use windows::Win32::UI::Shell::{
        COPYENGINE_E_ACCESS_DENIED_DEST, COPYENGINE_E_ACCESS_DENIED_SRC,
        COPYENGINE_E_SHARING_VIOLATION_DEST, COPYENGINE_E_SHARING_VIOLATION_SRC,
    };
    let code = hr as u32;
    // HRESULT_FROM_WIN32(x) = 0x8007_0000 | x.
    let win32 = (code & 0xFFFF_0000 == 0x8007_0000).then_some(code & 0xFFFF);
    match win32 {
        Some(32 | 33) => RecycleFail::InUse,
        Some(5) => RecycleFail::Denied,
        _ => {
            // Shell copy-engine codes (`COPYENGINE_E_*`). `ACCESS_DENIED_SRC` was OBSERVED when moving a folder with an open
            // file inside to the Recycle Bin (moving is a rename).
            let h = HRESULT(hr);
            if h == COPYENGINE_E_SHARING_VIOLATION_SRC || h == COPYENGINE_E_SHARING_VIOLATION_DEST {
                RecycleFail::InUse
            } else if h == COPYENGINE_E_ACCESS_DENIED_SRC || h == COPYENGINE_E_ACCESS_DENIED_DEST {
                RecycleFail::Denied
            } else {
                RecycleFail::Shell(hr)
            }
        }
    }
}

/// Is the object (using its latest creation, write, and change timestamp) newer than `it`'s minimum age? Never, if there is
/// no minimum age. A fast clock or future timestamps count as "recent" (fail closed).
fn too_young(it: &PlanItem, newest_filetime: i64) -> bool {
    let min = engine::duration_to_ft(it.min_age);
    min > 0 && engine::now_filetime() - newest_filetime < min
}

enum Prepared {
    Ready(Box<Pending>),
    Rejected(ItemStatus, Outcome),
}

fn map_open_error(c: sys::Win32, st: &mut StatsSnapshot) -> Prepared {
    match c {
        2 | 3 => Prepared::Rejected(ItemStatus::Root(RootStatus::Missing), Outcome::Failed(c)),
        sys::SHARING_VIOLATION => {
            st.in_use += 1;
            Prepared::Rejected(ItemStatus::NotRecycled(RecycleFail::InUse), Outcome::InUse)
        }
        sys::ACCESS_DENIED => {
            st.denied += 1;
            Prepared::Rejected(
                ItemStatus::NotRecycled(RecycleFail::Denied),
                Outcome::Denied,
            )
        }
        other => {
            st.other_errors += 1;
            Prepared::Rejected(
                ItemStatus::Root(RootStatus::Failed(other)),
                Outcome::Failed(other),
            )
        }
    }
}

/// Items that disappeared during permanent deletion, with full paths.
fn removed_of(it: &PlanItem, u: &Unit) -> Vec<Removed> {
    match &it.spec {
        UnitSpec::Contents { path, .. } => u
            .tops()
            .into_iter()
            .map(|t| Removed {
                path: path.join(&t.name),
                is_dir: t.is_dir,
                removed: t.removed,
                bytes: t.bytes,
                logical: t.logical,
                files: t.files,
            })
            .collect(),
        UnitSpec::Entry { path, .. } => u
            .root_result()
            .into_iter()
            .map(|t| Removed {
                path: path.clone(),
                is_dir: t.is_dir,
                removed: t.removed,
                bytes: t.bytes,
                logical: t.logical,
                files: t.files,
            })
            .collect(),
    }
}

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;
