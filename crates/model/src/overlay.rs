//! Overlay de limpieza (PLAN.md §3.6): el árbol cambia tras limpiar SIN reescanear y SIN copiar el `Tree`.
//!
//! * Un bitset de nodos eliminados (125 KB por millón de nodos).
//! * Un `FxHashMap<NodeId, Delta>` para los ancestros: al eliminar algo se resta su tamaño a lo largo de los ~30 ancestros
//!   como máximo.
//! * Tamaño visible = base − delta. El layout reordena los hijos visibles de los nodos tocados.
//! * Se descarta al reescanear (el árbol nuevo es otro).
//!
//! Invariante: cada ancestro de algo que cambió está en `delta`, así que un nodo que NO está en `delta` tiene exactamente
//! los hijos y el orden del árbol base. Los histogramas (tipos, antigüedad) no se recalculan: la interfaz los marca como
//! aproximados hasta reescanear.

use crate::tree::{NodeId, Tree};
use crate::{NO_PARENT, flags};
use rustc_hash::FxHashMap;
use std::ops::Range;

/// Lo que se ha restado a un nodo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Delta {
    pub alloc: u64,
    pub logical: u64,
    pub files: u64,
}

/// El overlay.
#[derive(Clone, Debug, Default)]
pub struct Overlay {
    removed: Vec<u64>,
    delta: FxHashMap<NodeId, Delta>,
    version: u64,
}

impl Overlay {
    /// Overlay vacío para un árbol de `tree.len()` nodos.
    pub fn new(tree: &Tree) -> Overlay {
        Overlay {
            removed: vec![0; tree.len().div_ceil(64)],
            delta: FxHashMap::default(),
            version: 0,
        }
    }

    /// Contador que cambia con cada modificación (entra en la clave de la caché del layout).
    pub fn version(&self) -> u64 {
        self.version
    }

    /// ¿Se ha modificado algo?
    pub fn is_empty(&self) -> bool {
        self.version == 0
    }

    fn bit(&self, id: NodeId) -> bool {
        self.removed
            .get(id as usize / 64)
            .is_some_and(|w| w >> (id % 64) & 1 != 0)
    }

    /// ¿Está marcado como eliminado (él mismo, no sus ancestros)?
    pub fn is_removed(&self, id: NodeId) -> bool {
        self.bit(id)
    }

    /// ¿Tiene algún cambio en sus hijos o en su tamaño (está en `delta`)?
    pub fn is_touched(&self, id: NodeId) -> bool {
        self.delta.contains_key(&id)
    }

    /// Lo restado a `id`.
    pub fn delta(&self, id: NodeId) -> Delta {
        self.delta.get(&id).copied().unwrap_or_default()
    }

    fn add_up(&mut self, tree: &Tree, from: NodeId, d: Delta, include_self: bool) {
        let mut cur = from;
        if !include_self {
            match tree.node(cur).parent {
                NO_PARENT => return,
                p => cur = p,
            }
        }
        loop {
            let e = self.delta.entry(cur).or_default();
            e.alloc = e.alloc.saturating_add(d.alloc);
            e.logical = e.logical.saturating_add(d.logical);
            e.files = e.files.saturating_add(d.files);
            // Nunca se resta más de lo que había.
            let n = tree.node(cur);
            e.alloc = e.alloc.min(n.alloc);
            e.logical = e.logical.min(n.logical);
            e.files = e.files.min(n.files_below as u64);
            match n.parent {
                NO_PARENT => break,
                p => cur = p,
            }
        }
    }

    /// Marca `id` (y con él todo lo que contiene) como eliminado y resta su tamaño visible a sus ancestros. Idempotente.
    /// No se puede eliminar la raíz.
    pub fn remove(&mut self, tree: &Tree, id: NodeId) {
        if id == 0 || id as usize >= tree.len() || self.bit(id) || !self.is_visible(tree, id) {
            return;
        }
        let n = tree.node(id);
        let d = Delta {
            alloc: self.alloc(tree, id),
            logical: self.logical(tree, id),
            files: if n.is_dir() {
                self.files_below(tree, id)
            } else {
                u64::from(n.flags & flags::SYSTEM_INFO == 0)
            },
        };
        if self.removed.len() < tree.len().div_ceil(64) {
            self.removed.resize(tree.len().div_ceil(64), 0);
        }
        self.removed[id as usize / 64] |= 1 << (id % 64);
        self.add_up(tree, id, d, false);
        self.version += 1;
    }

    /// Resta `alloc`, `logical` y `files` a `id` y a sus ancestros (limpieza parcial de una carpeta que se queda).
    pub fn shrink(&mut self, tree: &Tree, id: NodeId, alloc: u64, logical: u64, files: u64) {
        if id as usize >= tree.len() || !self.is_visible(tree, id) || (alloc | logical | files) == 0
        {
            return;
        }
        // No más de lo que le queda visible.
        let d = Delta {
            alloc: alloc.min(self.alloc(tree, id)),
            logical: logical.min(self.logical(tree, id)),
            files: files.min(self.files_below(tree, id)),
        };
        self.add_up(tree, id, d, true);
        self.version += 1;
    }

    /// ¿Es visible `id`? No lo está si él o alguno de sus ancestros se eliminó.
    pub fn is_visible(&self, tree: &Tree, id: NodeId) -> bool {
        let mut cur = id;
        loop {
            if self.bit(cur) {
                return false;
            }
            match tree.node(cur).parent {
                NO_PARENT => return true,
                p => cur = p,
            }
        }
    }

    /// Tamaño en disco visible.
    pub fn alloc(&self, tree: &Tree, id: NodeId) -> u64 {
        if self.bit(id) {
            return 0;
        }
        tree.node(id).alloc.saturating_sub(self.delta(id).alloc)
    }

    /// Tamaño lógico visible.
    pub fn logical(&self, tree: &Tree, id: NodeId) -> u64 {
        if self.bit(id) {
            return 0;
        }
        tree.node(id).logical.saturating_sub(self.delta(id).logical)
    }

    /// Archivos visibles por debajo.
    pub fn files_below(&self, tree: &Tree, id: NodeId) -> u64 {
        if self.bit(id) {
            return 0;
        }
        (tree.node(id).files_below as u64).saturating_sub(self.delta(id).files)
    }

    /// Bytes que el overlay ha restado de la raíz (lo escaneado ya no está en disco).
    pub fn removed_alloc(&self) -> u64 {
        self.delta(0).alloc
    }
}

/// Los hijos visibles de un directorio (ver [`View::kids`]).
#[derive(Clone, Debug)]
pub enum Kids {
    /// El rango contiguo del árbol base (nada cambió en este directorio).
    Range(Range<NodeId>),
    /// Los hijos que quedan, en orden de tamaño visible descendente (y por nombre a igual tamaño).
    List(Vec<NodeId>),
}

impl Kids {
    pub fn len(&self) -> usize {
        match self {
            Kids::Range(r) => r.len(),
            Kids::List(v) => v.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn get(&self, i: usize) -> NodeId {
        match self {
            Kids::Range(r) => r.start + i as u32,
            Kids::List(v) => v[i],
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = NodeId> + '_ {
        (0..self.len()).map(|i| self.get(i))
    }
}

/// Un árbol visto a través de un overlay (o tal cual, sin él). Es `Copy`: dos referencias.
#[derive(Clone, Copy)]
pub struct View<'a> {
    tree: &'a Tree,
    overlay: Option<&'a Overlay>,
}

impl<'a> View<'a> {
    pub fn new(tree: &'a Tree, overlay: Option<&'a Overlay>) -> View<'a> {
        View {
            tree,
            overlay: overlay.filter(|o| !o.is_empty()),
        }
    }

    pub fn tree(&self) -> &'a Tree {
        self.tree
    }

    pub fn overlay(&self) -> Option<&'a Overlay> {
        self.overlay
    }

    /// Versión del overlay (0 sin overlay), para la clave de la caché del layout.
    pub fn version(&self) -> u64 {
        self.overlay.map_or(0, Overlay::version)
    }

    pub fn alloc(&self, id: NodeId) -> u64 {
        match self.overlay {
            Some(o) => o.alloc(self.tree, id),
            None => self.tree.node(id).alloc,
        }
    }

    pub fn logical(&self, id: NodeId) -> u64 {
        match self.overlay {
            Some(o) => o.logical(self.tree, id),
            None => self.tree.node(id).logical,
        }
    }

    pub fn files_below(&self, id: NodeId) -> u64 {
        match self.overlay {
            Some(o) => o.files_below(self.tree, id),
            None => self.tree.node(id).files_below as u64,
        }
    }

    /// ¿Es visible (ni él ni sus ancestros se eliminaron)?
    pub fn is_visible(&self, id: NodeId) -> bool {
        self.overlay.is_none_or(|o| o.is_visible(self.tree, id))
    }

    /// ¿Se ha tocado este directorio (cambió algo dentro)?
    pub fn is_touched(&self, id: NodeId) -> bool {
        self.overlay.is_some_and(|o| o.is_touched(id))
    }

    /// Hijos visibles de `id`. Para un directorio que no se tocó es el rango del árbol base (sin reservar memoria); para uno
    /// tocado, los que quedan, reordenados por su tamaño visible (el nodo sintético del sistema va al final, como en el
    /// árbol base: quien lo coloca por tamaño es el layout).
    pub fn kids(&self, id: NodeId) -> Kids {
        let r = self.tree.children(id);
        let Some(o) = self.overlay.filter(|o| o.is_touched(id)) else {
            return Kids::Range(r);
        };
        let mut v: Vec<NodeId> = r.filter(|&c| !o.is_removed(c)).collect();
        let t = self.tree;
        v.sort_by(|&a, &b| {
            let (sa, sb) = (
                t.node(a).flags & flags::SYSTEM_INFO != 0,
                t.node(b).flags & flags::SYSTEM_INFO != 0,
            );
            sa.cmp(&sb)
                .then(o.alloc(t, b).cmp(&o.alloc(t, a)))
                .then_with(|| t.name_bytes(a).cmp(t.name_bytes(b)))
        });
        Kids::List(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::{FinishOptions, TreeBuilder};
    use crate::record::{Batch, EntryData};

    const NOW: u32 = 1_760_000_000;

    fn dir() -> EntryData {
        EntryData {
            flags: flags::DIR,
            ..EntryData::default()
        }
    }

    fn file(alloc: u64) -> EntryData {
        EntryData {
            alloc,
            logical: alloc + 10,
            mtime: NOW - 86_400,
            ..EntryData::default()
        }
    }

    /// R/{a/{x 100, y 50}, b/{z 30}, w 20}: raíz 200, a 150, b 30, w 20.
    fn sample() -> Tree {
        let mut b = TreeBuilder::new();
        let mut root = Batch::detached(0);
        root.push(0, b"R", dir());
        b.add_batch(root).unwrap();
        let mut l0 = Batch::listing(0, 1);
        l0.push(0, b"a", dir());
        l0.push(0, b"b", dir());
        l0.push(0, b"w", file(20));
        b.add_batch(l0).unwrap();
        let mut la = Batch::listing(1, 4);
        la.push(1, b"x", file(100));
        la.push(1, b"y", file(50));
        b.add_batch(la).unwrap();
        let mut lb = Batch::listing(2, 6);
        lb.push(2, b"z", file(30));
        b.add_batch(lb).unwrap();
        b.finish(&FinishOptions::new(NOW))
    }

    fn id(t: &Tree, parent: NodeId, name: &str) -> NodeId {
        t.child_by_name(parent, name.as_bytes())
            .unwrap_or_else(|| panic!("{name}"))
    }

    #[test]
    fn el_arbol_de_prueba_es_el_esperado() {
        let t = sample();
        assert_eq!((t.node(0).alloc, t.node(0).files_below), (200, 4));
        assert_eq!(t.children(0).count(), 3);
        assert_eq!(t.name_lossy(1), "a");
    }

    #[test]
    fn sin_overlay_la_vista_es_el_arbol() {
        let t = sample();
        let v = View::new(&t, None);
        assert_eq!(v.alloc(0), 200);
        assert_eq!(v.version(), 0);
        assert!(matches!(v.kids(0), Kids::Range(_)));
        // Un overlay vacío se trata como ninguno.
        let o = Overlay::new(&t);
        let v = View::new(&t, Some(&o));
        assert!(v.overlay().is_none() && matches!(v.kids(0), Kids::Range(_)));
    }

    #[test]
    fn eliminar_un_archivo_resta_a_todos_sus_ancestros() {
        let t = sample();
        let a = id(&t, 0, "a");
        let x = id(&t, a, "x");
        let mut o = Overlay::new(&t);
        o.remove(&t, x);
        assert!(o.is_removed(x) && !o.is_visible(&t, x));
        assert_eq!(
            (o.alloc(&t, a), o.logical(&t, a), o.files_below(&t, a)),
            (50, 60, 1)
        );
        assert_eq!((o.alloc(&t, 0), o.files_below(&t, 0)), (100, 3));
        assert_eq!(o.alloc(&t, x), 0);
        assert!(o.is_touched(a) && o.is_touched(0) && !o.is_touched(id(&t, 0, "b")));
        assert_eq!(o.version(), 1);
        // Idempotente.
        o.remove(&t, x);
        assert_eq!((o.alloc(&t, 0), o.version()), (100, 1));
        // Los hijos visibles de `a` son solo `y`; los de `b` siguen siendo el rango del árbol.
        let v = View::new(&t, Some(&o));
        assert_eq!(v.kids(a).iter().collect::<Vec<_>>(), vec![id(&t, a, "y")]);
        assert!(matches!(v.kids(id(&t, 0, "b")), Kids::Range(_)));
        assert_eq!(v.alloc(a), 50);
        assert_eq!(v.version(), 1);
    }

    #[test]
    fn eliminar_una_carpeta_oculta_todo_lo_que_contiene() {
        let t = sample();
        let a = id(&t, 0, "a");
        let (x, y) = (id(&t, a, "x"), id(&t, a, "y"));
        let mut o = Overlay::new(&t);
        o.remove(&t, a);
        assert!(!o.is_visible(&t, x) && !o.is_visible(&t, y));
        assert_eq!((o.alloc(&t, 0), o.files_below(&t, 0)), (50, 2));
        // Eliminar un descendiente de lo ya eliminado no cambia nada.
        o.remove(&t, x);
        assert_eq!((o.alloc(&t, 0), o.version()), (50, 1));
        let v = View::new(&t, Some(&o));
        let kids: Vec<String> = v
            .kids(0)
            .iter()
            .map(|c| t.name_lossy(c).into_owned())
            .collect();
        assert_eq!(kids, ["b", "w"]);
    }

    #[test]
    fn una_limpieza_parcial_reordena_los_hijos_visibles() {
        let t = sample();
        let (a, b, w) = (id(&t, 0, "a"), id(&t, 0, "b"), id(&t, 0, "w"));
        let mut o = Overlay::new(&t);
        // `b` pesaba 30 y queda en 15: pasa por debajo de `w` (20).
        o.shrink(&t, b, 15, 15, 1);
        assert_eq!((o.alloc(&t, b), o.alloc(&t, 0)), (15, 185));
        let v = View::new(&t, Some(&o));
        assert_eq!(v.kids(0).iter().collect::<Vec<_>>(), vec![a, w, b]);
        // A igual tamaño visible, por nombre.
        o.shrink(&t, w, 5, 5, 0);
        let v = View::new(&t, Some(&o));
        assert_eq!(
            v.kids(0).iter().collect::<Vec<_>>(),
            vec![a, b, w],
            "15 y 15: b antes que w"
        );
    }

    #[test]
    fn nunca_se_resta_mas_de_lo_que_hay_ni_se_toca_la_raiz() {
        let t = sample();
        let b = id(&t, 0, "b");
        let mut o = Overlay::new(&t);
        o.shrink(&t, b, 1_000, 1_000, 10);
        assert_eq!(
            (o.alloc(&t, b), o.logical(&t, b), o.files_below(&t, b)),
            (0, 0, 0)
        );
        assert_eq!(o.alloc(&t, 0), 170, "solo se resta lo que había en `b`");
        let before = o.version();
        o.remove(&t, 0);
        assert!(
            !o.is_removed(0) && o.version() == before,
            "la raíz no se puede eliminar"
        );
        // Un id fuera de rango no hace nada.
        o.remove(&t, 9_999);
        o.shrink(&t, 9_999, 1, 1, 1);
        assert_eq!(o.version(), before);
        assert_eq!(o.removed_alloc(), 30);
    }
}
