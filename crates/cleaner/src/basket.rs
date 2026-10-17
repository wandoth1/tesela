//! The basket (PLAN.md §2.5 and §2.7): the normalized set of items selected by the user.
//!
//! Ancestor normalization: adding an item whose ancestor is already in the basket does not duplicate it; adding an
//! ancestor absorbs its descendants. "Contents of X" covers items under X (not X itself). The basket stores only
//! what the user requested (paths and categories); approval (`plan`) decides what is actually deleted and rechecks
//! protection by handle.

use crate::paths::PathKey;
use crate::protect::{Protection, Verdict};
use crate::rules::CategoryId;
use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// What the user requested.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A rule-based category (selected in Recommendations).
    Category(CategoryId),
    /// "Your file": any file or folder. Review risk, Recycle Bin destination.
    Path(PathBuf),
    /// "Contents of X": items inside a known folder, excluding the folder itself.
    ContentsOf(PathBuf),
    /// Empty the Recycle Bin (scope enumerated during approval).
    EmptyBin,
}

/// Result of adding a path to the basket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddOutcome {
    /// Added; `absorbed` earlier entries (descendants) are now covered by the new entry.
    Added { absorbed: usize },
    /// Already covered by an ancestor (or identical entry) in the basket; no duplicate is added.
    AlreadyCovered,
    /// Cannot be added (untouchable, denied, or a known folder that only accepts "Contents of").
    Rejected(Verdict),
    /// Not a local drive-letter path (UNC, including a local disk reached through its administrative
    /// share): `cleaner` only cleans local drives and reports this when adding the item, not just at approval.
    NotLocal,
}

#[derive(Clone, Debug)]
struct Entry {
    source: Source,
    key: Option<PathKey>,
}

/// What to add in a batch ([`Basket::add_batch`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BatchKind {
    /// Same as [`Basket::add_path`].
    Path,
    /// Same as [`Basket::add_contents_of`].
    ContentsOf,
}

/// Global basket version counter: every basket modification gets a new number, so two baskets
/// (or a replaced basket) never share a version unless both are empty and untouched.
static REV: AtomicU64 = AtomicU64::new(1);

/// Does either index cover path `key`? A path entry covers itself and everything beneath it;
/// "Contents of X" covers items beneath X (not X itself). Walk ancestors (O(depth) index lookups).
fn covered_by(paths: &BTreeSet<PathKey>, contents: &BTreeSet<PathKey>, key: &PathKey) -> bool {
    if paths.contains(key) {
        return true;
    }
    let mut cur = key.parent();
    while let Some(p) = cur {
        if paths.contains(&p) || contents.contains(&p) {
            return true;
        }
        cur = p.parent();
    }
    false
}

/// Keys in `set` under `key` (excluding `key` itself): they are contiguous in index order, starting at `key\`.
fn below<'a>(
    set: &'a BTreeSet<PathKey>,
    key: &'a PathKey,
) -> impl Iterator<Item = &'a PathKey> + 'a {
    let lo = key.join("");
    set.range(lo..).take_while(move |k| k.is_same_or_under(key))
}

/// The basket.
///
/// Besides the entries in insertion order, this stores two key-sorted indexes: one for paths
/// and one for "Contents of". This makes `covers`, descendant absorption, and batch additions cost
/// O(depth · log n) instead of scanning the entire basket, which was quadratic with tens of thousands of entries.
#[derive(Clone, Debug, Default)]
pub struct Basket {
    entries: Vec<Entry>,
    paths: BTreeSet<PathKey>,
    contents: BTreeSet<PathKey>,
    rev: u64,
}

impl Basket {
    pub fn new() -> Basket {
        Basket::default()
    }

    fn touch(&mut self) {
        self.rev = REV.fetch_add(1, Ordering::Relaxed);
    }

    /// Basket version: changes on every modification and is unique across baskets. The interface uses it as a cache key for
    /// estimates and selection summaries instead of traversing the basket on every frame.
    pub fn rev(&self) -> u64 {
        self.rev
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.paths.clear();
        self.contents.clear();
        self.touch();
    }

    /// Entries, in the order they were added.
    pub fn sources(&self) -> impl Iterator<Item = &Source> {
        self.entries.iter().map(|e| &e.source)
    }

    pub fn contains_category(&self, id: CategoryId) -> bool {
        self.entries
            .iter()
            .any(|e| e.source == Source::Category(id))
    }

    pub fn has_empty_bin(&self) -> bool {
        self.entries.iter().any(|e| e.source == Source::EmptyBin)
    }

    /// Select or deselect a category.
    pub fn set_category(&mut self, id: CategoryId, on: bool) {
        self.entries.retain(|e| e.source != Source::Category(id));
        if on {
            self.entries.push(Entry {
                source: Source::Category(id),
                key: None,
            });
        }
        self.touch();
    }

    /// Select or deselect "empty the Recycle Bin".
    pub fn set_empty_bin(&mut self, on: bool) {
        self.entries.retain(|e| e.source != Source::EmptyBin);
        if on {
            self.entries.push(Entry {
                source: Source::EmptyBin,
                key: None,
            });
        }
        self.touch();
    }

    /// Remove an exact entry.
    pub fn remove(&mut self, source: &Source) {
        let before = self.entries.len();
        self.entries.retain(|e| &e.source != source);
        if self.entries.len() != before {
            match source {
                Source::Path(p) => {
                    self.paths.remove(&PathKey::new(p));
                }
                Source::ContentsOf(p) => {
                    self.contents.remove(&PathKey::new(p));
                }
                _ => {}
            }
            self.touch();
        }
    }

    /// Is this path covered by the basket (equal to an entry, or under an added folder or its contents)?
    pub fn covers(&self, key: &PathKey) -> bool {
        covered_by(&self.paths, &self.contents, key)
    }

    /// Is there an entry of its own (a path or "Contents of") exactly at `key` that can be removed? Unlike
    /// [`Basket::covers`], this does not check ancestors. O(log n) with the indexes.
    pub fn has_own(&self, key: &PathKey) -> bool {
        self.paths.contains(key) || self.contents.contains(key)
    }

    /// Is "Contents of X" in the basket for folder `key`?
    pub fn has_contents_of(&self, key: &PathKey) -> bool {
        self.contents.contains(key)
    }

    /// Quita de la cesta las entradas (rutas y "Contenido de") cuya clave está en `gone` y devuelve cuántas.
    fn drop_keys(&mut self, gone: &HashSet<PathKey>) -> usize {
        if gone.is_empty() {
            return 0;
        }
        let before = self.entries.len();
        self.entries
            .retain(|e| e.key.as_ref().is_none_or(|k| !gone.contains(k)));
        for k in gone {
            self.paths.remove(k);
            self.contents.remove(k);
        }
        before - self.entries.len()
    }

    /// Añade un archivo o una carpeta ("Tu archivo"). Una carpeta conocida solo se añade con
    /// [`Basket::add_contents_of`]; los intocables y los denegados rebotan.
    pub fn add_path(&mut self, path: PathBuf, prot: &Protection) -> AddOutcome {
        let key = PathKey::new(&path);
        if !key.is_drive_path() {
            return AddOutcome::NotLocal;
        }
        match prot.target_verdict(&key) {
            Verdict::Allowed => {}
            other => return AddOutcome::Rejected(other),
        }
        if self.covers(&key) {
            return AddOutcome::AlreadyCovered;
        }
        // El nuevo ancestro absorbe a lo que cuelga de él (rutas sueltas y contenidos de carpetas dentro de él).
        let gone: HashSet<PathKey> = below(&self.paths, &key)
            .chain(below(&self.contents, &key))
            .cloned()
            .collect();
        let absorbed = self.drop_keys(&gone);
        self.paths.insert(key.clone());
        self.entries.push(Entry {
            source: Source::Path(path),
            key: Some(key),
        });
        self.touch();
        AddOutcome::Added { absorbed }
    }

    /// Añade "Contenido de X" para una carpeta conocida (`Verdict::ContentsOnly`).
    pub fn add_contents_of(&mut self, path: PathBuf, prot: &Protection) -> AddOutcome {
        let key = PathKey::new(&path);
        if !key.is_drive_path() {
            return AddOutcome::NotLocal;
        }
        match prot.target_verdict(&key) {
            Verdict::ContentsOnly => {}
            other => return AddOutcome::Rejected(other),
        }
        if self.covers(&key) || self.contents.contains(&key) {
            return AddOutcome::AlreadyCovered;
        }
        // Lo que cuelga de la carpeta (rutas sueltas) queda cubierto por su contenido.
        let gone: HashSet<PathKey> = below(&self.paths, &key)
            .chain(below(&self.contents, &key))
            .cloned()
            .collect();
        let absorbed = self.drop_keys(&gone);
        self.contents.insert(key.clone());
        self.entries.push(Entry {
            source: Source::ContentsOf(path),
            key: Some(key),
        });
        self.touch();
        AddOutcome::Added { absorbed }
    }

    /// Add many paths at once, using the SAME checks as [`Basket::add_path`] and
    /// [`Basket::add_contents_of`] for each path (local drive path, protection verdict, and existing coverage), with the
    /// same result as adding them one by one in order, but in one pass: query indexes by key and
    /// absorb existing entries once at the end. Returns one result per item, in the same order.
    ///
    /// Batch items need not be siblings, and the final basket is the same regardless of order. The only
    /// difference from adding them one by one is the INDIVIDUAL result for an item under another item in the same batch: it returns
    /// `AlreadyCovered` even if it appears before its ancestor (one by one it would return `Added` and later be absorbed), because
    /// the basket never stores both an item and its ancestor.
    pub fn add_batch(
        &mut self,
        items: Vec<(PathBuf, BatchKind)>,
        prot: &Protection,
    ) -> Vec<AddOutcome> {
        let mut out: Vec<AddOutcome> = Vec::with_capacity(items.len());
        // (índice del resultado, ruta, clave, tipo) de lo que pasa las comprobaciones contra la cesta anterior.
        let mut cands: Vec<(usize, PathBuf, PathKey, BatchKind)> = Vec::new();
        let mut seen: HashSet<(BatchKind, PathKey)> = HashSet::new();
        for (path, kind) in items {
            let key = PathKey::new(&path);
            let idx = out.len();
            if !key.is_drive_path() {
                out.push(AddOutcome::NotLocal);
                continue;
            }
            let wanted = match kind {
                BatchKind::Path => Verdict::Allowed,
                BatchKind::ContentsOf => Verdict::ContentsOnly,
            };
            let verdict = prot.target_verdict(&key);
            if verdict != wanted {
                out.push(AddOutcome::Rejected(verdict));
                continue;
            }
            let dup_contents = kind == BatchKind::ContentsOf && self.contents.contains(&key);
            if self.covers(&key) || dup_contents || !seen.insert((kind, key.clone())) {
                out.push(AddOutcome::AlreadyCovered);
                continue;
            }
            out.push(AddOutcome::Added { absorbed: 0 });
            cands.push((idx, path, key, kind));
        }
        // Entre sí: lo que cuelga de otro elemento del mismo lote (o de su contenido) ya está cubierto por él.
        let new_paths: BTreeSet<PathKey> = cands
            .iter()
            .filter(|c| c.3 == BatchKind::Path)
            .map(|c| c.2.clone())
            .collect();
        let new_contents: BTreeSet<PathKey> = cands
            .iter()
            .filter(|c| c.3 == BatchKind::ContentsOf)
            .map(|c| c.2.clone())
            .collect();
        let mut keep: Vec<(PathBuf, PathKey, BatchKind)> = Vec::with_capacity(cands.len());
        for (idx, path, key, kind) in cands {
            let mut cur = key.parent();
            let mut nested = false;
            while let Some(p) = cur {
                if new_paths.contains(&p) || new_contents.contains(&p) {
                    nested = true;
                    break;
                }
                cur = p.parent();
            }
            if nested {
                out[idx] = AddOutcome::AlreadyCovered;
            } else {
                keep.push((path, key, kind));
            }
        }
        // Lo que ya había y cuelga de lo nuevo queda absorbido (una consulta por rango por elemento).
        let mut gone: HashSet<PathKey> = HashSet::new();
        for (_, key, _) in &keep {
            gone.extend(below(&self.paths, key).cloned());
            gone.extend(below(&self.contents, key).cloned());
        }
        self.drop_keys(&gone);
        for (path, key, kind) in keep {
            match kind {
                BatchKind::Path => {
                    self.paths.insert(key.clone());
                    self.entries.push(Entry {
                        source: Source::Path(path),
                        key: Some(key),
                    });
                }
                BatchKind::ContentsOf => {
                    self.contents.insert(key.clone());
                    self.entries.push(Entry {
                        source: Source::ContentsOf(path),
                        key: Some(key),
                    });
                }
            }
        }
        self.touch();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protect::ProtectionRoots;

    fn prot() -> Protection {
        Protection::new(&ProtectionRoots {
            denied: vec![PathBuf::from(r"C:\Windows")],
            untouchable: vec![PathBuf::from(r"C:\Users\<usuario>")],
            content_roots: vec![PathBuf::from(r"C:\Users\<usuario>\Videos")],
            ..ProtectionRoots::default()
        })
    }

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn un_ancestro_absorbe_a_sus_descendientes_y_un_descendiente_no_se_duplica() {
        let pr = prot();
        let mut b = Basket::new();
        assert_eq!(
            b.add_path(p(r"C:\datos\a\x.bin"), &pr),
            AddOutcome::Added { absorbed: 0 }
        );
        assert_eq!(
            b.add_path(p(r"C:\datos\a\y.bin"), &pr),
            AddOutcome::Added { absorbed: 0 }
        );
        assert_eq!(
            b.add_path(p(r"C:\datos\b"), &pr),
            AddOutcome::Added { absorbed: 0 }
        );
        assert_eq!(b.len(), 3);
        // El ancestro `a` absorbe a los dos archivos.
        assert_eq!(
            b.add_path(p(r"C:\datos\a"), &pr),
            AddOutcome::Added { absorbed: 2 }
        );
        assert_eq!(b.len(), 2);
        // Un descendiente de `a` ya está cubierto; igual que el propio `a`.
        assert_eq!(
            b.add_path(p(r"C:\datos\a\z.bin"), &pr),
            AddOutcome::AlreadyCovered
        );
        assert_eq!(
            b.add_path(p(r"C:\DATOS\A"), &pr),
            AddOutcome::AlreadyCovered
        );
        assert_eq!(
            b.add_path(p(r"c:/datos/a/sub/deep.bin"), &pr),
            AddOutcome::AlreadyCovered
        );
        // Un hermano con prefijo de nombre parecido no se confunde.
        assert_eq!(
            b.add_path(p(r"C:\datos\ab"), &pr),
            AddOutcome::Added { absorbed: 0 }
        );
        assert_eq!(b.len(), 3);
        // El padre de todo absorbe todo.
        assert_eq!(
            b.add_path(p(r"C:\datos"), &pr),
            AddOutcome::Added { absorbed: 3 }
        );
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn los_intocables_denegados_y_carpetas_conocidas_rebotan() {
        let pr = prot();
        let mut b = Basket::new();
        assert_eq!(
            b.add_path(p(r"C:\Windows\System32\x.dll"), &pr),
            AddOutcome::Rejected(Verdict::Denied)
        );
        assert_eq!(
            b.add_path(p(r"C:\"), &pr),
            AddOutcome::Rejected(Verdict::Untouchable)
        );
        assert_eq!(
            b.add_path(p(r"C:\Users\<usuario>"), &pr),
            AddOutcome::Rejected(Verdict::Untouchable)
        );
        assert_eq!(
            b.add_path(p(r"C:\Users\<usuario>\Videos"), &pr),
            AddOutcome::Rejected(Verdict::ContentsOnly)
        );
        assert!(b.is_empty());
        // Su contenido sí.
        assert_eq!(
            b.add_path(p(r"C:\Users\<usuario>\Videos\viaje"), &pr),
            AddOutcome::Added { absorbed: 0 }
        );
    }

    #[test]
    fn las_rutas_de_red_se_rechazan_al_anadir_con_su_motivo() {
        let pr = prot();
        let mut b = Basket::new();
        let sep = '\\';
        // Hacia un disco del propio equipo por su compartición administrativa, por IP, por un servidor y en forma verbatim.
        for host in ["localhost", "127.0.0.1", "servidor"] {
            for q in [
                format!("{sep}{sep}{host}{sep}c${sep}datos{sep}x.bin"),
                format!("{sep}{sep}?{sep}UNC{sep}{host}{sep}c${sep}datos{sep}x.bin"),
            ] {
                assert_eq!(b.add_path(p(&q), &pr), AddOutcome::NotLocal, "{q}");
                assert_eq!(b.add_contents_of(p(&q), &pr), AddOutcome::NotLocal, "{q}");
            }
        }
        // Un volumen sin letra y una ruta relativa tampoco.
        assert_eq!(
            b.add_path(
                p(r"\\?\Volume{00000000-0000-0000-0000-000000000000}\x"),
                &pr
            ),
            AddOutcome::NotLocal
        );
        assert_eq!(b.add_path(p(r"datos\x.bin"), &pr), AddOutcome::NotLocal);
        assert!(b.is_empty());
        // La misma ruta con letra de unidad sí se acepta (y la forma verbatim con letra también).
        assert_eq!(
            b.add_path(p(r"C:\datos\x.bin"), &pr),
            AddOutcome::Added { absorbed: 0 }
        );
        assert_eq!(
            b.add_path(p(r"\\?\C:\datos\y.bin"), &pr),
            AddOutcome::Added { absorbed: 0 }
        );
    }

    #[test]
    fn contenido_de_una_carpeta_conocida() {
        let pr = prot();
        let mut b = Basket::new();
        b.add_path(p(r"C:\Users\<usuario>\Videos\viaje"), &pr);
        b.add_path(p(r"C:\Users\<usuario>\Videos\fiesta.mp4"), &pr);
        // "Contenido de Videos" absorbe lo suelto de dentro y cubre lo que se añada después.
        assert_eq!(
            b.add_contents_of(p(r"C:\Users\<usuario>\Videos"), &pr),
            AddOutcome::Added { absorbed: 2 }
        );
        assert_eq!(
            b.add_path(p(r"C:\Users\<usuario>\Videos\otro.mkv"), &pr),
            AddOutcome::AlreadyCovered
        );
        assert_eq!(
            b.add_contents_of(p(r"C:\Users\<usuario>\Videos"), &pr),
            AddOutcome::AlreadyCovered
        );
        assert_eq!(b.len(), 1);
        // Solo las carpetas conocidas admiten "Contenido de".
        assert_eq!(
            b.add_contents_of(p(r"C:\datos"), &pr),
            AddOutcome::Rejected(Verdict::Allowed)
        );
        assert_eq!(
            b.add_contents_of(p(r"C:\Windows"), &pr),
            AddOutcome::Rejected(Verdict::Denied)
        );
    }

    #[test]
    fn categorias_y_vaciar_papelera() {
        let mut b = Basket::new();
        b.set_category(CategoryId::UserTemp, true);
        b.set_category(CategoryId::UserTemp, true);
        b.set_empty_bin(true);
        b.set_empty_bin(true);
        assert_eq!(b.len(), 2);
        assert!(b.contains_category(CategoryId::UserTemp) && b.has_empty_bin());
        b.set_category(CategoryId::UserTemp, false);
        b.set_empty_bin(false);
        assert!(b.is_empty());
    }

    /// El lote da el mismo resultado que añadir uno a uno en orden: mismas comprobaciones de protección por elemento,
    /// misma absorción de lo que ya había y misma lista final.
    #[test]
    fn un_lote_equivale_a_anadir_uno_a_uno() {
        let pr = prot();
        let sep = '\\';
        let seed = |b: &mut Basket| {
            b.add_path(p(r"C:\datos\a\x.bin"), &pr);
            b.add_path(p(r"C:\datos\b\y.bin"), &pr);
            b.add_path(p(r"C:\otro\z.bin"), &pr);
            b.add_contents_of(p(r"C:\Users\<usuario>\Videos"), &pr);
        };
        let items: Vec<(PathBuf, BatchKind)> = vec![
            (p(r"C:\datos\a"), BatchKind::Path),
            (p(r"C:\datos\c"), BatchKind::Path),
            (p(r"C:\datos\c"), BatchKind::Path),
            (p(r"C:\otro\z.bin"), BatchKind::Path),
            (p(r"C:\Windows\x.dll"), BatchKind::Path),
            (p(r"C:\Users\<usuario>"), BatchKind::Path),
            (p(r"C:\Users\<usuario>\Videos"), BatchKind::Path),
            (p(r"C:\Users\<usuario>\Videos"), BatchKind::ContentsOf),
            (p(r"C:\datos"), BatchKind::ContentsOf),
            (p(&format!("{sep}{sep}srv{sep}c${sep}x")), BatchKind::Path),
            (p(r"C:\Users\<usuario>\Videos\viaje"), BatchKind::Path),
            // Un descendiente ANTES que su ancestro en el mismo lote: uno a uno entra y luego lo absorbe el ancestro.
            (p(r"C:\datos\d\x.bin"), BatchKind::Path),
            (p(r"C:\datos\d"), BatchKind::Path),
        ];
        let nested_first = items.len() - 2;
        let mut one = Basket::new();
        seed(&mut one);
        let expect: Vec<AddOutcome> = items
            .iter()
            .map(|(path, kind)| match kind {
                BatchKind::Path => one.add_path(path.clone(), &pr),
                BatchKind::ContentsOf => one.add_contents_of(path.clone(), &pr),
            })
            .collect();
        let mut batch = Basket::new();
        seed(&mut batch);
        let rev = batch.rev();
        let got = batch.add_batch(items, &pr);
        assert_ne!(batch.rev(), rev, "el lote cambia la versión");
        // Mismos rechazos y mismos "ya cubierta"; en lo añadido solo puede variar el recuento de absorbidos.
        assert_eq!(got[nested_first], AddOutcome::AlreadyCovered);
        assert!(matches!(expect[nested_first], AddOutcome::Added { .. }));
        for (i, (g, e)) in got.iter().zip(&expect).enumerate() {
            if i == nested_first {
                continue;
            }
            assert_eq!(
                std::mem::discriminant(g),
                std::mem::discriminant(e),
                "{got:?} frente a {expect:?}"
            );
            if let (AddOutcome::Rejected(a), AddOutcome::Rejected(b)) = (g, e) {
                assert_eq!(a, b);
            }
        }
        let list = |b: &Basket| -> Vec<String> {
            let mut v: Vec<String> = b.sources().map(|s| format!("{s:?}")).collect();
            v.sort();
            v
        };
        assert_eq!(list(&batch), list(&one));
    }

    #[test]
    fn en_un_lote_lo_que_cuelga_de_otro_elemento_del_lote_queda_cubierto_sea_cual_sea_el_orden() {
        let pr = prot();
        for order in [[0usize, 1], [1, 0]] {
            let items = [
                (p(r"C:\datos\a"), BatchKind::Path),
                (p(r"C:\datos\a\b\c.bin"), BatchKind::Path),
            ];
            let mut b = Basket::new();
            let got = b.add_batch(order.iter().map(|&i| items[i].clone()).collect(), &pr);
            assert_eq!(b.len(), 1, "{order:?} {got:?}");
            assert!(b.covers(&PathKey::new(&p(r"C:\datos\a\b\c.bin"))));
        }
    }

    #[test]
    fn los_indices_siguen_a_las_altas_las_bajas_y_el_vaciado() {
        let pr = prot();
        let mut b = Basket::new();
        let (r0, a) = (b.rev(), PathKey::new(&p(r"C:\datos\a")));
        b.add_path(p(r"C:\datos\a\x.bin"), &pr);
        assert!(b.covers(&PathKey::new(&p(r"C:\datos\a\x.bin"))));
        assert!(!b.covers(&a), "el padre de una entrada no está cubierto");
        assert_ne!(b.rev(), r0);
        b.remove(&Source::Path(p(r"C:\datos\a\x.bin")));
        assert!(!b.covers(&PathKey::new(&p(r"C:\datos\a\x.bin"))));
        b.add_contents_of(p(r"C:\Users\<usuario>\Videos"), &pr);
        let v = PathKey::new(&p(r"C:\Users\<usuario>\Videos"));
        assert!(b.has_contents_of(&v) && !b.covers(&v));
        assert!(b.has_own(&v) && !b.has_own(&v.join("viaje")));
        b.add_path(p(r"C:\datos\a\x.bin"), &pr);
        let x = PathKey::new(&p(r"C:\datos\a\x.bin"));
        assert!(b.has_own(&x), "una entrada de ruta es propia");
        assert!(!b.has_own(&PathKey::new(&p(r"C:\datos\a\x.bin\hijo"))));
        b.remove(&Source::Path(p(r"C:\datos\a\x.bin")));
        assert!(!b.has_own(&x));
        assert!(b.covers(&v.join("viaje")));
        b.clear();
        assert!(!b.has_contents_of(&v) && !b.covers(&v.join("viaje")));
        // Dos cestas distintas no comparten versión aunque hagan lo mismo.
        let (mut x, mut y) = (Basket::new(), Basket::new());
        x.add_path(p(r"C:\datos\q"), &pr);
        y.add_path(p(r"C:\datos\q"), &pr);
        assert_ne!(x.rev(), y.rev());
    }

    /// Escala: 5 000 hermanos en un lote y la cesta sigue respondiendo (con el recorrido lineal de antes eran segundos).
    #[test]
    fn cinco_mil_hermanos_en_un_lote_y_otro_lote_sobre_la_cesta_llena() {
        let pr = prot();
        let mut b = Basket::new();
        let items: Vec<(PathBuf, BatchKind)> = (0..5000)
            .map(|i| {
                (
                    p(&format!(r"C:\datos\carpeta\hijo-{i:05}")),
                    BatchKind::Path,
                )
            })
            .collect();
        let t = std::time::Instant::now();
        let r = b.add_batch(items.clone(), &pr);
        assert!(r.iter().all(|o| matches!(o, AddOutcome::Added { .. })));
        assert_eq!(b.len(), 5000);
        // Otra vez el mismo lote: todo cubierto; y un ancestro lo absorbe de una vez.
        let r = b.add_batch(items, &pr);
        assert!(r.iter().all(|o| *o == AddOutcome::AlreadyCovered));
        assert_eq!(
            b.add_path(p(r"C:\datos\carpeta"), &pr),
            AddOutcome::Added { absorbed: 5000 }
        );
        assert_eq!(b.len(), 1);
        assert!(
            t.elapsed() < std::time::Duration::from_secs(5),
            "tardó {:?}",
            t.elapsed()
        );
    }
}
