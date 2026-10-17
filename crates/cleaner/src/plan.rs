//! Approval (PLAN.md §3.7.1): turn the basket into a [`Plan`] with IDENTITIES.
//!
//! When the user confirms, `cleaner` reopens each item by handle (`FILE_FLAG_OPEN_REPARSE_POINT | BACKUP_SEMANTICS`) and records
//! `(volume serial number, 128-bit FileId)` and `NumberOfLinks`. Paths only help find the objects again;
//! the object opened during execution must have THAT identity.
//!
//! Approval also:
//! * revalidates protection against the path resolved by the handle (not the input string);
//! * expands a folder containing protected items into its allowed parts ("N protected items will be skipped");
//! * measures each item with the same traversal used for deletion (`Measure` mode): disk size WITHOUT shared hard
//!   links, logical size, files, and protected/recent entries and links that will be skipped;
//! * decides whether each item targeting the Recycle Bin FITS ([`Fit`]), using the volume's actual limit;
//! * captures the Recycle Bin listing if emptying it was requested (enumerated scope).

use crate::basket::{Basket, Source};
use crate::engine::{self, Env, Mode, StatsSnapshot, Unit, UnitSpec};
use crate::paths::PathKey;
use crate::protect::{ChildAction, Protection, Verdict};
use crate::recycle::BinItem;
use crate::roots::{RootRejection, Roots};
use crate::rules::{Action, CategoryId, Destination, Risk, Rules, rules};
use crate::sys;
use crate::volume::{self, BinLimit};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::time::Duration;

/// Why an item does NOT fit in the Recycle Bin (shown in the amber confirmation group).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnfitReason {
    /// The drive has no Recycle Bin (network or removable).
    NoBin,
    /// The user disabled the Recycle Bin for this volume ("don't move files to the Recycle Bin").
    NukeOnDelete,
    /// Exceeds the volume's maximum Recycle Bin size (including the safety margin).
    TooBig,
    /// The path exceeds the Recycle Bin's 260-character limit.
    PathTooLong,
    /// A system policy ("Don't move deleted files to the Recycle Bin") makes the shell permanently delete items.
    Policy,
    /// Could not determine whether it fits (unknown volume or limit); treat it as not fitting.
    Unknown,
    /// The volume's Recycle Bin has NO configured maximum size (`MaxCapacity` is absent from the registry): the shell
    /// uses an undocumented default percentage, so there is no guarantee it fits (B2: fail closed).
    CapacityUnknown,
}

/// Whether an item targeting the Recycle Bin fits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    Fits,
    Unfit(UnfitReason),
}

/// Why a basket entry was not included in the plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    /// No longer exists.
    Missing,
    /// Windows denied access (permissions).
    AccessDenied,
    /// The file system does not provide identity (FAT, exFAT, etc.); unverifiable items are not approved.
    NoIdentity,
    /// It is a link (junction, symlink, or mount point); links are neither followed nor deleted.
    Reparse,
    /// A category root has a link (junction, symlink, or mount point) in its ancestor chain, or the
    /// checked path changed: do not offer it as a category (a link above `User Data` or
    /// `%TEMP%` could redirect it to a protected folder).
    ReparseAncestor,
    /// The user's temporary folder is redirected outside `%LOCALAPPDATA%` (it is not `Local\Temp` or a descendant): do not
    /// offer it as a category. This is not a link in its path (R5).
    OutsideLocalAppData,
    /// The resolved path is not on a local drive letter (UNC, including a local disk reached through an administrative
    /// share, or a network drive): `cleaner` only cleans local drives because protection is defined
    /// over drive paths.
    NotLocal,
    /// Protected: denied, untouchable, or a known folder.
    Protected(Verdict),
    /// The item was going to the Recycle Bin, but the folder could not be measured COMPLETELY (subfolders without access or in use, entries
    /// that changed during traversal, etc.): without the actual size, allocation and fit cannot be checked, and
    /// execution would always fail. Includes the number of unreadable entries. Not offered.
    Incomplete(u64),
    /// A category with a minimum age (error reports, dumps): the item or something inside it was modified
    /// more recently than the required age (7 days). Do not offer it; record it in the plan and log instead of silently hiding it.
    Young,
    /// Any other system error.
    Other(u32),
}

impl RejectReason {
    /// Text recorded in the log.
    pub fn log_text(&self) -> String {
        match self {
            RejectReason::Young => "Young (modified less than the minimum age ago)".to_string(),
            other => format!("{other:?}"),
        }
    }
}

/// A basket entry that did not pass approval.
#[derive(Clone, Debug)]
pub struct Rejected {
    pub path: PathBuf,
    pub reason: RejectReason,
}

/// An approved item.
#[derive(Clone, Debug)]
pub struct PlanItem {
    pub id: usize,
    /// Rule-based category this came from (`None` = "Your file").
    pub category: Option<CategoryId>,
    /// Approved path (the category root or the item).
    pub path: PathBuf,
    pub risk: Risk,
    /// Approved destination.
    pub destination: Destination,
    /// Whether it fits in the Recycle Bin (only for `Recycle`; `Permanent` always means `Fits`).
    pub fit: Fit,
    /// What was approved and with which identity.
    pub spec: UnitSpec,
    /// Rule's minimum age.
    pub min_age: Duration,
    /// Measurement taken during approval.
    pub estimate: StatsSnapshot,
    /// This is a cloud-synced file (or contains one): deleting it also removes it from the cloud.
    pub cloud_synced: bool,
    /// This file has multiple hard links and will not free disk space.
    pub hard_linked: bool,
    /// Volume root containing this item.
    pub volume: Option<PathBuf>,
    /// Under a diagnostic root (`ReportArchive`, `ReportQueue`, or `CrashDumps`), even if manually added as "Your
    /// file": never permanently delete based on its PATH, regardless of category (see [`PlanItem::never_permanent`]).
    pub diagnostic_path: bool,
}

impl PlanItem {
    pub fn is_dir(&self) -> bool {
        match &self.spec {
            UnitSpec::Contents { .. } => true,
            UnitSpec::Entry { is_dir, .. } => *is_dir,
        }
    }
    /// Is it going to the Recycle Bin but does not fit? (The amber confirmation group.)
    pub fn is_unfit(&self) -> bool {
        self.destination == Destination::Recycle && matches!(self.fit, Fit::Unfit(_))
    }
    /// Is PERMANENT deletion prohibited in all cases (including when "does not fit in the Recycle Bin" is checked)?
    ///
    /// * Error reports and crash dumps (diagnostic categories) are never permanently deleted, either
    ///   by category or by path (`diagnostic_path`: an item under one of these roots that was manually added).
    /// * An `Entry` unit with a minimum age: permanent traversal does not recheck each child's age

    ///   or require the folder fingerprint (that could split a report, deleting old items while leaving recent ones), so
    ///   it fails closed. Only diagnostic categories create these today; this prevents another rule from using them without
    ///   reviewing this comparison.
    pub fn never_permanent(&self) -> bool {
        self.category.is_some_and(CategoryId::is_diagnostic)
            || self.diagnostic_path
            || (matches!(self.spec, UnitSpec::Entry { .. }) && !self.min_age.is_zero())
    }
    /// Bytes that will be freed (excluding shared hard links).
    pub fn bytes(&self) -> u64 {
        self.estimate.bytes
    }
}

/// Number of reports or dumps above which confirmation warns that cleanup may take several minutes: the Recycle Bin receives one
/// operation per folder (about 0.15 seconds each according to PLAN §3.7.4).
pub const MANY_DIAGNOSTIC_ITEMS: usize = 200;

/// Recycle Bin listing captured at confirmation (enumerated scope).
#[derive(Clone, Debug)]
pub struct BinPlan {
    /// Item list as it was at confirmation. Only THIS list is purged.
    pub items: Vec<BinItem>,
    /// Approximate Recycle Bin size (as known by the caller; 0 if unknown).
    pub bytes_hint: u64,
}

/// The approved plan.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub items: Vec<PlanItem>,
    pub bin: Option<BinPlan>,
    pub rejected: Vec<Rejected>,
    /// Protected children skipped when expanding folders: (entries, bytes from the listing).
    pub omitted_protected: (u64, u64),
    /// Recycle Bin items NOT listed because their metadata (`$I`) is unreadable (m1): they are not emptied, and their size does not
    /// count toward the promised emptying total.
    pub bin_unlisted: u64,
    /// Outlook was running during approval (informational, never a deletion guard).
    pub outlook_open: bool,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.bin.as_ref().is_none_or(|b| b.items.is_empty())
    }

    /// Items permanently deleted by rule, excluding those sent to the Recycle Bin.
    pub fn permanent_items(&self) -> impl Iterator<Item = &PlanItem> {
        self.items
            .iter()
            .filter(|i| i.destination == Destination::Permanent)
    }

    /// Items sent to the Recycle Bin that fit.
    pub fn recycle_items(&self) -> impl Iterator<Item = &PlanItem> {
        self.items
            .iter()
            .filter(|i| i.destination == Destination::Recycle && !i.is_unfit())
    }

    /// Items that would go to the Recycle Bin but do not fit AND may be permanently deleted if the user confirms (amber
    /// group: "will be permanently deleted"). Excludes items prohibited by [`PlanItem::never_permanent`].
    pub fn unfit_items(&self) -> impl Iterator<Item = &PlanItem> {
        self.items
            .iter()
            .filter(|i| i.is_unfit() && !i.never_permanent())
    }

    /// Items that do not fit and are NEVER permanently deleted (error reports, dumps): leave them where
    /// they are, and state this in confirmation without offering a checkbox.
    pub fn kept_unfit_items(&self) -> impl Iterator<Item = &PlanItem> {
        self.items
            .iter()
            .filter(|i| i.is_unfit() && i.never_permanent())
    }

    /// Number of diagnostic items (error reports, dumps) sent to the Recycle Bin when there are many (more than
    /// [`MANY_DIAGNOSTIC_ITEMS`]); confirmation warns cleanup may take several minutes.
    pub fn many_diagnostic_items(&self) -> Option<usize> {
        let n = self
            .items
            .iter()
            .filter(|i| {
                i.destination == Destination::Recycle
                    && !i.is_unfit()
                    && i.category.is_some_and(CategoryId::is_diagnostic)
            })
            .count();
        (n > MANY_DIAGNOSTIC_ITEMS).then_some(n)
    }

    /// Number of items left in place because they were too recent (see [`RejectReason::Young`]).
    pub fn young_count(&self) -> usize {
        self.rejected
            .iter()
            .filter(|r| r.reason == RejectReason::Young)
            .count()
    }

    pub fn permanent_bytes(&self) -> u64 {
        self.permanent_items().map(PlanItem::bytes).sum()
    }
    pub fn recycle_bytes(&self) -> u64 {
        self.recycle_items().map(PlanItem::bytes).sum()
    }
    pub fn unfit_bytes(&self) -> u64 {
        self.unfit_items().map(PlanItem::bytes).sum()
    }

    /// ((local PST files, bytes), (OST/NST cache files, bytes)).
    pub fn mail_totals(&self) -> ((u64, u64), (u64, u64)) {
        self.items
            .iter()
            .fold(((0, 0), (0, 0)), |((ln, lb), (cn, cb)), i| {
                let e = &i.estimate;
                (
                    (ln + e.mail_local_files, lb + e.mail_local_bytes),
                    (cn + e.mail_cache_files, cb + e.mail_cache_bytes),
                )
            })
    }

    pub fn permanent_mail_files(&self, allow_unfit: bool) -> u64 {
        self.items
            .iter()
            .filter(|i| i.destination == Destination::Permanent || (allow_unfit && i.is_unfit()))
            .map(|i| i.estimate.mail_local_files)
            .sum()
    }

    /// The `n` largest "Your file" items (without a category), in descending order.
    pub fn biggest_user_items(&self, n: usize) -> Vec<&PlanItem> {
        let mut v: Vec<&PlanItem> = self.items.iter().filter(|i| i.category.is_none()).collect();
        v.sort_by(|a, b| b.bytes().cmp(&a.bytes()).then_with(|| a.path.cmp(&b.path)));
        v.truncate(n);
        v
    }

    /// Protected entries that will be preserved within measured items and when expanding folders: (entries, bytes).
    pub fn protected_total(&self) -> (u64, u64) {
        let mut n = self.omitted_protected.0;
        let mut b = self.omitted_protected.1;
        for i in &self.items {
            n += i.estimate.protected;
            b += i.estimate.protected_bytes;
        }
        (n, b)
    }

    /// Bytes in multiply linked files (which will not free space), and their count.
    pub fn shared_total(&self) -> (u64, u64) {
        self.items.iter().fold((0, 0), |(n, b), i| {
            (n + i.estimate.shared_files, b + i.estimate.shared_bytes)
        })
    }
}

/// Approval options.
#[derive(Clone, Debug, Default)]
pub struct ApproveOptions {
    /// Approximate Recycle Bin size, if emptying it was requested (for example, the size of its folder in the scanned tree).
    pub bin_bytes_hint: u64,
    /// Optional live approval counter, shared with the UI.
    pub progress: Option<Arc<AtomicU64>>,
}

/// The `cleaner`: rules, roots, and protections, ready to approve and execute.
pub struct Cleaner {
    pub(crate) rules: &'static Rules,
    pub(crate) roots: Roots,
    pub(crate) protection: Arc<Protection>,
    pub(crate) threads: usize,
    /// Scope of "empty Recycle Bin": the user's ENTIRE bin by default; it can be restricted (tests limit it to
    /// owned items so unrelated items are never touched).
    pub(crate) bin_filter: Option<BinFilter>,
    /// `%LOCALAPPDATA%\Tesela`: registro local e historial.
    pub(crate) data_dir: Option<PathBuf>,
    /// Shell implementation for sending items to the Recycle Bin (real in production, simulated in tests).
    pub(crate) shell: Arc<dyn crate::exec::RecycleShell>,
    /// Whether the process is elevated (production checks the token; tests inject the state).
    pub(crate) elevation: Arc<dyn crate::exec::Elevation>,
}

/// Filter for the "empty Recycle Bin" scope.
pub type BinFilter = Arc<dyn Fn(&BinItem) -> bool + Send + Sync>;

/// Default elevation check: query the real process token (and report "not elevated" in tests; see
/// `exec::NotElevated`).
fn default_elevation() -> Arc<dyn crate::exec::Elevation> {
    #[cfg(test)]
    {
        Arc::new(crate::exec::NotElevated)
    }
    #[cfg(not(test))]
    {
        Arc::new(crate::exec::RealElevation)
    }
}

/// Default number of deletion-pool threads: `min(8, 2 × cores)` (deletion is mostly file-system I/O).
pub fn default_threads() -> usize {
    let cores = std::thread::available_parallelism().map_or(2, std::num::NonZero::get);
    (2 * cores).clamp(2, 8)
}

/// Safety margin against the Recycle Bin's maximum size: above 90%, an item is considered not to fit. This is a
/// batching HEURISTIC, not a guarantee: the shell independently decides whether an item fits, on its own schedule and
/// by its own criteria. If it does not fit, the shell deletes it WITHOUT warning; Tesela detects this afterward
/// (`DeletedUnexpectedly`) and stops sending items.
const BIN_MARGIN_PERCENT: u128 = 90;

/// An item that alone exceeds 10% of capacity is sent in its own operation.
const BIN_BIG_PERCENT: u128 = 10;

/// Per-operation limit: 90% of `max`, calculated once without overflow (GM-m6: `fit_decide` and `take_op` use
/// EXACTLY this calculation; previously one used `bytes * 100 > max * 90` and the other `max / 10 * 9`).
pub(crate) fn bin_cap(max: u64) -> u64 {
    ((u128::from(max) * BIN_MARGIN_PERCENT) / 100) as u64
}

/// "Large" item threshold (10% of `max`), using the same calculation as [`bin_cap`].
pub(crate) fn bin_big(max: u64) -> u64 {
    ((u128::from(max) * BIN_BIG_PERCENT) / 100) as u64
}

/// The Recycle Bin does not support paths of 260 characters or more.
const MAX_BIN_PATH: usize = 258;

/// Path length in UTF-16 code units (what Windows counts for `MAX_PATH`), not WTF-8 bytes (GM-M3: names with
/// non-BMP or accented characters have different byte and code-unit lengths).
pub(crate) fn path_units(p: &Path) -> usize {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().count()
}

impl Cleaner {
    pub fn new(roots: Roots) -> Cleaner {
        // Protection (including category-root exemptions) is built ONCE here. A folder created later (for example,
        // WER creates `ReportQueue` after the first crash while Tesela is open) appears in Recommendations, which reads
        // roots dynamically, but approval rejects it as protected until restart. This fails closed (nothing extra is
        // touched); the displayed reason is not the actual cause. Accepted tradeoff: rebuilding it on each approval
        // would require a few opens per category and would not improve safety.
        let protection = Arc::new(roots.protection());
        let data_dir = roots
            .browser
            .local_appdata
            .as_ref()
            .map(|l| l.join("Tesela"));
        Cleaner {
            rules: rules(),
            roots,
            protection,
            threads: default_threads(),
            bin_filter: None,
            data_dir,
            shell: Arc::new(crate::exec::RealShell),
            elevation: default_elevation(),
        }
    }

    /// Whether the process is elevated. Real cleanup is not performed in that case ([`crate::ExecError::Elevated`]).
    pub fn is_elevated(&self) -> bool {
        self.elevation.is_elevated()
    }

    /// TEST ONLY: inject the elevation state.
    #[cfg(test)]
    pub(crate) fn with_elevation(mut self, e: Arc<dyn crate::exec::Elevation>) -> Cleaner {
        self.elevation = e;
        self
    }

    /// TEST ONLY: use another shell for Recycle Bin operations (batching and safety aborts).
    #[cfg(test)]
    pub(crate) fn with_shell(mut self, shell: Arc<dyn crate::exec::RecycleShell>) -> Cleaner {
        self.shell = shell;
        self
    }

    /// Changes the log and history folder (tests use their own).
    pub fn with_data_dir(mut self, dir: PathBuf) -> Cleaner {
        self.data_dir = Some(dir);
        self
    }

    /// Restricts the "empty Recycle Bin" scope to items that match the filter.
    pub fn with_bin_filter(mut self, f: BinFilter) -> Cleaner {
        self.bin_filter = Some(f);
        self
    }

    pub fn with_threads(mut self, n: usize) -> Cleaner {
        self.threads = n.clamp(1, 32);
        self
    }

    pub fn roots(&self) -> &Roots {
        &self.roots
    }

    pub fn protection(&self) -> &Arc<Protection> {
        &self.protection
    }

    pub fn threads(&self) -> usize {
        self.threads
    }

    pub(crate) fn env(
        &self,
        mode: Mode,
        min_age: Duration,
        cancel: &Arc<AtomicBool>,
        progress: &Arc<AtomicU64>,
        sink: Option<Arc<engine::EventSink>>,
    ) -> Arc<Env> {
        Arc::new(Env {
            protection: self.protection.clone(),
            mode,
            min_age_ft: engine::duration_to_ft(min_age),
            now_ft: engine::now_filetime(),
            cancel: cancel.clone(),
            sink,
            progress: progress.clone(),
        })
    }

    /// Does `bytes` (the larger of allocated and logical size) for an item at `path` fit in its volume's Recycle Bin?
    /// This is calculated during APPROVAL and recalculated during EXECUTION.
    pub fn fit(&self, path: &Path, bytes: u64) -> Fit {
        self.shell.fit(path, bytes)
    }

    /// Like [`Cleaner::fit`], with the longest path length among the item's contents (from its measurement).
    pub(crate) fn fit_deep(&self, path: &Path, bytes: u64, deepest: usize) -> Fit {
        self.shell.fit_deep(path, bytes, deepest)
    }

    /// Approves the basket: captures identities, revalidates protections, measures items, and checks Recycle Bin fit.
    pub fn approve(
        &self,
        basket: &Basket,
        opts: &ApproveOptions,
        cancel: &Arc<AtomicBool>,
    ) -> Plan {
        let mut cands: Vec<Candidate> = Vec::new();
        let mut plan = Plan::default();
        for src in basket.sources() {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            match src {
                Source::Category(id) => self.candidates_of_category(*id, &mut cands, &mut plan),
                Source::Path(p) => self.candidate_entry(p, &mut cands, &mut plan),
                Source::ContentsOf(p) => self.candidates_of_contents(p, &mut cands, &mut plan),
                Source::EmptyBin => {
                    if let Ok((mut items, unlisted)) = crate::recycle::list_bin_counted() {
                        plan.bin_unlisted = unlisted;
                        if let Some(f) = &self.bin_filter {
                            items.retain(|i| f(i));
                        }
                        // Size must match the ENUMERATED LIST: with a filter, sum the listed items; without one,
                        // use the shell's total for the entire Recycle Bin (or the caller's value if that query fails).
                        let listed: u64 = items.iter().map(|i| i.size).sum();
                        // m1: unlisted items are included in the shell total, which could overpromise how much will be emptied.
                        // Use the listed sum instead (it may be low because the shell gives no folder sizes, but never overpromises).
                        let hint = if self.bin_filter.is_some() || unlisted > 0 {
                            listed
                        } else {
                            crate::recycle::bin_totals()
                                .map(|t| t.1)
                                .ok()
                                .filter(|b| *b > 0)
                                .unwrap_or(if opts.bin_bytes_hint > 0 {
                                    opts.bin_bytes_hint
                                } else {
                                    listed
                                })
                        };
                        plan.bin = Some(BinPlan {
                            items,
                            bytes_hint: hint,
                        });
                    }
                }
            }
        }
        // Measure with the same traversal used for deletion (shared opens; nothing is modified).
        let progress = opts
            .progress
            .clone()
            .unwrap_or_else(|| Arc::new(AtomicU64::new(0)));
        let units: Vec<Arc<Unit>> = cands
            .iter()
            .enumerate()
            .map(|(i, c)| {
                Unit::new(
                    i,
                    c.spec.clone(),
                    self.env(Mode::Measure, c.min_age, cancel, &progress, None),
                )
            })
            .collect();
        engine::run(&units, self.threads);
        // Verified diagnostic roots: descendants are never permanently deleted, even if manually added as "Your file".
        let diag_roots: Vec<PathKey> = [CategoryId::WerReports, CategoryId::CrashDumps]
            .into_iter()
            .flat_map(|id| self.roots.category_roots(id))
            .map(|p| PathKey::new(&p))
            .collect();
        for (c, u) in cands.into_iter().zip(units) {
            let est = u.stats.snapshot();
            let id = plan.items.len();
            let dest = c.destination;
            // A category sent to the Recycle Bin item by item (error reports, dumps) offers a child only if NONE of its
            // contents are newer than the minimum age: a report folder containing a file from today stays intact.
            if c.category.is_some()
                && dest == Destination::Recycle
                && !c.min_age.is_zero()
                && est.young > 0
            {
                plan.rejected.push(Rejected {
                    path: c.canon,
                    reason: RejectReason::Young,
                });
                continue;
            }
            // m-4: sending items to the Recycle Bin requires a COMPLETE measurement (see `StatsSnapshot::is_complete`).
            if dest == Destination::Recycle && !est.is_complete() {
                plan.rejected.push(Rejected {
                    path: c.canon,
                    reason: RejectReason::Incomplete(
                        est.in_use + est.denied + est.other_errors + est.changed + est.gone,
                    ),
                });
                continue;
            }
            let size = est.recycle_charge();
            let fit = if dest == Destination::Recycle {
                self.shell.fit_deep(&c.canon, size, est.max_path as usize)
            } else {
                Fit::Fits
            };
            let canon_key = PathKey::new(&c.canon);
            let diagnostic_path = diag_roots.iter().any(|r| canon_key.is_same_or_under(r));
            plan.items.push(PlanItem {
                id,
                category: c.category,
                path: c.canon,
                risk: c.risk,
                destination: dest,
                fit,
                spec: c.spec,
                min_age: c.min_age,
                estimate: est,
                cloud_synced: c.cloud || est.cloud > 0,
                hard_linked: c.links > 1,
                volume: c.volume,
                diagnostic_path,
            });
        }
        if plan.mail_totals() != ((0, 0), (0, 0)) {
            plan.outlook_open = crate::process::outlook_open();
        }
        plan
    }

    fn candidates_of_category(&self, id: CategoryId, out: &mut Vec<Candidate>, plan: &mut Plan) {
        let rule = self.rules.get(id);
        if rule.action != Action::Contents {
            return;
        }
        let Some(dest) = rule.destination else { return };
        for check in self.roots.category_root_checks(id) {
            let (root, chain_identity) = match check.result {
                Ok(v) => (v.path, v.identity),
                // A missing root (for example, `ReportQueue` not created yet) is simply empty: no rejection or warning.
                // This applies only to diagnostic categories, whose folders Windows creates when needed.
                Err(RootRejection::Os(2 | 3)) if id.is_diagnostic() => continue,
                Err(RootRejection::Os(c)) => {
                    plan.rejected.push(Rejected {
                        path: check.raw,
                        reason: reason_of(c),
                    });
                    continue;
                }
                // B1 layer 1: the path chain contains a link (or changed); do not offer this category for this root.
                Err(RootRejection::ReparseInChain { .. } | RootRejection::NotCanonical) => {
                    plan.rejected.push(Rejected {
                        path: check.raw,
                        reason: RejectReason::ReparseAncestor,
                    });
                    continue;
                }
                // %TEMP% redirected outside %LOCALAPPDATA% has its own reason (consistent with the Recommendations card),
                // rather than "a link in its path".
                Err(RootRejection::OutsideLocalAppData) => {
                    plan.rejected.push(Rejected {
                        path: check.raw,
                        reason: RejectReason::OutsideLocalAppData,
                    });
                    continue;
                }
            };
            #[cfg(test)]
            crate::engine::test_hooks::before_open(&root);
            let h = match sys::open_dir_for_listing(&root) {
                Ok(h) => h,
                Err(c) => {
                    plan.rejected.push(Rejected {
                        path: root,
                        reason: reason_of(c),
                    });
                    continue;
                }
            };
            let Some((m, canon, key)) = self.verify(&h, &root, plan) else {
                continue;
            };
            // H1: the object opened for listing is EXACTLY what the chain traversal saw (same identity and resolved path).
            // If an ancestor was replaced by a link between these steps, reject it: otherwise we would capture the wrong object's identity.
            if !chain_matches(chain_identity, &root, m.identity, &key) {
                plan.rejected.push(Rejected {
                    path: canon,
                    reason: RejectReason::ReparseAncestor,
                });
                continue;
            }
            // A category root is valid only if it is a real directory and is NOT denied (ancestors of protected paths
            // and known folders are allowed: their contents are cleaned, not the folders themselves).
            if !m.is_dir
                || m.is_reparse()
                || self.protection.target_verdict(&key) == Verdict::Denied
            {
                plan.rejected.push(Rejected {
                    path: canon,
                    reason: if m.is_reparse() {
                        RejectReason::Reparse
                    } else {
                        RejectReason::Protected(Verdict::Denied)
                    },
                });
                continue;
            }
            // The Recycle Bin receives individual items only (`prepare_recycle` does not accept "root contents"): a category
            // targeting it expands into children, each with its own identity, category, risk, and minimum age (which `approve`
            // applies to each child after measuring it). The root is never sent.
            if dest == Destination::Recycle {
                let start = out.len();
                self.children_as_entries(
                    &h,
                    &canon,
                    m.identity.volume_serial,
                    rule.min_age,
                    out,
                    plan,
                );
                for c in &mut out[start..] {
                    c.category = Some(id);
                    c.risk = rule.risk;
                    c.min_age = rule.min_age;
                }
                continue;
            }
            out.push(Candidate {
                category: Some(id),
                risk: rule.risk,
                destination: dest,
                spec: UnitSpec::Contents {
                    path: canon.clone(),
                    approved: m.identity,
                },
                min_age: rule.min_age,
                volume: volume::volume_root(&canon),
                canon,
                links: 1,
                cloud: false,
            });
        }
    }

    /// Opens `h` and reads its identity and resolved path. Records the rejection and returns `None` on failure.
    fn verify(
        &self,
        h: &sys::Handle,
        path: &Path,
        plan: &mut Plan,
    ) -> Option<(sys::Meta, PathBuf, PathKey)> {
        let m = match sys::meta(h) {
            Ok(m) => m,
            Err(c) => {
                plan.rejected.push(Rejected {
                    path: path.to_path_buf(),
                    reason: match c {
                        1 | 50 | 87 => RejectReason::NoIdentity,
                        other => reason_of(other),
                    },
                });
                return None;
            }
        };
        let canon = match sys::final_path(h) {
            Ok(p) => p,
            Err(c) => {
                plan.rejected.push(Rejected {
                    path: path.to_path_buf(),
                    reason: reason_of(c),
                });
                return None;
            }
        };
        if !volume::is_local_path(&canon) {
            plan.rejected.push(Rejected {
                path: canon,
                reason: RejectReason::NotLocal,
            });
            return None;
        }
        let key = PathKey::new(&canon);
        Some((m, canon, key))
    }

    /// GM-B2: the ANCESTOR chain of `path` as written (component by component from the volume root, using relative opens
    /// and no reparse points except cloud folders; see [`tolerated_ancestor`]), then the item opened RELATIVE to that verified
    /// parent with the same identity as the handle already opened by path. `GetFinalPathNameByHandleW` silently resolves
    /// ancestor junctions and would replace the requested path with the resolved one, causing the plan to act on a different
    /// folder from the one the user selected. Comparing `PathKey` is insufficient (in CI, `%TEMP%` uses 8.3 names and the two
    /// forms of the same path differ), so traverse the actual chain; the file system resolves 8.3 names during open.
    fn check_ancestors(path: &Path, m: &sys::Meta) -> Result<(), RejectReason> {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            // A drive root has no ancestors (and protection already denies it).
            return Ok(());
        };
        let ph = match sys::open_chain_dir(parent, tolerated_ancestor) {
            Ok(ph) => ph,
            Err(sys::ChainError::Reparse { .. } | sys::ChainError::BadPath) => {
                return Err(RejectReason::ReparseAncestor);
            }
            Err(sys::ChainError::Os(c)) => return Err(reason_of(c)),
        };
        let wide: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(name).collect();
        let expect = if m.is_dir {
            sys::Expect::Dir
        } else {
            sys::Expect::File
        };
        match sys::open_child_for_query(&ph, &wide, expect).and_then(|ch| sys::meta(&ch)) {
            Ok(cm) if cm.identity == m.identity => Ok(()),
            // Different object: the path-opened object is not the one beneath the verified parent (an ancestor changed in between).
            Ok(_) => Err(RejectReason::ReparseAncestor),
            Err(c) => Err(reason_of(c)),
        }
    }

    fn candidates_of_contents(&self, folder: &Path, out: &mut Vec<Candidate>, plan: &mut Plan) {
        let h = match sys::open_dir_for_listing(folder) {
            Ok(h) => h,
            Err(c) => {
                plan.rejected.push(Rejected {
                    path: folder.to_path_buf(),
                    reason: reason_of(c),
                });
                return;
            }
        };
        let Some((m, canon, key)) = self.verify(&h, folder, plan) else {
            return;
        };
        // GM-B2: ancestors of the requested folder cannot be links (the resolved path would replace the requested path).
        if let Err(reason) = Self::check_ancestors(folder, &m) {
            plan.rejected.push(Rejected {
                path: canon,
                reason,
            });
            return;
        }
        let verdict = self.protection.target_verdict(&key);
        if !m.is_dir || m.is_reparse() || verdict != Verdict::ContentsOnly {
            plan.rejected.push(Rejected {
                path: canon,
                reason: if m.is_reparse() {
                    RejectReason::Reparse
                } else {
                    RejectReason::Protected(verdict)
                },
            });
            return;
        }
        self.children_as_entries(
            &h,
            &canon,
            m.identity.volume_serial,
            Duration::ZERO,
            out,
            plan,
        );
    }

    /// Children of the opened folder `h` (`canon`) as individual items targeting the Recycle Bin: each child is opened
    /// RELATIVE to `h` and checked against the listing's `FileId` (GM-B2). Links are rejected. Used by "Contents of X"
    /// (Downloads, etc.) and "Review" risk categories sent to the Recycle Bin (error reports and dumps). When `min_age` > 0,
    /// a child whose newest timestamp (creation, write, or change) is newer is left in place; `approve` checks contents inside
    /// a folder when measuring it.
    fn children_as_entries(
        &self,
        h: &sys::Handle,
        canon: &Path,
        volume_serial: u64,
        min_age: Duration,
        out: &mut Vec<Candidate>,
        plan: &mut Plan,
    ) {
        let entries = match sys::list_dir(h) {
            Ok(e) => e,
            Err(c) => {
                plan.rejected.push(Rejected {
                    path: canon.to_path_buf(),
                    reason: reason_of(c),
                });
                return;
            }
        };
        let (min_ft, now_ft) = (engine::duration_to_ft(min_age), engine::now_filetime());
        for e in entries {
            let child = canon.join(OsString::from_wide(&e.name));
            if e.is_reparse() && (e.is_dir() || !sys::is_cloud_tag(e.reparse_tag)) {
                plan.rejected.push(Rejected {
                    path: child,
                    reason: RejectReason::Reparse,
                });
                continue;
            }
            if min_ft > 0 && now_ft - e.newest_filetime < min_ft {
                plan.rejected.push(Rejected {
                    path: child,
                    reason: RejectReason::Young,
                });
                continue;
            }
            self.candidate_child(h, volume_serial, &child, &e, min_age, out, plan);
        }
    }

    /// GM-B2: a child of an already-open folder (`dir`), opened RELATIVE to that handle and checked against the listing's
    /// `FileId` and volume. Previously it was reopened by path after listing; the child (or an ancestor) could change in between,
    /// causing approval to capture the wrong object's identity.
    #[allow(clippy::too_many_arguments)]
    fn candidate_child(
        &self,
        dir: &sys::Handle,
        volume_serial: u64,
        child: &Path,
        e: &sys::DirEntry,
        min_age: Duration,
        out: &mut Vec<Candidate>,
        plan: &mut Plan,
    ) {
        #[cfg(test)]
        crate::engine::test_hooks::before_open(child);
        let expect = if e.is_dir() {
            sys::Expect::Dir
        } else {
            sys::Expect::File
        };
        let h = match sys::open_child_for_query(dir, &e.name, expect) {
            Ok(h) => h,
            Err(c) => {
                plan.rejected.push(Rejected {
                    path: child.to_path_buf(),
                    reason: reason_of(c),
                });
                return;
            }
        };
        let meta = sys::meta(&h);
        let same = meta.as_ref().is_ok_and(|m| {
            m.identity.file_id == e.file_id && m.identity.volume_serial == volume_serial
        });
        if !same {
            // It changed between listing and opening: do not approve an object that was not listed.
            plan.rejected.push(Rejected {
                path: child.to_path_buf(),
                reason: RejectReason::Other(0),
            });
            return;
        }
        // M-1: minimum age is also checked against the HANDLE timestamp (the listing may predate a file another process just
        // wrote). A `min_age` of zero has no effect.
        let min_ft = engine::duration_to_ft(min_age);
        if min_ft > 0
            && meta
                .as_ref()
                .is_ok_and(|m| engine::now_filetime() - m.newest_filetime < min_ft)
        {
            plan.rejected.push(Rejected {
                path: child.to_path_buf(),
                reason: RejectReason::Young,
            });
            return;
        }
        self.candidate_from(&h, child, false, out, plan);
    }

    /// A user-selected file or folder ("Your file"): captures its identity and, if the folder contains protected items,
    /// expands it into its allowed parts.
    fn candidate_entry(&self, path: &Path, out: &mut Vec<Candidate>, plan: &mut Plan) {
        let h = match sys::open_for_query(path) {
            Ok(h) => h,
            Err(c) => {
                plan.rejected.push(Rejected {
                    path: path.to_path_buf(),
                    reason: reason_of(c),
                });
                return;
            }
        };
        self.candidate_from(&h, path, true, out, plan);
    }

    /// The remaining work of [`Cleaner::candidate_entry`] on an already-open handle. `check_chain` checks the ancestors of
    /// `path` (GM-B2); a child opened relative to a verified directory does not need this check.
    fn candidate_from(
        &self,
        h: &sys::Handle,
        path: &Path,
        check_chain: bool,
        out: &mut Vec<Candidate>,
        plan: &mut Plan,
    ) {
        let Some((m, canon, key)) = self.verify(h, path, plan) else {
            return;
        };
        if check_chain && let Err(reason) = Self::check_ancestors(path, &m) {
            plan.rejected.push(Rejected {
                path: canon,
                reason,
            });
            return;
        }
        let cloud_file = !m.is_dir && m.is_reparse() && sys::is_cloud_tag(m.reparse_tag);
        if m.is_reparse() && !cloud_file {
            plan.rejected.push(Rejected {
                path: canon,
                reason: RejectReason::Reparse,
            });
            return;
        }
        let verdict = self.protection.target_verdict(&key);
        if verdict != Verdict::Allowed {
            plan.rejected.push(Rejected {
                path: canon,
                reason: RejectReason::Protected(verdict),
            });
            return;
        }
        if m.is_dir && self.protection.contains_protected(&key) {
            // Listing requires another handle (the query handle cannot enumerate); verify that it is the same object.
            match sys::open_dir_for_listing(&canon) {
                Ok(lh) if sys::meta(&lh).is_ok_and(|x| x.identity == m.identity) => {
                    self.expand_dir(&lh, &canon, &key, m.identity.volume_serial, out, plan);
                }
                Ok(_) => plan.rejected.push(Rejected {
                    path: canon,
                    reason: RejectReason::Other(0),
                }),
                Err(c) => plan.rejected.push(Rejected {
                    path: canon,
                    reason: reason_of(c),
                }),
            }
            return;
        }
        out.push(Candidate {
            category: None,
            risk: Risk::Review,
            destination: Destination::Recycle,
            spec: UnitSpec::Entry {
                path: canon.clone(),
                approved: m.identity,
                approved_tag: if cloud_file { m.reparse_tag } else { 0 },
                approved_links: m.links,
                is_dir: m.is_dir,
            },
            min_age: Duration::ZERO,
            volume: volume::volume_root(&canon),
            canon,
            links: m.links,
            cloud: cloud_file,
        });
    }

    /// A folder containing protected items: allowed children become items and protected ones are skipped. Children are opened
    /// RELATIVE to `dir` and checked against the listing identity (GM-B2).
    fn expand_dir(
        &self,
        dir: &sys::Handle,
        dir_path: &Path,
        dir_key: &PathKey,
        volume_serial: u64,
        out: &mut Vec<Candidate>,
        plan: &mut Plan,
    ) {
        let entries = match sys::list_dir(dir) {
            Ok(e) => e,
            Err(c) => {
                plan.rejected.push(Rejected {
                    path: dir_path.to_path_buf(),
                    reason: reason_of(c),
                });
                return;
            }
        };
        for e in entries {
            let ckey = dir_key.join(&crate::paths::wide_to_string(&e.name));
            let child = dir_path.join(OsString::from_wide(&e.name));
            match self.protection.child_action(&ckey) {
                ChildAction::Skip => {
                    plan.omitted_protected.0 += 1;
                    plan.omitted_protected.1 += e.alloc;
                }
                ChildAction::Descend if e.is_dir() && !e.is_reparse() => {
                    // A known or untouchable folder INSIDE is preserved (the folder itself, not its contents) and counts as protected.
                    if self.protection.is_retained_root(&ckey) {
                        plan.omitted_protected.0 += 1;
                    }
                    // Traverse through a relative handle and verify it is the same object as the listed entry.
                    match sys::open_child_dir_for_listing(dir, &e.name) {
                        Ok(ch) => {
                            let same = sys::meta(&ch).is_ok_and(|m| {
                                m.identity.file_id == e.file_id
                                    && m.identity.volume_serial == volume_serial
                                    && !m.is_reparse()
                            });
                            if same {
                                self.expand_dir(&ch, &child, &ckey, volume_serial, out, plan);
                            } else {
                                plan.rejected.push(Rejected {
                                    path: child,
                                    reason: RejectReason::Reparse,
                                });
                            }
                        }
                        Err(c) => plan.rejected.push(Rejected {
                            path: child,
                            reason: reason_of(c),
                        }),
                    }
                }
                ChildAction::Descend => {
                    plan.omitted_protected.0 += 1;
                    plan.omitted_protected.1 += e.alloc;
                }
                ChildAction::Delete => {
                    if e.is_reparse() && (e.is_dir() || !sys::is_cloud_tag(e.reparse_tag)) {
                        plan.rejected.push(Rejected {
                            path: child,
                            reason: RejectReason::Reparse,
                        });
                    } else {
                        self.candidate_child(
                            dir,
                            volume_serial,
                            &child,
                            &e,
                            Duration::ZERO,
                            out,
                            plan,
                        );
                    }
                }
            }
        }
    }
}

/// M1: should this reparse-point ancestor be tolerated when approving a basket item? Only cloud-sync folders
/// (OneDrive and similar, with an `IO_REPARSE_TAG_CLOUD*` tag) qualify: they are placeholders that do not redirect name
/// resolution. Rejecting them prevented every standalone file under a cloud folder from reaching `cloud_file` or
/// `confirm-cloud` (a regression in the first GM-B2 version). Junctions, symlinks, mount points, WSL links, and all other
/// tags remain rejected. The identity of each path component is still verified.
pub(crate) fn tolerated_ancestor(m: &sys::Meta) -> bool {
    m.is_dir && sys::is_cloud_tag(m.reparse_tag)
}

struct Candidate {
    category: Option<CategoryId>,
    risk: Risk,
    destination: Destination,
    spec: UnitSpec,
    min_age: Duration,
    canon: PathBuf,
    volume: Option<PathBuf>,
    links: u32,
    cloud: bool,
}

/// Is the object opened during approval (`opened`, with resolved path `key`) the same one seen during chain traversal
/// (`chain`, with canonical path `root`)? Both identity (volume + FileId) AND resolved path must match.
pub(crate) fn chain_matches(
    chain: sys::FileIdentity,
    root: &Path,
    opened: sys::FileIdentity,
    key: &PathKey,
) -> bool {
    chain == opened && PathKey::new(root) == *key
}

fn reason_of(code: u32) -> RejectReason {
    match code {
        2 | 3 => RejectReason::Missing,
        5 => RejectReason::AccessDenied,
        other => RejectReason::Other(other),
    }
}

/// All inputs used to decide whether an item fits in the Recycle Bin, already read (so the decision is a pure function and
/// can be tested without touching the registry or volume).
#[derive(Clone, Copy, Debug)]
pub(crate) struct BinFacts {
    /// Item path length.
    pub path_len: usize,
    /// Whether the item's volume was identified.
    pub volume_known: bool,
    /// Directiva `NoRecycleFiles`.
    pub policy_no_recycle: bool,
    pub limit: BinLimit,
}

/// The decision (see [`fit_for`]): any uncertainty means `Unfit`.
pub(crate) fn fit_decide(bytes: u64, f: &BinFacts) -> Fit {
    // The Recycle Bin does not support paths of 260 characters or more.
    if f.path_len > MAX_BIN_PATH {
        return Fit::Unfit(UnfitReason::PathTooLong);
    }
    if !f.volume_known {
        return Fit::Unfit(UnfitReason::Unknown);
    }
    if f.policy_no_recycle {
        return Fit::Unfit(UnfitReason::Policy);
    }
    let too_big = |max: u64| bytes > bin_cap(max);
    match f.limit {
        BinLimit::NoBin => Fit::Unfit(UnfitReason::NoBin),
        BinLimit::NukeOnDelete => Fit::Unfit(UnfitReason::NukeOnDelete),
        BinLimit::Indeterminate => Fit::Unfit(UnfitReason::Unknown),
        BinLimit::Max(max) if too_big(max) => Fit::Unfit(UnfitReason::TooBig),
        BinLimit::Max(_) => Fit::Fits,
        // Without `MaxCapacity`, there is no capacity to guarantee: stop assuming 5% of the volume. Planning and execution
        // must agree (`exec::take_op` also fails closed when capacity is unknown).
        BinLimit::Unknown => Fit::Unfit(UnfitReason::CapacityUnknown),
    }
}

/// Does an item of `bytes` at `path` fit in its volume's Recycle Bin? Any error while checking (volume root, limit, volume
/// size, or policy) returns `Unfit`: this never fails open.
pub(crate) fn fit_for(path: &Path, bytes: u64) -> Fit {
    fit_for_deep(path, bytes, 0)
}

/// Like [`fit_for`], with the longest path length (in UTF-16 code units) among the item's contents (`deepest`, from the
/// measurement; 0 if unknown): the shell does not support paths of 260 characters or more, so such a descendant makes the
/// folder ineligible for the Recycle Bin (GM-M3).
pub(crate) fn fit_for_deep(path: &Path, bytes: u64, deepest: usize) -> Fit {
    // `GetVolumePathNameW` resolves unusual or relative paths against the current directory: query only absolute local-drive
    // paths (those already checked by `is_local_path`).
    let root = volume::is_local_path(path)
        .then(|| volume::volume_root(path))
        .flatten();
    let limit = match &root {
        Some(r) => volume::bin_limit(r),
        None => BinLimit::Indeterminate,
    };
    fit_decide(
        bytes,
        &BinFacts {
            path_len: path_units(path).max(deepest),
            volume_known: root.is_some(),
            policy_no_recycle: volume::recycle_policy_disabled(),
            limit,
        },
    )
}

#[cfg(test)]
mod fit_tests {
    use super::*;

    fn facts(limit: BinLimit) -> BinFacts {
        BinFacts {
            path_len: 40,
            volume_known: true,
            policy_no_recycle: false,
            limit,
        }
    }

    #[test]
    fn con_un_limite_configurado_se_compara_con_el_margen() {
        let max = 100_000_000_000u64;
        let f = facts(BinLimit::Max(max));
        assert_eq!(fit_decide(10_000_000_000, &f), Fit::Fits);
        assert_eq!(fit_decide(89_000_000_000, &f), Fit::Fits);
        assert_eq!(
            fit_decide(91_000_000_000, &f),
            Fit::Unfit(UnfitReason::TooBig)
        );
    }

    #[test]
    fn ante_cualquier_duda_no_cabe() {
        // Volumen desconocido o límite indeterminado: Unfit(Unknown), nunca Fits.
        let mut f = facts(BinLimit::Max(1 << 40));
        f.volume_known = false;
        assert_eq!(fit_decide(1, &f), Fit::Unfit(UnfitReason::Unknown));
        assert_eq!(
            fit_decide(1, &facts(BinLimit::Indeterminate)),
            Fit::Unfit(UnfitReason::Unknown)
        );
        // Sin Papelera o con la Papelera desactivada.
        assert_eq!(
            fit_decide(1, &facts(BinLimit::NoBin)),
            Fit::Unfit(UnfitReason::NoBin)
        );
        assert_eq!(
            fit_decide(1, &facts(BinLimit::NukeOnDelete)),
            Fit::Unfit(UnfitReason::NukeOnDelete)
        );
    }

    #[test]
    fn la_directiva_no_recycle_files_lo_hace_no_caber_aunque_el_limite_sea_enorme() {
        let mut f = facts(BinLimit::Max(1 << 50));
        assert_eq!(fit_decide(1, &f), Fit::Fits);
        f.policy_no_recycle = true;
        assert_eq!(fit_decide(1, &f), Fit::Unfit(UnfitReason::Policy));
    }

    /// B2: sin `MaxCapacity` en el registro no hay capacidad que garantizar: no cabe, con su propio motivo, sea cual sea
    /// el tamaño (ya no se asume el 5 % del volumen).
    #[test]
    fn sin_capacidad_configurada_no_cabe_nada_con_su_propio_motivo() {
        let f = facts(BinLimit::Unknown);
        for bytes in [0, 1, 1_000, 44_000_000_000, u64::MAX / 2] {
            assert_eq!(
                fit_decide(bytes, &f),
                Fit::Unfit(UnfitReason::CapacityUnknown),
                "{bytes}"
            );
        }
        // Con límite configurado sí cabe lo pequeño; el motivo nuevo es distinto del de "no se pudo comprobar".
        assert_eq!(
            fit_decide(1_000, &facts(BinLimit::Max(100_000_000_000))),
            Fit::Fits
        );
        assert_ne!(UnfitReason::CapacityUnknown, UnfitReason::Unknown);
    }

    #[test]
    fn las_rutas_largas_no_caben_se_sepa_lo_que_se_sepa() {
        let mut f = facts(BinLimit::Max(1 << 50));
        f.path_len = 259;
        assert_eq!(fit_decide(1, &f), Fit::Unfit(UnfitReason::PathTooLong));
        f.path_len = 258;
        assert_eq!(fit_decide(1, &f), Fit::Fits);
        // Con cualquier otra duda a la vez, gana la ruta larga (el motivo más concreto).
        f.path_len = 300;
        f.volume_known = false;
        assert_eq!(fit_decide(1, &f), Fit::Unfit(UnfitReason::PathTooLong));
    }

    #[test]
    fn el_mismo_calculo_sobre_el_volumen_real_no_falla_abierto() {
        // El temporal de la prueba existe: la respuesta es una de las dos, y un tamaño absurdo nunca "cabe".
        let tmp = dunce::canonicalize(std::env::temp_dir()).unwrap();
        assert!(matches!(fit_for(&tmp, 1), Fit::Fits | Fit::Unfit(_)));
        assert!(matches!(fit_for(&tmp, u64::MAX / 2), Fit::Unfit(_)));
        // Una ruta sin volumen conocido no cabe.
        assert!(matches!(
            fit_for(Path::new("ZZ:\\no\\existe"), 1),
            Fit::Unfit(_)
        ));
    }
}
