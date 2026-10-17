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
    /// Nombre EXACTO tal como lo lista el disco (una carpeta que distingue mayúsculas puede tener `Temp` y `temp`).
    pub name: OsString,
    pub id: u32,
    pub tag: u16,
    /// Carpeta de la nube (se abre dejando actuar a su filtro): si una ruta prioritaria ES esta carpeta y su tarea
    /// general se sustituye, la tarea nueva tiene que conservar la marca.
    pub cloud: bool,
}

#[derive(Default)]
pub(crate) struct Resolved {
    pub adopt: HashMap<u128, Adopt>,
    /// Rutas prioritarias aceptadas (índice = `Task::prio`).
    pub targets: Vec<PathBuf>,
    pub initial: Vec<Task>,
    /// Ids de carpetas de primer nivel que son ellas mismas rutas prioritarias: su tarea general se sustituye.
    pub takeover: Vec<u32>,
    /// Nodos precreados (ancestros y rutas prioritarias que no son de primer nivel).
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

/// Prefijo de ruta en una forma comparable: la unidad `C:` es la misma con o sin `\\?\`, y los recursos de red
/// (`\\<servidor>\<recurso>`) también; el resto (`Volume{GUID}`...) se compara por su texto. Todo en minúsculas.
fn norm_prefix(p: PrefixComponent<'_>) -> String {
    match p.kind() {
        Prefix::Disk(d) | Prefix::VerbatimDisk(d) => format!("{}:", d.to_ascii_lowercase() as char),
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
            format!("\\\\{}\\{}", lower(server), lower(share))
        }
        _ => lower(p.as_os_str()),
    }
}

/// (prefijo normalizado, componentes normales con su mayúscula original).
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
    // `GetLongPathNameW` no normaliza `..`, y `split` los descartaba: `<raiz>\a\..\b` se validaba como `<raiz>\a\b` mientras el
    // sistema la resolvería a `<raiz>\b`. Una candidata con `..` (o `.` inicial) es ambigua: se descarta. (`Path::components` ya
    // normaliza los `.` intermedios.)
    if long
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return None;
    }
    let (prefix, comps) = split(&long);
    // La candidata es una ruta sin canonizar: las mayúsculas pueden no ser las del disco.
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
    // 1. Candidatas que cuelgan de la raíz, validadas componente a componente desde ella (ver `resolve_candidate`).
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
        // Igual o dentro de otra ya aceptada (comparación exacta: `Temp` y `temp` pueden ser carpetas distintas).
        if kept_keys.iter().any(|a| k.starts_with(a)) {
            continue;
        }
        // Carpeta de primer nivel: ya existe.
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
            // Datos tal como los dio el listado del padre al validar la candidata. Tiene que ser un directorio normal
            // (`Walk`: tampoco uno con atributos de recuperación de la nube aunque no sea reparse point) con `FileId`.
            let m = metas[i];
            if !(walkable(m.attrs, m.reparse_tag) && m.file_id != 0) {
                // No es un directorio normal legible con FileId: se abandona esta ruta y se deshace lo añadido.
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
        // La tarea de una ruta que es ELLA MISMA una carpeta de primer nivel sustituye a la general de esa carpeta:
        // conserva su marca de nube. Una carpeta precreada (nivel 2 o más) es un directorio normal: nunca es de la nube.
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

    // 3. Reservar ids de golpe y publicar los nodos precreados.
    let base = if planned.is_empty() {
        0
    } else {
        sh.reserve_ids(planned.len() as u32)
    };
    if sh.id_overflow.load(std::sync::atomic::Ordering::SeqCst) {
        // Los ids se agotaron (el escaneo ya está cancelado y se marcará como tal): nada de lotes con ids envueltos.
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
        // No es un directorio.
        assert!(!walkable(0, 0));
        // Junction, symlink de carpeta o punto de montaje.
        assert!(!walkable(DIR | REPARSE, TAG_MOUNT_POINT));
        assert!(!walkable(DIR | REPARSE, TAG_SYMLINK));
        // Carpeta de la nube ya poblada (`WalkCloud`): se recorre con su filtro, pero NO es un destino prioritario.
        assert!(!walkable(DIR | REPARSE, TAG_CLOUD));
        // GM-M1: carpeta de la nube sin poblar, con o sin marca de reparse (`SkipUnpopulatedCloud`).
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

    /// Carpeta temporal propia (`%TEMP%\tesela-test-<uuid>`) que se borra al soltarla; los junctions se quitan antes como
    /// enlaces (`remove_dir` sobre un junction borra solo el enlace).
    struct Tmp(PathBuf, Vec<PathBuf>);

    impl Tmp {
        fn new() -> Tmp {
            let base = dunce::canonicalize(std::env::temp_dir()).unwrap();
            let p = base.join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&p).unwrap();
            Tmp(p, Vec::new())
        }

        /// `mklink /J` hacia una carpeta de ESTE mismo árbol (nunca una unidad ni un volumen).
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

    /// Raíz canónica (como la da `open_resolved`) de la carpeta temporal.
    fn canonical_root(t: &Tmp) -> PathBuf {
        sys::open_resolved(&t.0).unwrap().path
    }

    /// Árbol: `real\sub\hondo`, `otra` y un junction `enlace` -> `real` (todo dentro de la carpeta temporal propia).
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
        // `enlace\sub` es `real\sub` pasando por un junction: antes se canonizaba hacia el destino y se aceptaba.
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
        // Lo mismo si el enlace es la propia candidata.
        let r = resolve(&sh, &root, &children(), vec![t.0.join("enlace")]);
        assert!(r.targets.is_empty() && r.takeover.is_empty());
        // Y un enlace más arriba aunque el resto de la ruta sea normal.
        let r = resolve(
            &sh,
            &root,
            &children(),
            vec![t.0.join("enlace").join("sub").join("hondo")],
        );
        assert!(r.targets.is_empty());
        // La ruta real sí se acepta, con la cadena de ancestros precreada.
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
        // R-m2: `real\..\otra` NO es `real\otra` (que aquí existe): el sistema la resolvería a `otra`. Se descarta.
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
        // Sin `..` la misma carpeta sí vale (la prueba no es vacía).
        let r = resolve(&sh, &root, &children(), vec![t.0.join("real").join("otra")]);
        assert_eq!(r.targets, [root.join("real").join("otra")]);
    }

    #[test]
    fn la_candidata_se_valida_desde_la_raiz_y_sale_con_los_nombres_del_disco() {
        let t = tree();
        let root = canonical_root(&t);
        let sh = shared();
        // Mayúsculas distintas de las del disco: se acepta y la ruta sale con los nombres reales.
        let r = resolve(&sh, &root, &children(), vec![t.0.join("REAL").join("Sub")]);
        assert_eq!(r.targets, [root.join("real").join("sub")]);
        // Fuera de la raíz, la propia raíz, una ruta que no existe y una que es un archivo: nada.
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
        // GM-M1: la candidata ES una carpeta de primer nivel: su tarea sustituye a la general y antes salía siempre con
        // `cloud: false`. Con la marca del `RootChild` se conserva (y sin ella, no se inventa).
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
        // Una ruta más profunda se precrea como directorio normal: nunca es de la nube.
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
