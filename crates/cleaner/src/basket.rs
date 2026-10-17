//! La cesta (PLAN.md §2.5 y §2.7): lo que el usuario ha marcado, normalizado.
//!
//! Normalización por ancestros: si se añade un elemento cuyo ancestro ya está en la cesta, no se duplica; si se añade un
//! ancestro, absorbe a sus descendientes. "Contenido de X" cubre lo que cuelga de X (no a X). La cesta guarda solo lo
//! que el usuario pidió (rutas y categorías); lo que de verdad se borra lo decide la aprobación (`plan`), que vuelve a
//! comprobar la protección por handle.

use crate::paths::PathKey;
use crate::protect::{Protection, Verdict};
use crate::rules::CategoryId;
use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Qué ha pedido el usuario.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Una categoría con regla (se marca en Recomendaciones).
    Category(CategoryId),
    /// "Tu archivo": un archivo o una carpeta cualquiera. Riesgo Revisar, destino Papelera.
    Path(PathBuf),
    /// "Contenido de X": lo que hay dentro de una carpeta conocida, sin la carpeta en sí.
    ContentsOf(PathBuf),
    /// Vaciar la Papelera (alcance enumerado al aprobar).
    EmptyBin,
}

/// Resultado de añadir una ruta a la cesta.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddOutcome {
    /// Se añadió; `absorbed` entradas anteriores (descendientes) quedaron cubiertas por la nueva.
    Added { absorbed: usize },
    /// Ya estaba cubierta por un ancestro (o igual) de la cesta: no se duplica.
    AlreadyCovered,
    /// No se puede añadir (intocable, denegada, o es una carpeta conocida que solo admite "Contenido de").
    Rejected(Verdict),
    /// No es una ruta de una unidad local con letra (UNC, también hacia un disco propio por su compartición
    /// administrativa): el `cleaner` solo limpia unidades locales y lo dice ya al añadir, no solo al aprobar.
    NotLocal,
}

#[derive(Clone, Debug)]
struct Entry {
    source: Source,
    key: Option<PathKey>,
}

/// Qué se añade en un lote ([`Basket::add_batch`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BatchKind {
    /// Como [`Basket::add_path`].
    Path,
    /// Como [`Basket::add_contents_of`].
    ContentsOf,
}

/// Contador global de versiones de cestas: cada modificación de cualquier cesta toma un número nuevo, así que dos cestas
/// distintas (o una sustituida por otra) nunca comparten versión salvo estando vacías y sin tocar.
static REV: AtomicU64 = AtomicU64::new(1);

/// ¿Cubre algo de este par de índices la ruta `key`? Una entrada de ruta cubre esa ruta y todo lo que cuelga de ella;
/// "Contenido de X" cubre lo que cuelga de X (no a X). Sube por los ancestros (O(profundidad) consultas al índice).
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

/// Las claves de `set` que cuelgan de `key` (sin incluirla): son contiguas en el orden del índice, a partir de `key\`.
fn below<'a>(
    set: &'a BTreeSet<PathKey>,
    key: &'a PathKey,
) -> impl Iterator<Item = &'a PathKey> + 'a {
    let lo = key.join("");
    set.range(lo..).take_while(move |k| k.is_same_or_under(key))
}

/// La cesta.
///
/// Además de la lista de entradas (en el orden en que se añadieron) lleva dos índices ordenados por clave, el de las rutas
/// y el de los "Contenido de", para que `covers`, la absorción de descendientes y las altas por lotes cuesten
/// O(profundidad · log n) y no recorran la cesta entera: con decenas de miles de entradas lo segundo era cuadrático.
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

    /// Versión de la cesta: cambia con cada modificación y no se repite entre cestas. La interfaz la usa como clave de
    /// sus cachés (estimaciones, resumen de la selección) en vez de recorrer la cesta en cada fotograma.
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

    /// Las entradas, en el orden en que se añadieron.
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

    /// Marca o desmarca una categoría.
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

    /// Marca o desmarca "vaciar la Papelera".
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

    /// Quita una entrada exacta.
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

    /// ¿Está esta ruta cubierta por la cesta (igual a una entrada, o dentro de una carpeta añadida o del contenido de una)?
    pub fn covers(&self, key: &PathKey) -> bool {
        covered_by(&self.paths, &self.contents, key)
    }

    /// ¿Está "Contenido de X" en la cesta para la carpeta `key`?
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

    /// Añade muchas rutas de una vez, con las MISMAS comprobaciones que [`Basket::add_path`] y
    /// [`Basket::add_contents_of`] para cada una (ruta de unidad local, veredicto de la protección, ya cubierta) y el
    /// mismo resultado que añadirlas una a una en orden, pero en una sola pasada: los índices se consultan por clave y la
    /// absorción de lo que ya había se hace al final, una vez. Devuelve un resultado por elemento, en el mismo orden.
    ///
    /// No hace falta que los elementos del lote sean hermanos: si uno cuelga de otro del mismo lote, queda como
    /// `AlreadyCovered` (la cesta nunca guarda un elemento y su ancestro).
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
        ];
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
        for (g, e) in got.iter().zip(&expect) {
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
