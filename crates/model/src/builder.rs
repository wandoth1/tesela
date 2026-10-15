//! `TreeBuilder`: de registros planos (en cualquier orden) al `Tree` final (PLAN.md §3.2).
//!
//! # Orden del algoritmo (`finish`)
//! 1. Flags derivados: un directorio del que no llegó ningún lote es `INCOMPLETE`; uno cuyo listado falló, `ERROR`.
//! 2. Adyacencia provisional (ordenación por cuenta del padre) y recorrido BFS desde la raíz. Lo que no cuelga de la
//!    raíz (entradas cuyo padre no llegó) queda fuera del árbol y se cuenta como huérfano.
//! 3. **Deduplicación de enlaces duros** por `FileId`, SOLO entre las entradas alcanzadas en el BFS (un huérfano no
//!    puede quitarle el tamaño a un enlace que sí está en el árbol). Va ANTES de agregar porque pone `alloc` y
//!    `logical` a 0 en los enlaces sobrantes, y eso cambia las sumas de los directorios.
//! 4. Agregación bottom-up recorriendo ese BFS al revés: `alloc`, `logical`, `files_below`, los acumuladores `u64`
//!    (saturantes) de los histogramas y `INCOMPLETE_BELOW`. Los umbrales del 50 % y el 80 % se aplican sobre los `u64`.
//! 5. Ordenación de los hijos de cada directorio, UNA sola vez, por `(alloc desc, nombre asc)`; el nodo sintético
//!    `SYSTEM_INFO` va el último. Solo existe si no hay directorios `INCOMPLETE` (si faltan cosas por escanear, la
//!    diferencia con el espacio usado no es "espacio no explicado").
//! 6. Asignación de los ids finales en BFS y escritura de `Node`, `mtime` e `hist`.
//!
//! # Determinismo
//! El `Tree` solo depende del árbol lógico, no de los ids provisionales ni del orden de llegada: los empates de
//! tamaño se deshacen por nombre, y entre enlaces duros conserva el tamaño el de ruta completa menor en orden
//! lexicográfico de bytes WTF-8 (la ruta se compone con `\` desde la raíz, sin incluir el nombre de la raíz).
//!
//! # Sin recursión
//! Todo se hace con vectores indexados y recorridos iterativos: la profundidad del árbol no gasta pila.

use crate::flags;
use crate::record::{Batch, DirOutcome, NO_PARENT, Record};
use crate::tree::{Hist, Node, Tree, TreeStats};
use crate::types::{AGE_BUCKETS, FileType, age_bucket};
use rustc_hash::FxHashMap;
use std::cmp::Ordering;

/// Errores al añadir un lote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildError {
    /// Los ids del lote no caben en `u32` o chocan con `NO_PARENT`.
    IdOverflow,
    /// El arena de nombres superaría los 4 GiB (los desplazamientos son `u32`).
    NamesOverflow,
    /// Un id se ha usado dos veces.
    DuplicateId(u32),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::IdOverflow => write!(f, "demasiadas entradas: los ids no caben en u32"),
            BuildError::NamesOverflow => write!(f, "los nombres superan los 4 GiB"),
            BuildError::DuplicateId(id) => {
                write!(f, "el id provisional {id} se ha usado dos veces")
            }
        }
    }
}

impl std::error::Error for BuildError {}

/// Opciones de [`TreeBuilder::finish`].
#[derive(Clone, Copy, Debug)]
pub struct FinishOptions {
    /// "Ahora" en segundos Unix, para los tramos de antigüedad (se pasa explícito para que sea determinista).
    pub now_unix: u32,
    /// Deduplicar enlaces duros por `FileId` (por defecto sí).
    pub dedup_hard_links: bool,
    /// Bytes usados del volumen (`total − libre`) si es un escaneo de volumen completo y terminado. Si supera lo
    /// escaneado, la diferencia se añade como nodo sintético `SYSTEM_INFO` hijo de la raíz (PLAN.md §2.8); se recorta
    /// a ≥ 0.
    pub volume_used: Option<u64>,
}

impl FinishOptions {
    /// Opciones por defecto con el "ahora" indicado.
    pub fn new(now_unix: u32) -> FinishOptions {
        FinishOptions {
            now_unix,
            dedup_hard_links: true,
            volume_used: None,
        }
    }
}

/// Nombre del nodo sintético de espacio no explicado (la interfaz lo muestra traducido según el flag).
pub const SYSTEM_INFO_NAME: &[u8] = b"[system-data]";

#[derive(Default)]
struct BitSet(Vec<u64>);

impl BitSet {
    fn set(&mut self, i: u32) {
        let w = (i / 64) as usize;
        if self.0.len() <= w {
            self.0.resize(w + 1, 0);
        }
        self.0[w] |= 1 << (i % 64);
    }
    fn get(&self, i: u32) -> bool {
        self.0
            .get((i / 64) as usize)
            .is_some_and(|w| w & (1 << (i % 64)) != 0)
    }
}

/// Acumula lotes y construye el [`Tree`].
#[derive(Default)]
pub struct TreeBuilder {
    /// Por id provisional; `parent == NO_PARENT` = sin entrada.
    recs: Vec<Record>,
    /// Arena de nombres (los lotes se concatenan tal cual, así los hermanos quedan contiguos).
    names: Vec<u8>,
    /// Directorios de los que llegó un lote.
    enumerated: BitSet,
    /// Directorios cuyo listado falló.
    failed: BitSet,
    /// Candidatos a enlace duro: (mitad baja del `FileId`, id provisional). 16 B por archivo.
    link_lo: Vec<(u64, u32)>,
    /// Mitad alta del `FileId` cuando no es 0 (en NTFS siempre lo es; en ReFS no).
    link_hi: FxHashMap<u32, u64>,
}

impl TreeBuilder {
    /// Builder vacío.
    pub fn new() -> TreeBuilder {
        TreeBuilder::default()
    }

    /// Builder con espacio para `entries` entradas y `name_bytes` bytes de nombres.
    pub fn with_capacity(entries: usize, name_bytes: usize) -> TreeBuilder {
        TreeBuilder {
            recs: Vec::with_capacity(entries),
            names: Vec::with_capacity(name_bytes),
            link_lo: Vec::with_capacity(entries),
            ..TreeBuilder::default()
        }
    }

    /// Entradas reservadas hasta ahora (ids provisionales vistos, con huecos).
    pub fn slots(&self) -> usize {
        self.recs.len()
    }

    /// Añade un lote. Se admite cualquier orden de llegada. Si falla, el builder no cambia.
    pub fn add_batch(&mut self, batch: Batch) -> Result<(), BuildError> {
        let Batch {
            dir,
            outcome,
            base_id,
            records,
            file_ids,
            names,
        } = batch;
        let end = base_id as u64 + records.len() as u64;
        if end >= NO_PARENT as u64 {
            return Err(BuildError::IdOverflow);
        }
        let off = self.names.len();
        if off as u64 + names.len() as u64 > u32::MAX as u64 {
            return Err(BuildError::NamesOverflow);
        }
        let (start, end) = (base_id as usize, end as usize);
        for (i, r) in records.iter().enumerate() {
            if r.is_present() && self.recs.get(start + i).is_some_and(Record::is_present) {
                return Err(BuildError::DuplicateId((start + i) as u32));
            }
        }
        if self.recs.len() < end {
            self.recs.resize(end, Record::HOLE);
        }
        for (i, (mut r, fid)) in records.into_iter().zip(file_ids).enumerate() {
            if !r.is_present() {
                continue;
            }
            r.name_off += off as u32;
            let id = (start + i) as u32;
            if r.flags & flags::DIR == 0 && fid != 0 {
                self.link_lo.push((fid as u64, id));
                let hi = (fid >> 64) as u64;
                if hi != 0 {
                    self.link_hi.insert(id, hi);
                }
            }
            self.recs[start + i] = r;
        }
        self.names.extend_from_slice(&names);
        if let Some(d) = dir {
            self.enumerated.set(d);
            if let DirOutcome::Error(_) = outcome {
                self.failed.set(d);
            }
        }
        Ok(())
    }

    /// Construye el árbol. Ver la documentación del módulo para el orden de los pasos.
    pub fn finish(self, opts: &FinishOptions) -> Tree {
        let TreeBuilder {
            mut recs,
            mut names,
            enumerated,
            failed,
            mut link_lo,
            link_hi,
        } = self;
        let n = recs.len();
        if n == 0 || !recs[0].is_present() {
            return Tree::placeholder();
        }
        let mut stats = TreeStats::default();
        recs[0].flags |= flags::DIR; // la raíz siempre es un directorio

        // 1. Flags derivados.
        for (id, r) in recs.iter_mut().enumerate() {
            if !r.is_present() {
                continue;
            }
            let id = id as u32;
            if r.flags & flags::DIR != 0 && r.flags & flags::REPARSE == 0 && !enumerated.get(id) {
                r.flags |= flags::INCOMPLETE;
            }
            if failed.get(id) {
                r.flags |= flags::ERROR;
            }
        }

        let mut first = vec![0u32; n + 1];
        let valid_parent = |recs: &[Record], p: &Record| -> Option<usize> {
            let par = p.parent as usize;
            (p.is_present()
                && par < recs.len()
                && recs[par].is_present()
                && recs[par].flags & flags::DIR != 0)
                .then_some(par)
        };
        let mut present = 0u64;
        for p in 0..n {
            if recs[p].is_present() {
                present += 1;
            }
            if p == 0 {
                continue;
            }
            if let Some(par) = valid_parent(&recs, &recs[p]) {
                first[par + 1] += 1;
            }
        }
        for i in 0..n {
            first[i + 1] += first[i];
        }
        let mut kids = vec![0u32; first[n] as usize];
        for p in 1..n {
            if let Some(par) = valid_parent(&recs, &recs[p]) {
                kids[first[par] as usize] = p as u32;
                first[par] += 1;
            }
        }
        for i in (1..=n).rev() {
            first[i] = first[i - 1];
        }
        first[0] = 0;

        let mut order: Vec<u32> = Vec::with_capacity(n + 1);
        let mut slot = vec![u32::MAX; n]; // índice del directorio en `acc`
        let mut dir_count = 0u32;
        order.push(0);
        let mut i = 0;
        while i < order.len() {
            let p = order[i] as usize;
            i += 1;
            if recs[p].flags & flags::DIR != 0 {
                slot[p] = dir_count;
                dir_count += 1;
            }
            order.extend_from_slice(&kids[first[p] as usize..first[p + 1] as usize]);
        }
        stats.orphans = present - order.len() as u64;

        // 3. Deduplicación de enlaces duros, SOLO entre entradas que cuelgan de la raíz: un huérfano (su padre no
        // llegó) no está en el árbol y no puede quitarle el tamaño a un enlace que sí está.
        if opts.dedup_hard_links {
            let mut reached = BitSet(vec![0u64; n / 64 + 1]);
            for &p in &order {
                reached.set(p);
            }
            link_lo.retain(|&(_, id)| reached.get(id));
            dedup_hard_links(&mut recs, &names, &mut link_lo, &link_hi, &mut stats);
        }
        drop(link_lo);
        drop(link_hi);

        // 4. Agregación bottom-up.
        let mut acc = vec![Acc::default(); dir_count as usize];
        for idx in (1..order.len()).rev() {
            let p = order[idx] as usize;
            let r = recs[p];
            let par = r.parent as usize;
            let ps = slot[par] as usize;
            if r.flags & flags::DIR != 0 {
                let child = acc[slot[p] as usize];
                let dst = &mut acc[ps];
                for (d, s) in dst.bytes.iter_mut().zip(child.bytes) {
                    *d = d.saturating_add(s);
                }
                dst.files = dst.files.saturating_add(child.files);
                if r.flags & flags::REPARSE != 0 {
                    stats.reparse_dirs += 1;
                } else {
                    stats.dirs += 1;
                }
                if r.flags & flags::INCOMPLETE != 0 {
                    stats.incomplete_dirs += 1;
                }
                if r.flags & flags::ERROR != 0 {
                    stats.error_dirs += 1;
                }
                // Los ancestros de un directorio incompleto (o con algo incompleto debajo) lo saben: su tamaño es un
                // mínimo ("≥"). El hijo ya tiene su propio INCOMPLETE_BELOW porque se procesa antes que el padre.
                if r.flags & (flags::INCOMPLETE | flags::INCOMPLETE_BELOW) != 0 {
                    recs[par].flags |= flags::INCOMPLETE_BELOW;
                }
            } else if r.flags & flags::SYSTEM_INFO == 0 {
                let dst = &mut acc[ps];
                dst.files = dst.files.saturating_add(1);
                let ty = FileType::classify(
                    &names[r.name_off as usize..r.name_off as usize + r.name_len as usize],
                );
                dst.bytes[ty as usize] = dst.bytes[ty as usize].saturating_add(r.alloc);
                // Un mtime de 0 es "desconocido" (o anterior a 1970): no cuenta como el tramo más antiguo.
                if r.mtime != 0 {
                    let a = FileType::COUNT + age_bucket(opts.now_unix, r.mtime);
                    dst.bytes[a] = dst.bytes[a].saturating_add(r.alloc);
                }
                stats.files += 1;
                if r.flags & flags::REPARSE != 0 {
                    stats.reparse_files += 1;
                }
            }
            if r.flags & flags::CLOUD != 0 {
                stats.cloud_entries += 1;
            }
            let parent = &mut recs[par];
            parent.alloc = parent.alloc.saturating_add(r.alloc);
            parent.logical = parent.logical.saturating_add(r.logical);
        }
        stats.dirs += 1; // la raíz
        if recs[0].flags & flags::INCOMPLETE != 0 {
            stats.incomplete_dirs += 1;
        }
        if recs[0].flags & flags::ERROR != 0 {
            stats.error_dirs += 1;
        }
        if recs[0].flags & flags::CLOUD != 0 {
            stats.cloud_entries += 1;
        }
        stats.scanned_alloc = recs[0].alloc;

        // Nodo sintético de espacio no explicado (solo hijo de la raíz).
        let synthetic = match opts.volume_used {
            // Con directorios incompletos la diferencia no es "espacio no explicado", es lo que falta por escanear.
            Some(used) if used > stats.scanned_alloc && stats.incomplete_dirs == 0 => {
                let alloc = used - stats.scanned_alloc;
                stats.unexplained_alloc = alloc;
                recs[0].alloc = recs[0].alloc.saturating_add(alloc);
                let name_off = names.len() as u32;
                names.extend_from_slice(SYSTEM_INFO_NAME);
                recs.push(Record {
                    alloc,
                    logical: 0,
                    parent: 0,
                    name_off,
                    mtime: 0,
                    name_len: SYSTEM_INFO_NAME.len() as u16,
                    flags: flags::SYSTEM_INFO,
                });
                Some(n as u32)
            }
            _ => None,
        };

        // 5 y 6. Orden de los hijos y ids finales en BFS.
        let total = order.len() + synthetic.is_some() as usize;
        let mut nodes: Vec<Node> = Vec::with_capacity(total);
        let mut mtime: Vec<u32> = Vec::with_capacity(total);
        let mut hist: Vec<Hist> = Vec::with_capacity(total);
        let make_node = |p: usize,
                         parent: u32,
                         nodes: &mut Vec<Node>,
                         mtime: &mut Vec<u32>,
                         hist: &mut Vec<Hist>| {
            let r = &recs[p];
            let (h, files_below) = if r.flags & flags::DIR != 0 {
                let a = &acc[slot[p] as usize];
                (a.to_hist(), a.files)
            } else {
                (Hist::EMPTY, 0)
            };
            nodes.push(Node {
                alloc: r.alloc,
                logical: r.logical,
                parent,
                first_child: 0,
                child_count: 0,
                name_off: r.name_off,
                name_len: r.name_len,
                flags: r.flags,
                files_below,
            });
            mtime.push(r.mtime);
            hist.push(h);
        };
        make_node(0, NO_PARENT, &mut nodes, &mut mtime, &mut hist);
        order.clear();
        order.push(0);
        let mut f = 0usize;
        while f < order.len() {
            let p = order[f] as usize;
            if p >= n {
                f += 1;
                continue; // el nodo sintético no tiene hijos
            }
            let kslice = &mut kids[first[p] as usize..first[p + 1] as usize];
            if kslice.len() > 1 {
                kslice.sort_unstable_by(|&a, &b| compare_siblings(&recs, &names, a, b));
            }
            let extra = if p == 0 { synthetic } else { None };
            let count = kslice.len() + extra.is_some() as usize;
            if count > 0 {
                nodes[f].first_child = nodes.len() as u32;
                nodes[f].child_count = count as u32;
            }
            for &c in kslice.iter().chain(extra.iter()) {
                make_node(c as usize, f as u32, &mut nodes, &mut mtime, &mut hist);
                order.push(c);
            }
            f += 1;
        }
        names.shrink_to_fit();
        nodes.shrink_to_fit();
        Tree {
            nodes,
            names,
            mtime,
            hist,
            stats,
        }
    }
}

/// Orden de hermanos: el nodo sintético el último; luego `alloc` descendente; luego nombre ascendente.
fn compare_siblings(recs: &[Record], names: &[u8], a: u32, b: u32) -> Ordering {
    let (ra, rb) = (&recs[a as usize], &recs[b as usize]);
    let (sa, sb) = (
        ra.flags & flags::SYSTEM_INFO != 0,
        rb.flags & flags::SYSTEM_INFO != 0,
    );
    sa.cmp(&sb)
        .then(rb.alloc.cmp(&ra.alloc))
        .then_with(|| name_of(names, ra).cmp(name_of(names, rb)))
}

fn name_of<'a>(names: &'a [u8], r: &Record) -> &'a [u8] {
    &names[r.name_off as usize..r.name_off as usize + r.name_len as usize]
}

/// Acumuladores `u64` de un directorio: bytes por tipo (7) y por tramo de antigüedad (5) y número de archivos.
#[derive(Clone, Copy, Default)]
struct Acc {
    bytes: [u64; FileType::COUNT + AGE_BUCKETS],
    files: u32,
}

impl Acc {
    /// Porcentajes (para dibujar) y resultados de los umbrales, todos calculados sobre los `u64`.
    fn to_hist(self) -> Hist {
        let types = &self.bytes[..FileType::COUNT];
        let ages = &self.bytes[FileType::COUNT..];
        let total: u128 = types.iter().map(|&b| b as u128).sum();
        if total == 0 {
            return Hist::EMPTY;
        }
        let mut h = Hist::EMPTY;
        h.type_pct = percentages(types);
        // Tipo dominante: ESTRICTAMENTE más de la mitad de los bytes.
        for (i, &b) in types.iter().enumerate() {
            if (b as u128) * 2 > total {
                h.dominant_type = i as u8;
            }
        }
        // Antigüedad, sobre los bytes de fecha conocida (mtime != 0): el tramo más antiguo tal que >= 80 % de esos
        // bytes estén en él o en uno más antiguo.
        let known: u128 = ages.iter().map(|&b| b as u128).sum();
        if known > 0 {
            h.age_pct = percentages(ages);
            let mut cum = 0u128;
            for k in (0..AGE_BUCKETS).rev() {
                cum += ages[k] as u128;
                if cum * 5 >= known * 4 {
                    h.dominant_age = k as u8;
                    break;
                }
            }
        }
        h
    }
}

/// Reparto en porcentajes enteros que suman exactamente 100 (método del mayor resto).
fn percentages<const N: usize>(v: &[u64]) -> [u8; N] {
    let total: u128 = v.iter().map(|&x| x as u128).sum();
    let mut out = [0u8; N];
    if total == 0 {
        return out;
    }
    let mut rem: Vec<(u128, usize)> = Vec::with_capacity(N);
    let mut assigned = 0u32;
    for (i, &x) in v.iter().enumerate() {
        let scaled = x as u128 * 100;
        out[i] = (scaled / total) as u8;
        assigned += out[i] as u32;
        rem.push((scaled % total, i));
    }
    rem.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for &(_, i) in rem.iter().take((100 - assigned) as usize) {
        out[i] += 1;
    }
    out
}

/// Deduplica enlaces duros: en cada grupo con el mismo `FileId` conserva el tamaño el enlace de ruta completa menor
/// (bytes WTF-8 de la ruta desde la raíz, con `\`); los demás pasan a `alloc = logical = 0` con el flag `HARDLINK`.
fn dedup_hard_links(
    recs: &mut [Record],
    names: &[u8],
    link_lo: &mut [(u64, u32)],
    link_hi: &FxHashMap<u32, u64>,
    stats: &mut TreeStats,
) {
    link_lo.sort_unstable();
    let mut i = 0;
    while i < link_lo.len() {
        let mut j = i + 1;
        while j < link_lo.len() && link_lo[j].0 == link_lo[i].0 {
            j += 1;
        }
        if j - i >= 2 {
            // Mismo FileId de 64 bits bajos: se separa por la mitad alta (casi siempre 0, salvo ReFS).
            let mut group: Vec<(u64, u32)> = link_lo[i..j]
                .iter()
                .map(|&(_, id)| (link_hi.get(&id).copied().unwrap_or(0), id))
                .collect();
            group.sort_unstable();
            let mut a = 0;
            while a < group.len() {
                let mut b = a + 1;
                while b < group.len() && group[b].0 == group[a].0 {
                    b += 1;
                }
                if b - a >= 2 {
                    resolve_group(recs, names, group[a..b].iter().map(|&(_, id)| id), stats);
                }
                a = b;
            }
        }
        i = j;
    }
}

fn resolve_group(
    recs: &mut [Record],
    names: &[u8],
    ids: impl Iterator<Item = u32>,
    stats: &mut TreeStats,
) {
    let mut members: Vec<(Vec<u8>, u32)> = ids.map(|id| (path_key(recs, names, id), id)).collect();
    members.sort_unstable();
    stats.hardlink_groups += 1;
    for (_, id) in members.iter().skip(1) {
        let r = &mut recs[*id as usize];
        stats.hardlink_extra_entries += 1;
        stats.hardlink_alloc_saved += r.alloc;
        stats.hardlink_logical_saved += r.logical;
        r.alloc = 0;
        r.logical = 0;
        r.flags |= flags::HARDLINK;
    }
}

/// Ruta de `id` desde la raíz (sin el nombre de la raíz), con `\`, en bytes WTF-8. Solo se llama para los
/// miembros de grupos de colisión, que son pocos.
fn path_key(recs: &[Record], names: &[u8], id: u32) -> Vec<u8> {
    let mut parts: Vec<&[u8]> = Vec::new();
    let mut cur = id as usize;
    while cur != 0 && cur < recs.len() && recs[cur].is_present() && parts.len() < 65_536 {
        parts.push(name_of(names, &recs[cur]));
        cur = recs[cur].parent as usize;
    }
    let mut out = Vec::new();
    for (k, part) in parts.iter().rev().enumerate() {
        if k > 0 {
            out.push(b'\\');
        }
        out.extend_from_slice(part);
    }
    out
}

#[cfg(test)]
mod tests;
