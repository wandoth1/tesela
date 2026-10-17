//! Priority queue resolution (PLAN.md §3.3): for each known path under the scanned path,
//! reserve the IDs of its ancestor chain BEFORE starting threads (precreated nodes with actual metadata)
//! and record each directory's `FileId`. General traversal then recognizes them by `FileId`
//! (not by a lowercased path: NTFS supports case-sensitive folders) and adopts the existing ID instead of
//! creating another node. Once resolved, the adoption map is read-only and needs no locks.
//!
//! Ancestors have no batch until general traversal lists them, so the builder marks them
//! `INCOMPLETE` in the meantime and "Stop and view" never displays orphan nodes.

use crate::classify;
use crate::sys::{self, EntryInfo};
use crate::walker::{Adopt, AdoptKind, NO_PRIO, Shared, Task};
use std::collections::HashMap;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf, Prefix, PrefixComponent};
use tesela_model::{Batch, EntryData};

/// A listed top-level folder (the root is listed before anything is resolved).
pub(crate) struct RootChild {
    /// EXACT name as listed on disk (a case-sensitive folder may contain both `Temp` and `temp`).
    pub name: OsString,
    pub id: u32,
    pub tag: u16,
    /// Cloud folder (opened through its filter): if a priority path IS this folder and replaces its general task,
    /// the replacement task must preserve this marker.
    pub cloud: bool,
}

#[derive(Default)]
pub(crate) struct Resolved {
    pub adopt: HashMap<u128, Adopt>,
    /// Accepted priority paths (index = `Task::prio`).
    pub targets: Vec<PathBuf>,
    pub initial: Vec<Task>,
    /// IDs of top-level folders that are themselves priority paths: their general task is replaced.
    pub takeover: Vec<u32>,
    /// Precreated nodes (ancestors and priority paths that are not top-level).
    pub batch: Option<Batch>,
}

fn lower(s: &std::ffi::OsStr) -> String {
    s.to_string_lossy().to_lowercase()
}

/// Top-level folder `name`: an exact match wins; a case-insensitive match is only a fallback and
/// is accepted only if unique.
fn find_root_child<'a>(children: &'a [RootChild], name: &OsString) -> Option<&'a RootChild> {
    if let Some(c) = children.iter().find(|c| c.name == *name) {
        return Some(c);
    }
    let lowered = lower(name);
    let mut hits = children.iter().filter(|c| lower(&c.name) == lowered);
    match (hits.next(), hits.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// Path prefix in a comparable form: drive `C:` is the same with or without `\\?\`, as are network shares
/// (`\\<server>\<share>`); other forms (`Volume{GUID}`, etc.) are compared as text. All characters are lowercased.
fn norm_prefix(p: PrefixComponent<'_>) -> String {
    match p.kind() {
        Prefix::Disk(d) | Prefix::VerbatimDisk(d) => format!("{}:", d.to_ascii_lowercase() as char),
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
            format!("\\\\{}\\{}", lower(server), lower(share))
        }
        _ => lower(p.as_os_str()),
    }
}

/// (normalized prefix, normal components with their original casing).
fn split(p: &Path) -> (String, Vec<OsString>) {
    let mut prefix = String::new();
    let mut comps = Vec::new();
    for c in p.components() {
        match c {
            Component::Prefix(x) => prefix = norm_prefix(x),
            Component::Normal(n) => comps.push(n.to_os_string()),
            _ => {}
        }
    }
    (prefix, comps)
}

#[derive(Clone, Copy)]
enum NodeRef {
    Existing(u32),
    Planned(usize),
}

struct Planned {
    parent: NodeRef,
    name: OsString,
    meta: EntryInfo,
    tag: u16,
    target: Option<u16>,
}

/// Should this directory, as listed by its parent, be traversed? ONLY if it is a normal directory
/// (`DirAction::Walk`): not a junction, symlink, mount point, or cloud folder (populated or not: unpopulated folders
/// are never listed, and populated ones are opened with their filter and are not priority targets), nor one with
/// recall attributes even if it lacks the reparse flag.
fn walkable(attrs: u32, reparse_tag: u32) -> bool {
    classify::is_dir(attrs) && classify::dir_action(attrs, reparse_tag) == classify::DirAction::Walk
}

/// A validated candidate: its canonical path, components with their exact on-disk names (relative to the root), and
/// metadata for each component from its parent's listing.
struct Candidate {
    canon: PathBuf,
    rel: Vec<OsString>,
    metas: Vec<EntryInfo>,
}

/// Validate candidate `p` (a non-canonicalized path) component by component FROM THE ROOT, without opening it first: at each step
/// list the parent (already validated: the root is canonical and not a link, and each previous component is a normal
/// directory) and look up the name. If the entry is a link (of any kind, including an ancestor), a cloud folder, or not a
/// normal directory, discard it WITHOUT opening its target. This builds the final path only from real on-disk names
/// under the root, without resolving links along the way. The only operation performed on the raw candidate beforehand
/// is expanding its 8.3 names (`%TEMP%` is often abbreviated) with `GetLongPathNameW`, which does not open paths for
/// traversal.
fn resolve_candidate(
    root: &Path,
    root_prefix: &str,
    root_comps: &[OsString],
    p: &Path,
) -> Option<Candidate> {
    let long = sys::long_path(p).ok()?;
    // `GetLongPathNameW` does not normalize `..`, and `split` discarded it: `<root>\a\..\b` was validated as
    // `<root>\a\b` while the system resolves it to `<root>\b`. A candidate containing `..` (or an initial `.`) is
    // ambiguous and is rejected. (`Path::components` already normalizes intermediate `.` components.)
    if long
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return None;
    }
    let (prefix, comps) = split(&long);
    // The candidate is not canonicalized: its casing may differ from the on-disk names.
    if prefix != root_prefix
        || comps.len() <= root_comps.len()
        || !comps
            .iter()
            .zip(root_comps)
            .all(|(a, b)| lower(a) == lower(b))
    {
        return None;
    }
    let mut canon = root.to_path_buf();
    let mut rel = Vec::new();
    let mut metas = Vec::new();
    for name in &comps[root_comps.len()..] {
        let (exact, meta) = sys::find_entry_named(&sys::wide(&canon), &sys::wide(Path::new(name)))
            .ok()
            .flatten()?;
        if !walkable(meta.attrs, meta.reparse_tag) {
            return None;
        }
        let exact = OsString::from_wide(&exact);
        canon.push(&exact);
        rel.push(exact);
        metas.push(meta);
    }
    Some(Candidate { canon, rel, metas })
}

/// Resolve `candidates` (uncanonicalized, unchecked paths). `root` is the canonical scanned path.
pub(crate) fn resolve(
    sh: &Shared,
    root: &Path,
    root_children: &[RootChild],
    candidates: Vec<PathBuf>,
) -> Resolved {
    let (root_prefix, root_comps) = split(root);
    // 1. Candidates beneath the root, validated component by component from it (see `resolve_candidate`).
    let mut accepted: Vec<Candidate> = Vec::new();
    for p in candidates {
        // Cancelled during startup: stop validating paths (I/O here may take time).
        if sh.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        if let Some(c) = resolve_candidate(root, &root_prefix, &root_comps, &p) {
            accepted.push(c);
        }
    }
    // 2. Process paths closest to the root first; paths under an accepted path (or duplicates) are redundant.
    accepted.sort_by_key(|c| (c.rel.len(), c.rel.clone()));
    let key = |rel: &[OsString]| -> Vec<OsString> { rel.to_vec() };
    let mut kept_keys: Vec<Vec<OsString>> = Vec::new();
    let mut out = Resolved::default();
    let mut planned: Vec<Planned> = Vec::new();
    let mut by_key: HashMap<Vec<OsString>, NodeRef> = HashMap::new();
    let mut ids_of_target: Vec<(NodeRef, u16, bool, PathBuf)> = Vec::new();

    'cand: for Candidate { canon, rel, metas } in accepted {
        if sh.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let k = key(&rel);
        // Equal to or beneath another accepted path (exact comparison: `Temp` and `temp` may be different folders).
        if kept_keys.iter().any(|a| k.starts_with(a)) {
            continue;
        }
        // Top-level folder: it already exists.
        let Some(c1) = find_root_child(root_children, &rel[0]) else {
            continue;
        };
        let tag = c1.tag;
        let mark = planned.len();
        let mut added: Vec<Vec<OsString>> = Vec::new();
        let mut parent = NodeRef::Existing(c1.id);
        for (i, name) in rel.iter().enumerate().skip(1) {
            let ck = key(&rel[..=i]);
            if let Some(&existing) = by_key.get(&ck) {
                parent = existing;
                continue;
            }
            // Data from the parent's listing when validating the candidate. It must be a normal directory
            // (`Walk`: no cloud recall attributes, even if it is not a reparse point) with a `FileId`.
            let m = metas[i];
            if !(walkable(m.attrs, m.reparse_tag) && m.file_id != 0) {
                // Not a readable normal directory with a FileId: abandon this path and roll back the additions.
                planned.truncate(mark);
                for a in added {
                    by_key.remove(&a);
                }
                continue 'cand;
            }
            planned.push(Planned {
                parent,
                name: name.clone(),
                meta: m,
                tag,
                target: None,
            });
            parent = NodeRef::Planned(planned.len() - 1);
            by_key.insert(ck.clone(), parent);
            added.push(ck);
        }
        let idx = ids_of_target.len() as u16;
        // A priority path that IS a top-level folder replaces that folder's general task and preserves its cloud marker.
        // A precreated folder (level 2 or deeper) is a normal directory, never a cloud folder.
        let cloud = match parent {
            NodeRef::Planned(j) => {
                planned[j].target = Some(idx);
                false
            }
            NodeRef::Existing(id) => {
                out.takeover.push(id);
                c1.cloud
            }
        };
        ids_of_target.push((parent, tag, cloud, canon));
        kept_keys.push(k);
    }

    // 3. Reserve IDs in one batch and publish the precreated nodes.
    let base = if planned.is_empty() {
        0
    } else {
        sh.reserve_ids(planned.len() as u32)
    };
    if sh.id_overflow.load(std::sync::atomic::Ordering::SeqCst) {
        // IDs are exhausted (the scan is already cancelled and will be marked accordingly): never return a batch with wrapped IDs.
        return Resolved::default();
    }
    let id_of = |r: NodeRef| match r {
        NodeRef::Existing(id) => id,
        NodeRef::Planned(j) => base + j as u32,
    };
    if !planned.is_empty() {
        let mut batch = Batch::detached(base);
        for (j, p) in planned.iter().enumerate() {
            let id = batch.push_utf16(
                id_of(p.parent),
                &sys::wide(Path::new(&p.name)),
                EntryData {
                    flags: classify::entry_flags(p.meta.attrs, p.meta.reparse_tag),
                    alloc: p.meta.alloc,
                    logical: p.meta.logical,
                    mtime: classify::filetime_to_unix(p.meta.mtime_filetime),
                    file_id: 0,
                },
            );
            debug_assert_eq!(id, base + j as u32);
            let kind = p.target.map_or(AdoptKind::Ancestor, AdoptKind::Target);
            out.adopt.insert(
                p.meta.file_id,
                Adopt {
                    id,
                    tag: p.tag,
                    kind,
                },
            );
        }
        out.batch = Some(batch);
    }
    for (idx, (r, tag, cloud, canon)) in ids_of_target.into_iter().enumerate() {
        out.initial.push(Task {
            path: sys::wide(&canon),
            id: id_of(r),
            tag,
            prio: idx as u16,
            claim: true,
            cloud,
        });
        out.targets.push(canon);
    }
    debug_assert!(out.initial.iter().all(|t| t.prio != NO_PRIO));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::Progress;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    const DIR: u32 = 0x10;
    const REPARSE: u32 = 0x400;
    const RECALL_ON_OPEN: u32 = 0x4_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
    const TAG_MOUNT_POINT: u32 = 0xA000_0003;
    const TAG_SYMLINK: u32 = 0xA000_000C;
    const TAG_CLOUD: u32 = 0x9000_001A;

    #[test]
    fn solo_se_recorre_un_directorio_normal() {
        assert!(walkable(DIR, 0));
        // Not a directory.
        assert!(!walkable(0, 0));
        // Directory junction, symlink, or mount point.
        assert!(!walkable(DIR | REPARSE, TAG_MOUNT_POINT));
        assert!(!walkable(DIR | REPARSE, TAG_SYMLINK));
        // Populated cloud folder (`WalkCloud`): it is traversed through its filter, but is NOT a priority target.
        assert!(!walkable(DIR | REPARSE, TAG_CLOUD));
        // GM-M1: unpopulated cloud folder, with or without a reparse tag (`SkipUnpopulatedCloud`).
        assert!(!walkable(DIR | REPARSE | RECALL_ON_DATA_ACCESS, TAG_CLOUD));
        assert!(!walkable(DIR | RECALL_ON_OPEN, 0));
        assert!(!walkable(DIR | RECALL_ON_DATA_ACCESS, 0));
    }

    #[test]
    fn el_prefijo_de_unidad_se_compara_con_o_sin_verbatim() {
        let pre = |p: &str| split(Path::new(p)).0;
        assert_eq!(pre(r"C:\a"), pre(r"\\?\c:\b"));
        assert_ne!(
            pre(r"C:\a"),
            pre(r"\\?\Volume{0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9}\a")
        );
        assert_eq!(pre(r"\\<Srv>\<Share>\a"), pre(r"\\?\UNC\<srv>\<share>\b"));
        assert_ne!(pre(r"\\<srv>\<share>\a"), pre(r"\\<srv>\<otro>\a"));
        assert_eq!(
            pre(r"\\?\Volume{0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9}\a"),
            pre(r"\\?\VOLUME{0A1B2C3D-4E5F-6071-8293-A4B5C6D7E8F9}\b")
        );
        assert_eq!(split(Path::new(r"C:\Datos\Ana")).1, ["Datos", "Ana"]);
    }

    /// Test-owned temporary folder (`%TEMP%\tesela-test-<uuid>`) removed on drop; junctions are first removed as links
    /// (`remove_dir` on a junction removes only the link).
    struct Tmp(PathBuf, Vec<PathBuf>);

    impl Tmp {
        fn new() -> Tmp {
            let base = dunce::canonicalize(std::env::temp_dir()).unwrap();
            let p = base.join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&p).unwrap();
            Tmp(p, Vec::new())
        }

        /// `mklink /J` to a folder in THIS same tree (never a drive or volume).
        fn junction(&mut self, link: &str, target: &str) -> PathBuf {
            let (l, t) = (self.0.join(link), self.0.join(target));
            let out = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&l)
                .arg(&t)
                .output()
                .unwrap();
            assert!(out.status.success(), "mklink /J falló");
            self.1.push(l.clone());
            l
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            for j in &self.1 {
                let _ = std::fs::remove_dir(j);
            }
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn shared() -> Shared {
        Shared::new(
            Arc::new(AtomicBool::new(false)),
            Arc::new(Progress::default()),
            4096,
            true,
        )
    }

    fn child(name: &str, id: u32, cloud: bool) -> RootChild {
        RootChild {
            name: OsString::from(name),
            id,
            tag: id as u16,
            cloud,
        }
    }

    /// Canonical root of the temporary folder (as returned by `open_resolved`).
    fn canonical_root(t: &Tmp) -> PathBuf {
        sys::open_resolved(&t.0).unwrap().path
    }

    /// Tree: `real\sub\hondo`, `otra`, and a junction `enlace` -> `real` (all inside the test-owned temporary folder).
    fn tree() -> Tmp {
        let mut t = Tmp::new();
        std::fs::create_dir_all(t.0.join("real").join("sub").join("hondo")).unwrap();
        std::fs::create_dir(t.0.join("otra")).unwrap();
        t.junction("enlace", "real");
        t
    }

    fn children() -> Vec<RootChild> {
        vec![
            child("real", 1, false),
            child("enlace", 2, false),
            child("otra", 3, false),
        ]
    }

    #[test]
    fn una_candidata_con_un_junction_en_un_ancestro_se_descarta_en_vez_de_canonizarla() {
        let t = tree();
        let root = canonical_root(&t);
        let sh = shared();
        // `enlace\sub` is `real\sub` through a junction: it was previously canonicalized to the target and accepted.
        let r = resolve(
            &sh,
            &root,
            &children(),
            vec![t.0.join("enlace").join("sub")],
        );
        assert!(
            r.targets.is_empty() && r.initial.is_empty() && r.takeover.is_empty(),
            "una ruta que pasa por un enlace no es prioritaria"
        );
        assert!(r.batch.is_none() && r.adopt.is_empty());
        // The same applies when the link itself is the candidate.
        let r = resolve(&sh, &root, &children(), vec![t.0.join("enlace")]);
        assert!(r.targets.is_empty() && r.takeover.is_empty());
        // Also reject a higher link even when the rest of the path is ordinary.
        let r = resolve(
            &sh,
            &root,
            &children(),
            vec![t.0.join("enlace").join("sub").join("hondo")],
        );
        assert!(r.targets.is_empty());
        // The real path is accepted, with its ancestor chain precreated.
        let r = resolve(
            &sh,
            &root,
            &children(),
            vec![t.0.join("real").join("sub").join("hondo")],
        );
        assert_eq!(r.targets, [root.join("real").join("sub").join("hondo")]);
        assert_eq!(r.initial.len(), 1);
        assert!(r.takeover.is_empty());
        assert_eq!(r.adopt.len(), 2, "sub y hondo se precrean");
    }

    #[test]
    fn una_candidata_con_punto_punto_se_descarta() {
        // R-m2: `real\..\otra` is NOT `real\otra` (which exists here): the system would resolve it to `otra`. Discard it.
        let t = tree();
        std::fs::create_dir(t.0.join("real").join("otra")).unwrap();
        let root = canonical_root(&t);
        let sh = shared();
        for rel in [
            t.0.join("real").join("..").join("otra"),
            t.0.join("real").join("sub").join("..").join("sub"),
            t.0.join("..").join(t.0.file_name().unwrap()).join("otra"),
        ] {
            let r = resolve(&sh, &root, &children(), vec![rel.clone()]);
            assert!(
                r.targets.is_empty() && r.initial.is_empty(),
                "se aceptó {rel:?}: {:?}",
                r.targets
            );
        }
        // Without `..`, the same folder is valid (the test is not vacuous).
        let r = resolve(&sh, &root, &children(), vec![t.0.join("real").join("otra")]);
        assert_eq!(r.targets, [root.join("real").join("otra")]);
    }

    #[test]
    fn la_candidata_se_valida_desde_la_raiz_y_sale_con_los_nombres_del_disco() {
        let t = tree();
        let root = canonical_root(&t);
        let sh = shared();
        // Different casing from disk: accepted, and the path is returned with the actual names.
        let r = resolve(&sh, &root, &children(), vec![t.0.join("REAL").join("Sub")]);
        assert_eq!(r.targets, [root.join("real").join("sub")]);
        // Outside the root, the root itself, a nonexistent path, and a file path: none are accepted.
        let other = Tmp::new();
        std::fs::write(t.0.join("otra").join("a.txt"), b"x").unwrap();
        let r = resolve(
            &sh,
            &root,
            &children(),
            vec![
                other.0.clone(),
                t.0.clone(),
                t.0.join("real").join("no-existe"),
                t.0.join("otra").join("a.txt"),
            ],
        );
        assert!(r.targets.is_empty() && r.initial.is_empty());
    }

    #[test]
    fn la_tarea_de_una_carpeta_de_primer_nivel_conserva_su_marca_de_nube() {
        let t = tree();
        let root = canonical_root(&t);
        let sh = shared();
        // GM-M1: the candidate IS a top-level folder, so its task replaces the general task and previously always had
        // `cloud: false`. Preserve the marker from `RootChild` (and do not invent one when it is absent).
        for cloud in [true, false] {
            let kids = vec![child("real", 1, cloud), child("otra", 3, false)];
            let r = resolve(&sh, &root, &kids, vec![t.0.join("real")]);
            assert_eq!(r.takeover, [1]);
            assert_eq!(r.initial.len(), 1);
            assert_eq!(r.initial[0].cloud, cloud);
            assert_eq!(
                (r.initial[0].id, r.initial[0].tag, r.initial[0].claim),
                (1, 1, true)
            );
        }
        // A deeper path is precreated as an ordinary directory: it is never a cloud folder.
        let kids = vec![child("real", 1, true)];
        let r = resolve(&sh, &root, &kids, vec![t.0.join("real").join("sub")]);
        assert_eq!(r.initial.len(), 1);
        assert!(!r.initial[0].cloud && r.takeover.is_empty());
    }

    #[test]
    fn si_los_ids_se_agotan_no_se_publica_ningun_lote() {
        let t = tree();
        let root = canonical_root(&t);
        let sh = shared();
        sh.ids.store(u32::MAX - 2, Ordering::SeqCst);
        let r = resolve(
            &sh,
            &root,
            &children(),
            vec![
                t.0.join("real").join("sub"),
                t.0.join("real").join("sub").join("hondo"),
            ],
        );
        assert!(sh.id_overflow.load(Ordering::SeqCst));
        assert!(r.batch.is_none() && r.initial.is_empty() && r.adopt.is_empty());
    }
}
