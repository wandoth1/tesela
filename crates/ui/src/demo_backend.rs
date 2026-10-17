//! SIMULATED cleanup backup for `tesela --demo`: basket, confirmation and summary work, but against
//! synthetic tree and WITHOUT touching the disk (it does not open, list, rename or delete anything: it only reads the `Tree` in memory).
//!
//! Imitates the observable of the real `cleaner`: protection by folders, "Content of...", minimum age of temporary ones,
//! the Recycle Bin limit (10% of the dummy unit), links to the cloud and the three counters.

use crate::demo::{self, DEMO_NOW, DEMO_SID, USER};
use crate::recs::eligible_by_age;
use crate::treepath::Locator;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tesela_cleaner::engine::{StatsSnapshot, UnitSpec};
use tesela_cleaner::exec::BinReport;
use tesela_cleaner::paths::PathKey;
use tesela_cleaner::plan::BinPlan;
use tesela_cleaner::protect::{ChildAction, Exempt, ProtectionRoots};
use tesela_cleaner::recycle::BinItem;
use tesela_cleaner::sys::FileIdentity;
use tesela_cleaner::{
    ApproveOptions, Backend, Basket, Browser, CategoryId, Destination, ExecError, ExecOptions, Fit,
    ItemReport, ItemStatus, Phase, Plan, PlanItem, Progress, Protection, Removed, Report, Risk,
    Source, UnfitReason, Verdict, rules,
};
use tesela_model::{Tree, View};

/// The simulated backup.
pub struct DemoBackend {
    tree: Arc<Tree>,
    root: String,
    prot: Arc<Protection>,
    browsers: Mutex<Vec<Browser>>,
    /// Categories "not available" (this is indicated by an interface test).
    unavailable: Mutex<Vec<(CategoryId, tesela_cleaner::roots::RootRejection)>>,
    /// How long the simulated run lasts (so you can see the progress). 0 in the tests.
    delay: Duration,
    cache_dirs: Vec<(CategoryId, PathBuf)>,
}

fn p(root: &str, rel: &str) -> PathBuf {
    PathBuf::from(format!("{root}{rel}"))
}

impl DemoBackend {
    pub fn new(delay: Duration) -> DemoBackend {
        let tree = Arc::new(demo::tree(demo::Variant::Complete));
        let root = demo::root_name();
        let me = format!("Users\\{USER}");
        let local = format!("{me}\\AppData\\Local");
        let prot = Protection::new(&ProtectionRoots {
            denied: vec![
                p(&root, "Windows"),
                p(&root, "Archivos de programa"),
                p(&root, "ProgramData"),
                p(&root, "Users\\Público"),
                p(&root, &format!("{local}\\Google\\Chrome\\User Data")),
                p(&root, &format!("{local}\\Microsoft\\Edge\\User Data")),
                p(&root, &format!("{local}\\Mozilla\\Firefox")),
            ],
            untouchable: vec![
                p(&root, "Users"),
                p(&root, &me),
                p(&root, &format!("{me}\\AppData")),
                p(&root, &local),
                p(&root, &format!("{me}\\AppData\\Roaming")),
            ],
            content_roots: ["Documentos", "Vídeos", "Imágenes", "Música", "Descargas"]
                .iter()
                .map(|n| p(&root, &format!("{me}\\{n}")))
                .collect(),
            exempt: vec![
                Exempt::new(p(&root, "Windows\\Temp"), Some(p(&root, "Windows"))),
                Exempt::new(
                    p(
                        &root,
                        &format!("{local}\\Google\\Chrome\\User Data\\Default\\Cache"),
                    ),
                    Some(p(&root, &format!("{local}\\Google\\Chrome\\User Data"))),
                ),
                Exempt::new(
                    p(
                        &root,
                        &format!("{local}\\Microsoft\\Edge\\User Data\\Default\\Cache"),
                    ),
                    Some(p(&root, &format!("{local}\\Microsoft\\Edge\\User Data"))),
                ),
                Exempt::new(
                    p(
                        &root,
                        &format!(
                            "{local}\\Mozilla\\Firefox\\Profiles\\demo.default-release\\cache2"
                        ),
                    ),
                    Some(p(&root, &format!("{local}\\Mozilla\\Firefox"))),
                ),
            ],
        });
        let cache_dirs = vec![
            (
                CategoryId::ChromeCache,
                p(
                    &root,
                    &format!("{local}\\Google\\Chrome\\User Data\\Default\\Cache"),
                ),
            ),
            (
                CategoryId::EdgeCache,
                p(
                    &root,
                    &format!("{local}\\Microsoft\\Edge\\User Data\\Default\\Cache"),
                ),
            ),
            (
                CategoryId::FirefoxCache,
                p(
                    &root,
                    &format!("{local}\\Mozilla\\Firefox\\Profiles\\demo.default-release\\cache2"),
                ),
            ),
        ];
        DemoBackend {
            tree,
            root,
            prot: Arc::new(prot),
            browsers: Mutex::new(vec![Browser::Chrome]),
            unavailable: Mutex::new(Vec::new()),
            delay,
            cache_dirs,
        }
    }

    /// Marks a category as "unavailable" (as if a root had been discarded by a link in its path):
    /// interface tests use it.
    pub fn set_unavailable(&self, id: CategoryId, why: tesela_cleaner::roots::RootRejection) {
        *self.unavailable.lock().unwrap_or_else(|e| e.into_inner()) = vec![(id, why)];
    }

    /// "Open" browsers (in the demo, Chrome by default; tests change this).
    pub fn set_browsers(&self, b: Vec<Browser>) {
        *self.browsers.lock().unwrap_or_else(|e| e.into_inner()) = b;
    }

    fn me(&self) -> String {
        format!("{}Users\\{USER}", self.root)
    }

    /// Does it fit in the dummy drive's Recycle Bin? The demo drive has a CONFIGURED maximum size (5%
    /// of the unit); as in the real `cleaner`, only what fits in that maximum, with a margin of 90%.
    fn fit(&self, bytes: u64) -> Fit {
        let limit = demo::drives()[0].total / 100 * 5;
        if (bytes as u128) * 100 > (limit as u128) * 90 {
            Fit::Unfit(UnfitReason::TooBig)
        } else {
            Fit::Fits
        }
    }

    fn identity(node: u32) -> FileIdentity {
        FileIdentity {
            volume_serial: 0xDE30,
            file_id: node as u128 + 1,
        }
    }

    fn entry_item(
        &self,
        path: &Path,
        loc: &mut Locator,
        view: &View,
        plan: &mut Plan,
        out: &mut Vec<PlanItem>,
    ) {
        let key = PathKey::new(path);
        let Some(node) = loc.locate(path) else {
            plan.rejected.push(tesela_cleaner::Rejected {
                path: path.to_path_buf(),
                reason: tesela_cleaner::RejectReason::Missing,
            });
            return;
        };
        let verdict = self.prot.target_verdict(&key);
        if verdict != Verdict::Allowed {
            plan.rejected.push(tesela_cleaner::Rejected {
                path: path.to_path_buf(),
                reason: tesela_cleaner::RejectReason::Protected(verdict),
            });
            return;
        }
        let tree = &self.tree;
        let n = tree.node(node);
        if n.is_dir() && self.prot.contains_protected(&key) {
            // Folder with something protected inside: your allowed children are the items; what is protected is omitted.
            for c in view.kids(node).iter() {
                let cpath = path.join(tree.name_lossy(c).as_ref());
                let ckey = PathKey::new(&cpath);
                match self.prot.child_action(&ckey) {
                    ChildAction::Skip => plan.omitted_protected.0 += 1,
                    ChildAction::Descend => {
                        if self.prot.is_retained_root(&ckey) {
                            plan.omitted_protected.0 += 1;
                        }
                        if tree.node(c).is_dir() {
                            self.entry_item(&cpath, loc, view, plan, out);
                        }
                    }
                    ChildAction::Delete => self.entry_item(&cpath, loc, view, plan, out),
                }
            }
            return;
        }
        let (bytes, logical, files, _) = eligible_by_age(view, node, u32::MAX);
        let fit = self.fit(bytes.max(logical));
        out.push(PlanItem {
            id: 0,
            category: None,
            path: path.to_path_buf(),
            risk: Risk::Review,
            destination: Destination::Recycle,
            fit,
            spec: UnitSpec::Entry {
                path: path.to_path_buf(),
                approved: DemoBackend::identity(node),
                approved_tag: 0,
                approved_links: 1,
                is_dir: n.is_dir(),
            },
            min_age: Duration::ZERO,
            estimate: StatsSnapshot {
                bytes,
                logical,
                files,
                dirs: u64::from(n.is_dir()),
                ..StatsSnapshot::default()
            },
            cloud_synced: n.flags & tesela_model::flags::CLOUD != 0,
            hard_linked: false,
            diagnostic_path: false,
            volume: Some(PathBuf::from(&self.root)),
        });
    }
}

impl Backend for DemoBackend {
    fn unavailable_roots(
        &self,
        id: CategoryId,
    ) -> Vec<(PathBuf, tesela_cleaner::roots::RootRejection)> {
        self.unavailable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(c, _)| *c == id)
            .map(|(_, why)| (p(&self.root, "Users\\ejemplo\\no-disponible"), why.clone()))
            .collect()
    }

    fn category_roots(&self, id: CategoryId) -> Vec<PathBuf> {
        if self
            .unavailable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|(c, _)| *c == id)
        {
            return Vec::new();
        }
        match id {
            CategoryId::UserTemp => vec![p(
                &self.root,
                &format!("Users\\{USER}\\AppData\\Local\\Temp"),
            )],
            CategoryId::WindowsTemp => vec![p(&self.root, "Windows\\Temp")],
            c if c.is_browser_cache() => self
                .cache_dirs
                .iter()
                .filter(|(i, _)| *i == c)
                .map(|(_, d)| d.clone())
                .collect(),
            _ => Vec::new(),
        }
    }

    fn downloads_dir(&self) -> Option<PathBuf> {
        Some(PathBuf::from(format!("{}\\Descargas", self.me())))
    }

    fn recycle_dir(&self, drive_root: &Path) -> Option<PathBuf> {
        Some(drive_root.join("$Recycle.Bin").join(DEMO_SID))
    }

    fn protection(&self) -> Arc<Protection> {
        self.prot.clone()
    }

    fn approve(&self, basket: &Basket, opts: &ApproveOptions, _cancel: &Arc<AtomicBool>) -> Plan {
        let view = View::new(&self.tree, None);
        let mut loc = Locator::new(&self.tree);
        let mut plan = Plan::default();
        let mut items: Vec<PlanItem> = Vec::new();
        for src in basket.sources() {
            match src {
                Source::Category(id) => {
                    let rule = rules().get(*id);
                    let Some(dest) = rule.destination else {
                        continue;
                    };
                    let cutoff = if rule.min_age.is_zero() {
                        u32::MAX
                    } else {
                        DEMO_NOW - rule.min_age.as_secs() as u32
                    };
                    for root in self.category_roots(*id) {
                        let Some(node) = loc.locate(&root) else {
                            continue;
                        };
                        let (bytes, logical, files, _) = eligible_by_age(&view, node, cutoff);
                        let (_, _, all_files, _) = eligible_by_age(&view, node, u32::MAX);
                        items.push(PlanItem {
                            id: 0,
                            category: Some(*id),
                            path: root.clone(),
                            risk: rule.risk,
                            destination: dest,
                            fit: Fit::Fits,
                            spec: UnitSpec::Contents {
                                path: root,
                                approved: DemoBackend::identity(node),
                            },
                            min_age: rule.min_age,
                            estimate: StatsSnapshot {
                                bytes,
                                logical,
                                files,
                                dirs: 0,
                                young: all_files - files,
                                ..StatsSnapshot::default()
                            },
                            cloud_synced: false,
                            hard_linked: false,
                            diagnostic_path: false,
                            volume: Some(PathBuf::from(&self.root)),
                        });
                    }
                }
                Source::Path(path) => self.entry_item(path, &mut loc, &view, &mut plan, &mut items),
                Source::ContentsOf(folder) => {
                    let verdict = self.prot.target_verdict(&PathKey::new(folder));
                    match (verdict, loc.locate(folder)) {
                        (Verdict::ContentsOnly, Some(node)) => {
                            for c in view.kids(node).iter() {
                                let cpath = folder.join(self.tree.name_lossy(c).as_ref());
                                self.entry_item(&cpath, &mut loc, &view, &mut plan, &mut items);
                            }
                        }
                        (v, _) => plan.rejected.push(tesela_cleaner::Rejected {
                            path: folder.clone(),
                            reason: tesela_cleaner::RejectReason::Protected(v),
                        }),
                    }
                }
                Source::EmptyBin => {
                    let drive = PathBuf::from(&self.root);
                    let bytes = self
                        .recycle_dir(&drive)
                        .and_then(|d| loc.locate(&d))
                        .map_or(opts.bin_bytes_hint, |n| view.alloc(n));
                    // A list of dummy items: the demo tree's Recycle Bin contents.
                    let n = loc
                        .locate(&self.recycle_dir(&drive).unwrap_or_default())
                        .map_or(0, |n| self.tree.node(n).files_below as usize);
                    let list: Vec<BinItem> = (0..n)
                        .map(|i| BinItem {
                            id: OsString::from(format!("demo-{i}")),
                            name: OsString::from(format!("elemento-{i}")),
                            original_parent: PathBuf::from(format!(
                                "{}Users\\{USER}\\Descargas",
                                self.root
                            )),
                            time_deleted: DEMO_NOW as i64 - 86_400 * (i as i64 % 60),
                            size: 0,
                            data_id: None,
                            meta_id: None,
                            no_identity: false,
                        })
                        .collect();
                    plan.bin = Some(BinPlan {
                        items: list,
                        bytes_hint: bytes,
                    });
                }
            }
        }
        for (i, mut it) in items.into_iter().enumerate() {
            it.id = i;
            plan.items.push(it);
        }
        plan
    }

    fn execute(
        &self,
        plan: &Plan,
        opts: &ExecOptions,
        progress: &Arc<Progress>,
    ) -> Result<Report, ExecError> {
        let step = |phase: Phase| {
            progress.set_phase(phase);
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay / 4);
            }
        };
        step(Phase::Preparing);
        let mut rep = Report {
            dry_run: opts.dry_run,
            ..Report::default()
        };
        let view = View::new(&self.tree, None);
        let mut loc = Locator::new(&self.tree);
        progress.set_items_total(plan.items.len() + usize::from(plan.bin.is_some()));
        if let Some(b) = &plan.bin {
            step(Phase::EmptyingBin);
            rep.bin = Some(BinReport {
                requested: b.items.len() as u64,
                purged: b.items.len() as u64,
                still_there: 0,
                changed: 0,
                unverifiable: 0,
                bytes_hint: b.bytes_hint,
                failed: false,
                ..BinReport::default()
            });
            progress.add_item_done();
        }
        step(Phase::Deleting);
        for it in &plan.items {
            if progress.is_cancelled() {
                break;
            }
            let unfit_skipped = it.is_unfit() && !opts.allow_unfit_permanent;
            let dest = if it.is_unfit() && opts.allow_unfit_permanent {
                Destination::Permanent
            } else {
                it.destination
            };
            let mut removed = Vec::new();
            let mut stats = StatsSnapshot::default();
            let status = if unfit_skipped {
                rep.skipped_unfit += 1;
                ItemStatus::SkippedUnfit
            } else if opts.dry_run {
                match dest {
                    Destination::Permanent => ItemStatus::Done,
                    Destination::Recycle => ItemStatus::WouldRecycle,
                }
            } else {
                stats.bytes = it.estimate.bytes;
                stats.logical = it.estimate.logical;
                stats.files = it.estimate.files;
                if let Some(node) = loc.locate(&it.path) {
                    match &it.spec {
                        UnitSpec::Contents { .. } => {
                            // One entry for each first-level son who reaches seniority.
                            let cutoff = if it.min_age.is_zero() {
                                u32::MAX
                            } else {
                                DEMO_NOW - it.min_age.as_secs() as u32
                            };
                            for c in view.kids(node).iter() {
                                let (b, l, f, all) = eligible_by_age(&view, c, cutoff);
                                if f == 0 {
                                    continue;
                                }
                                removed.push(Removed {
                                    path: it.path.join(self.tree.name_lossy(c).as_ref()),
                                    is_dir: self.tree.node(c).is_dir(),
                                    removed: all,
                                    bytes: b,
                                    logical: l,
                                    files: f,
                                });
                            }
                            if it.category == Some(CategoryId::UserTemp) {
                                // What a program had open (simulated).
                                stats.in_use = 37;
                            }
                        }
                        UnitSpec::Entry { is_dir, .. } => removed.push(Removed {
                            path: it.path.clone(),
                            is_dir: *is_dir,
                            removed: true,
                            bytes: it.estimate.bytes,
                            logical: it.estimate.logical,
                            files: it.estimate.files,
                        }),
                    }
                }
                match dest {
                    Destination::Permanent => ItemStatus::Done,
                    Destination::Recycle => ItemStatus::Recycled,
                }
            };
            match (dest, status) {
                (Destination::Permanent, ItemStatus::Done) => rep.permanent_bytes += stats.bytes,
                (Destination::Recycle, ItemStatus::Recycled | ItemStatus::WouldRecycle) => {
                    rep.recycled_bytes += if status == ItemStatus::WouldRecycle {
                        it.estimate.bytes
                    } else {
                        stats.bytes
                    };
                }
                _ => {}
            }
            if opts.dry_run && dest == Destination::Permanent && status == ItemStatus::Done {
                rep.permanent_bytes += it.estimate.bytes;
            }
            rep.totals.merge(&stats);
            rep.items.push(ItemReport {
                id: it.id,
                category: it.category,
                path: it.path.clone(),
                destination: dest,
                status,
                stats,
                removed,
            });
            progress.add_item_done();
        }
        step(Phase::Verifying);
        rep.cancelled = progress.is_cancelled();
        if !opts.dry_run && rep.recycled_bytes > 0 {
            // The entire dummy Recycle Bin: what you already had (the tree folder, if it wasn't emptied) plus what you just sent.
            let before = if rep.bin.is_some() {
                0
            } else {
                self.recycle_dir(&PathBuf::from(&self.root))
                    .and_then(|d| loc.locate(&d))
                    .map_or(0, |n| view.alloc(n))
            };
            rep.bin_total_after = Some(before + rep.recycled_bytes);
        }
        if !opts.dry_run {
            let bin = rep.bin.map_or(0, |b| b.bytes_hint);
            let freed = rep.permanent_bytes + bin;
            let d = &demo::drives()[0];
            rep.freed_now = Some(freed as i64);
            rep.volumes.push(tesela_cleaner::exec::VolumeDelta {
                root: PathBuf::from(&self.root),
                free_before: d.free,
                free_after: d.free + freed,
            });
        }
        progress.set_phase(Phase::Done);
        Ok(rep)
    }

    fn running_browsers(&self) -> Vec<Browser> {
        self.browsers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn data_dir(&self) -> Option<PathBuf> {
        None
    }

    fn is_demo(&self) -> bool {
        true
    }
}
