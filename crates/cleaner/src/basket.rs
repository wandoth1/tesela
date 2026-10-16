//! La cesta (PLAN.md §2.5 y §2.7): lo que el usuario ha marcado, normalizado.
//!
//! Normalización por ancestros: si se añade un elemento cuyo ancestro ya está en la cesta, no se duplica; si se añade un
//! ancestro, absorbe a sus descendientes. "Contenido de X" cubre lo que cuelga de X (no a X). La cesta guarda solo lo
//! que el usuario pidió (rutas y categorías); lo que de verdad se borra lo decide la aprobación (`plan`), que vuelve a
//! comprobar la protección por handle.

use crate::paths::PathKey;
use crate::protect::{Protection, Verdict};
use crate::rules::CategoryId;
use std::path::PathBuf;

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
}

#[derive(Clone, Debug)]
struct Entry {
    source: Source,
    key: Option<PathKey>,
}

/// La cesta.
#[derive(Clone, Debug, Default)]
pub struct Basket {
    entries: Vec<Entry>,
}

impl Basket {
    pub fn new() -> Basket {
        Basket::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
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
    }

    /// Quita una entrada exacta.
    pub fn remove(&mut self, source: &Source) {
        self.entries.retain(|e| &e.source != source);
    }

    /// ¿Está esta ruta cubierta por la cesta (igual a una entrada, o dentro de una carpeta añadida o del contenido de una)?
    pub fn covers(&self, key: &PathKey) -> bool {
        self.entries.iter().any(|e| match (&e.source, &e.key) {
            (Source::Path(_), Some(k)) => key.is_same_or_under(k),
            (Source::ContentsOf(_), Some(k)) => k.is_proper_ancestor_of(key),
            _ => false,
        })
    }

    /// ¿Está "Contenido de X" en la cesta para la carpeta `key`?
    pub fn has_contents_of(&self, key: &PathKey) -> bool {
        self.entries
            .iter()
            .any(|e| matches!(e.source, Source::ContentsOf(_)) && e.key.as_ref() == Some(key))
    }

    /// Añade un archivo o una carpeta ("Tu archivo"). Una carpeta conocida solo se añade con
    /// [`Basket::add_contents_of`]; los intocables y los denegados rebotan.
    pub fn add_path(&mut self, path: PathBuf, prot: &Protection) -> AddOutcome {
        let key = PathKey::new(&path);
        match prot.target_verdict(&key) {
            Verdict::Allowed => {}
            other => return AddOutcome::Rejected(other),
        }
        if self.covers(&key) {
            return AddOutcome::AlreadyCovered;
        }
        let before = self.entries.len();
        // El nuevo ancestro absorbe a lo que cuelga de él (rutas sueltas y contenidos de carpetas dentro de él).
        self.entries.retain(|e| match (&e.source, &e.key) {
            (Source::Path(_) | Source::ContentsOf(_), Some(k)) => !k.is_same_or_under(&key),
            _ => true,
        });
        let absorbed = before - self.entries.len();
        self.entries.push(Entry {
            source: Source::Path(path),
            key: Some(key),
        });
        AddOutcome::Added { absorbed }
    }

    /// Añade "Contenido de X" para una carpeta conocida (`Verdict::ContentsOnly`).
    pub fn add_contents_of(&mut self, path: PathBuf, prot: &Protection) -> AddOutcome {
        let key = PathKey::new(&path);
        match prot.target_verdict(&key) {
            Verdict::ContentsOnly => {}
            other => return AddOutcome::Rejected(other),
        }
        if self.covers(&key)
            || self
                .entries
                .iter()
                .any(|e| e.key.as_ref() == Some(&key) && matches!(e.source, Source::ContentsOf(_)))
        {
            return AddOutcome::AlreadyCovered;
        }
        let before = self.entries.len();
        // Lo que cuelga de la carpeta (rutas sueltas) queda cubierto por su contenido.
        self.entries.retain(|e| match (&e.source, &e.key) {
            (Source::Path(_) | Source::ContentsOf(_), Some(k)) => !key.is_proper_ancestor_of(k),
            _ => true,
        });
        let absorbed = before - self.entries.len();
        self.entries.push(Entry {
            source: Source::ContentsOf(path),
            key: Some(key),
        });
        AddOutcome::Added { absorbed }
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
}
