//! Resolución de la cola prioritaria (PLAN.md §3.3): para cada ruta conocida que cuelga de la ruta escaneada se
//! reservan, ANTES de arrancar los hilos, los ids de su cadena de ancestros (nodos precreados con sus metadatos
//! reales) y se anota el `FileId` de cada directorio de la cadena. Así el recorrido general los reconoce por `FileId`
//! (no por la ruta en minúsculas: NTFS admite carpetas que distinguen mayúsculas) y adopta el id existente en vez de
//! crear otro nodo. Al terminar, el mapa de adopción es de solo lectura, sin locks.
//!
//! Los ancestros se quedan sin lote propio hasta que el recorrido general los lista, de modo que el builder los marca
//! `INCOMPLETE` mientras tanto y "Parar y ver" nunca enseña huérfanos.

use crate::classify;
use crate::sys::{self, EntryInfo};
use crate::walker::{Adopt, AdoptKind, NO_PRIO, Shared, Task};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use tesela_model::{Batch, EntryData};

/// Carpeta de primer nivel ya listada (la raíz se lista antes de resolver nada).
pub(crate) struct RootChild {
    /// Nombre en minúsculas, para comparar sin distinguir mayúsculas.
    pub name_lower: String,
    pub id: u32,
    pub tag: u16,
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

/// (prefijo en minúsculas, componentes normales con su mayúscula original).
fn split(p: &Path) -> (String, Vec<OsString>) {
    let mut prefix = String::new();
    let mut comps = Vec::new();
    for c in p.components() {
        match c {
            Component::Prefix(x) => prefix = lower(x.as_os_str()),
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

/// Resuelve `candidates` (rutas sin canonizar, sin comprobar). `root` es la ruta escaneada, canónica.
pub(crate) fn resolve(
    sh: &Shared,
    root: &Path,
    root_children: &[RootChild],
    candidates: Vec<PathBuf>,
) -> Resolved {
    let (root_prefix, root_comps) = split(root);
    // 1. Candidatas canónicas que cuelgan de la raíz, como componentes relativos a ella.
    let mut accepted: Vec<(PathBuf, Vec<OsString>)> = Vec::new();
    for p in candidates {
        let Ok(canon) = std::fs::canonicalize(&p) else {
            continue;
        };
        let (prefix, comps) = split(&canon);
        if prefix != root_prefix
            || comps.len() <= root_comps.len()
            || !comps
                .iter()
                .zip(&root_comps)
                .all(|(a, b)| lower(a) == lower(b))
        {
            continue;
        }
        let rel = comps[root_comps.len()..].to_vec();
        accepted.push((canon, rel));
    }
    // 2. Las más cercanas a la raíz primero; una ruta dentro de otra ya aceptada (o repetida) sobra.
    accepted.sort_by_key(|(_, rel)| (rel.len(), rel.iter().map(|c| lower(c)).collect::<Vec<_>>()));
    let key = |rel: &[OsString]| rel.iter().map(|c| lower(c)).collect::<Vec<_>>().join("\\");
    let mut kept_keys: Vec<String> = Vec::new();
    let mut out = Resolved::default();
    let mut planned: Vec<Planned> = Vec::new();
    let mut by_key: HashMap<String, NodeRef> = HashMap::new();
    let mut ids_of_target: Vec<(NodeRef, u16, PathBuf)> = Vec::new();

    'cand: for (canon, rel) in accepted {
        let k = key(&rel);
        if kept_keys
            .iter()
            .any(|a| k == *a || k.starts_with(&format!("{a}\\")))
        {
            continue;
        }
        // Carpeta de primer nivel: ya existe.
        let Some(c1) = root_children
            .iter()
            .find(|c| c.name_lower == lower(&rel[0]))
        else {
            continue;
        };
        let tag = c1.tag;
        let mark = planned.len();
        let mut added: Vec<String> = Vec::new();
        let mut parent = NodeRef::Existing(c1.id);
        let mut cur = root.join(&rel[0]);
        for (i, name) in rel.iter().enumerate().skip(1) {
            cur.push(name);
            let ck = key(&rel[..=i]);
            if let Some(&existing) = by_key.get(&ck) {
                parent = existing;
                continue;
            }
            // Datos tal como los daría el listado del padre (`cur` sin su último componente).
            let parent_path = cur.parent().map(sys::wide);
            let meta = parent_path.and_then(|pp| {
                sys::find_entry(&pp, &sys::wide(Path::new(name)))
                    .ok()
                    .flatten()
            });
            match meta {
                Some(m)
                    if classify::is_dir(m.attrs)
                        && !classify::is_reparse(m.attrs)
                        && m.file_id != 0 =>
                {
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
                _ => {
                    // No es un directorio real legible con FileId: se abandona esta ruta y se deshace lo añadido.
                    planned.truncate(mark);
                    for a in added {
                        by_key.remove(&a);
                    }
                    continue 'cand;
                }
            }
        }
        let idx = ids_of_target.len() as u16;
        match parent {
            NodeRef::Planned(j) => planned[j].target = Some(idx),
            NodeRef::Existing(id) => out.takeover.push(id),
        }
        ids_of_target.push((parent, tag, canon));
        kept_keys.push(k);
    }

    // 3. Reservar ids de golpe y publicar los nodos precreados.
    let base = if planned.is_empty() {
        0
    } else {
        sh.reserve_ids(planned.len() as u32)
    };
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
    for (idx, (r, tag, canon)) in ids_of_target.into_iter().enumerate() {
        out.initial.push(Task {
            path: sys::wide(&canon),
            id: id_of(r),
            tag,
            prio: idx as u16,
            claim: true,
        });
        out.targets.push(canon);
    }
    debug_assert!(out.initial.iter().all(|t| t.prio != NO_PRIO));
    out
}
