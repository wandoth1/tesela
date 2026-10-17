//! Recommendations (PLAN.md §2.6): what can be cleaned, with risk and destination, calculated on the scanned tree (a
//! through the cleanup overlay) and the roots of the categories given by `cleaner`. The Recommendations screen and
//! the headline "Up to X in low-risk candidates" come from here.
//!
//! The sizes are an ESTIMATE of the tree ("up to"): the final figure is measured by `cleaner` when confirming (with the
//! actual age of each file, protections and hard links).

use crate::describe::{self, InfoKind, Special};
use crate::treepath::Locator;
use std::path::PathBuf;
use tesela_cleaner::{Backend, Browser, CategoryId, Risk, rules};
use tesela_model::{NodeId, Tree, View};

/// A specific root of a category (for example, a Chrome profile cache) with its size.
#[derive(Clone, Debug)]
pub(crate) struct RecPart {
    pub node: NodeId,
    pub bytes: u64,
}

/// A row of the screen.
#[derive(Clone, Debug)]
pub(crate) struct RecRow {
    pub id: CategoryId,
    /// What is estimated to be released (with the minimum age of the rule applied with the tree dates).
    pub bytes: u64,
    pub parts: Vec<RecPart>,
    /// Browser is open: Row is not preselected ("Close X to free up to N").
    pub blocked_by: Option<Browser>,
}

impl RecRow {
    /// Can it be marked and shortlisted now?
    pub fn usable(&self) -> bool {
        self.blocked_by.is_none() && self.bytes > 0
    }
}

/// A report-only file (`pagefile.sys`, `hiberfil.sys`, `swapfile.sys`).
#[derive(Clone, Debug)]
pub(crate) struct InfoRow {
    pub kind: InfoKind,
    pub bytes: u64,
}

/// Why a category is not available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unavailable {
    /// There is a link (junction, symlink, mount point) in your folder path.
    Link,
    /// The folder changed while checking.
    Changed,
    /// The temporary folder is redirected away from the user's local data.
    Outside,
}

/// A category that exists on the computer but is not offered by `cleaner`, with reason (H4).
#[derive(Clone, Debug)]
pub(crate) struct UnavailableRow {
    pub id: CategoryId,
    pub why: Unavailable,
}

/// All recommendations.
#[derive(Clone, Debug, Default)]
pub(crate) struct Recs {
    pub rows: Vec<RecRow>,
    /// Categories that `cleaner` discarded with a reason that the user should know (they are displayed as "not available").
    pub unavailable: Vec<UnavailableRow>,
    pub info: Vec<InfoRow>,
    pub downloads: Option<(NodeId, u64)>,
    pub recycle: Option<(NodeId, u64)>,
}

impl Recs {
    pub fn row(&self, id: CategoryId) -> Option<&RecRow> {
        self.rows.iter().find(|r| r.id == id)
    }

    /// The safe thing that can be cleaned now (the headline and the "Clear the safe" button).
    pub fn safe_total(&self) -> u64 {
        self.rows
            .iter()
            .filter(|r| rules().get(r.id).risk == Risk::Safe && r.usable())
            .map(|r| r.bytes)
            .sum()
    }

    /// What to look for (Review): Downloads, the Recycle Bin, and error and dump reports.
    pub fn review_total(&self) -> u64 {
        self.downloads.map_or(0, |d| d.1)
            + self.recycle.map_or(0, |d| d.1)
            + self.diagnostics().map(|r| r.bytes).sum::<u64>()
    }

    /// Rows of bug reports and crash dumps with something to offer (Review, with checkbox, Recycle Bin).
    pub fn diagnostics(&self) -> impl Iterator<Item = &RecRow> {
        self.rows
            .iter()
            .filter(|r| r.id.is_diagnostic() && r.usable())
    }

    /// The safe categories that can be preselected.
    pub fn preselected(&self) -> Vec<CategoryId> {
        self.rows
            .iter()
            .filter(|r| rules().get(r.id).preselected() && r.usable())
            .map(|r| r.id)
            .collect()
    }
}

/// Add the items in the subtree of `id` that meet the age limit: `(disk bytes, logical size, file count, all items qualify)`. A
/// A file qualifies if its `mtime` is older than `cutoff` (Unix seconds; 0, meaning unknown, counts as old). With
/// `cutoff == u32::MAX`, every file qualifies.
pub(crate) fn eligible_by_age(view: &View, id: NodeId, cutoff: u32) -> (u64, u64, u64, bool) {
    let tree = view.tree();
    let n = tree.node(id);
    if !n.is_dir() {
        let ok = cutoff == u32::MAX || tree.mtime(id) < cutoff;
        return if ok {
            (view.alloc(id), view.logical(id), 1, true)
        } else {
            (0, 0, 0, false)
        };
    }
    let (mut b, mut l, mut f, mut all) = (0u64, 0u64, 0u64, true);
    let mut stack = vec![id];
    while let Some(d) = stack.pop() {
        for c in view.kids(d).iter() {
            let cn = tree.node(c);
            if cn.flags & tesela_model::flags::SYSTEM_INFO != 0 {
                continue;
            }
            if cn.is_dir() {
                stack.push(c);
            } else if cutoff == u32::MAX || tree.mtime(c) < cutoff {
                b += view.alloc(c);
                l += view.logical(c);
                f += 1;
            } else {
                all = false;
            }
        }
    }
    (b, l, f, all)
}

/// Calculate the recommendations. `now_unix`: "now" (the clock, or the show's landline).
pub(crate) fn compute(
    tree: &Tree,
    view: &View,
    backend: &dyn Backend,
    browsers_open: &[Browser],
    now_unix: u32,
) -> Recs {
    let mut loc = Locator::new(tree);
    let mut recs = Recs::default();
    for id in [
        CategoryId::UserTemp,
        CategoryId::WindowsTemp,
        CategoryId::ChromeCache,
        CategoryId::EdgeCache,
        CategoryId::FirefoxCache,
        CategoryId::WerReports,
        CategoryId::CrashDumps,
    ] {
        let rule = rules().get(id);
        let cutoff = if rule.min_age.is_zero() {
            u32::MAX
        } else {
            now_unix.saturating_sub(rule.min_age.as_secs() as u32)
        };
        let mut parts = Vec::new();
        for path in backend.category_roots(id) {
            let Some(node) = loc.locate(&path) else {
                continue;
            };
            if !view.is_visible(node) {
                continue;
            }
            let (bytes, ..) = eligible_by_age(view, node, cutoff);
            parts.push(RecPart { node, bytes });
        }
        if parts.is_empty() {
            // Discarded for security (a link in your path...): Does not disappear silently. It is only said if the discarded root
            // is INSIDE what was scanned (it is not enough that it is from the same drive: when scanning a folder, a root
            // from another part of the unit is not seen in this tree).
            let on_this_drive = |p: &std::path::Path| loc.covers(p);
            if let Some((_, why)) = backend
                .unavailable_roots(id)
                .into_iter()
                .find(|(p, _)| on_this_drive(p))
            {
                use tesela_cleaner::roots::RootRejection as R;
                let why = match why {
                    R::ReparseInChain { .. } => Unavailable::Link,
                    R::NotCanonical => Unavailable::Changed,
                    R::OutsideLocalAppData => Unavailable::Outside,
                    R::Os(_) => continue,
                };
                recs.unavailable.push(UnavailableRow { id, why });
            }
            continue;
        }
        let bytes = parts.iter().map(|p| p.bytes).sum();
        let blocked_by = Browser::from_category(id).filter(|b| browsers_open.contains(b));
        recs.rows.push(RecRow {
            id,
            bytes,
            parts,
            blocked_by,
        });
    }
    // Downloads and the Recycle Bin are shown only for review.
    if let Some(d) = backend.downloads_dir()
        && let Some(n) = loc.locate(&d)
        && view.is_visible(n)
    {
        recs.downloads = Some((n, view.alloc(n)));
    }
    let drive_root = PathBuf::from(tree.name_lossy(tree.root()).into_owned());
    if describe::is_drive_root_name(drive_root.to_string_lossy().trim_end_matches('\\'))
        && let Some(r) = backend.recycle_dir(&drive_root)
        && let Some(n) = loc.locate(&r)
        && view.is_visible(n)
    {
        recs.recycle = Some((n, view.alloc(n)));
    }
    // Only report files at the root of the volume.
    for c in view.kids(tree.root()).iter() {
        if let Some(Special::InfoOnly(kind)) = describe::special(tree, c) {
            recs.info.push(InfoRow {
                kind,
                bytes: view.alloc(c),
            });
        }
    }
    recs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::{self, DEMO_NOW, Variant};

    /// Single folder tree (`name` is the path that was "scanned").
    fn tree_of(name: &str) -> Tree {
        use tesela_model::{Batch, EntryData, FinishOptions, TreeBuilder, flags};
        let mut b = TreeBuilder::new();
        let mut root = Batch::detached(0);
        root.push(
            0,
            name.as_bytes(),
            EntryData {
                flags: flags::DIR,
                ..EntryData::default()
            },
        );
        b.add_batch(root).unwrap();
        b.add_batch(Batch::listing(0, 1)).unwrap();
        b.finish(&FinishOptions::new(DEMO_NOW))
    }

    /// Bug Reports and Crash Dumps: “Review.” They count on what is convenient to look at, never on what is certain, never
    /// They preselect and the basket puts them with what goes to the Recycle Bin.
    #[test]
    fn informes_y_volcados_son_revisar_y_no_cuentan_como_seguros() {
        let row = |id, bytes| RecRow {
            id,
            bytes,
            parts: Vec::new(),
            blocked_by: None,
        };
        let recs = Recs {
            rows: vec![
                row(CategoryId::UserTemp, 100),
                row(CategoryId::WerReports, 40),
                row(CategoryId::CrashDumps, 0),
            ],
            ..Recs::default()
        };
        assert_eq!(recs.safe_total(), 100);
        assert_eq!(recs.review_total(), 40);
        assert_eq!(recs.preselected(), vec![CategoryId::UserTemp]);
        let ids: Vec<CategoryId> = recs.diagnostics().map(|r| r.id).collect();
        assert_eq!(ids, vec![CategoryId::WerReports]);
        // They go to the Recycle Bin (what is recoverable), not to the "delete and recreate" group.
        for id in CategoryId::ALL {
            assert_eq!(
                crate::basket::category_is_recycled(id),
                id.is_diagnostic() || matches!(id, CategoryId::Downloads),
                "{id:?}"
            );
        }
    }

    /// GM-m7: a rejected root is shown only if it is inside the scanned tree, not merely because it is on the same drive.
    #[test]
    fn una_raiz_descartada_solo_se_enseña_si_esta_dentro_de_lo_analizado() {
        use tesela_cleaner::roots::RootRejection;
        let be = crate::demo_backend::DemoBackend::new(std::time::Duration::ZERO);
        be.set_unavailable(
            CategoryId::EdgeCache,
            RootRejection::ReparseInChain {
                component: "User Data".into(),
            },
        );
        let unavailable = |t: &Tree| {
            let view = View::new(t, None);
            compute(t, &view, &be, &[], DEMO_NOW).unavailable.len()
        };
        // The support of the proof rules out a root of yours within the unit: with the entire unit, yes.
        assert_eq!(unavailable(&tree_of("C:\\")), 1);
        assert_eq!(unavailable(&tree_of("C:\\Users")), 1);
        // In another folder on the SAME drive (letter C), no: it is not what you are seeing.
        assert_eq!(unavailable(&tree_of("C:\\Otra")), 0);
        assert_eq!(unavailable(&tree_of("C:\\Otra\\Sub")), 0);
    }

    #[test]
    fn la_antiguedad_solo_cuenta_lo_que_cumple() {
        let t = demo::tree(Variant::Complete);
        let view = View::new(&t, None);
        let mut loc = Locator::new(&t);
        let root = t.name_lossy(0).into_owned();
        let temp = loc
            .locate(std::path::Path::new(&format!(
                "{root}Users\\{}\\AppData\\Local\\Temp",
                demo::USER
            )))
            .unwrap();
        let all = eligible_by_age(&view, temp, u32::MAX);
        let old = eligible_by_age(&view, temp, DEMO_NOW - 86_400);
        assert!(all.3 && !old.3, "con antigüedad no todo cumple");
        assert_eq!(all.0, t.node(temp).alloc);
        assert!(old.0 > 0 && old.0 < all.0, "{old:?} {all:?}");
        // Files from 0 to 12 days: just over 90% are more than one day old.
        assert!(old.0 * 100 / all.0 > 70);
    }
}
