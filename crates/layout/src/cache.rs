//! Caché del layout por `(raíz, viewport, profundidad, versión)` (PLAN.md §3.8).

use crate::layout::{Key, Layout};
use std::sync::Arc;
use tesela_model::Tree;

/// Caché pequeña (las últimas claves usadas) de layouts. Guardar más de uno permite animar el zoom entre dos
/// niveles y volver atrás sin recalcular.
#[derive(Debug)]
pub struct LayoutCache {
    entries: Vec<(Key, Arc<Layout>)>,
    capacity: usize,
    hits: u64,
    misses: u64,
}

impl LayoutCache {
    /// Caché de hasta `capacity` layouts (mínimo 1).
    pub fn new(capacity: usize) -> LayoutCache {
        LayoutCache {
            entries: Vec::new(),
            capacity: capacity.max(1),
            hits: 0,
            misses: 0,
        }
    }

    /// El layout de `key`: el guardado si la clave coincide exactamente, o uno nuevo (que desaloja al más antiguo).
    pub fn get(&mut self, tree: &Tree, key: Key) -> Arc<Layout> {
        if let Some(pos) = self.entries.iter().position(|(k, _)| *k == key) {
            self.hits += 1;
            let e = self.entries.remove(pos);
            let l = e.1.clone();
            self.entries.push(e);
            return l;
        }
        self.misses += 1;
        let layout = Arc::new(Layout::compute(tree, key));
        if self.entries.len() >= self.capacity {
            self.entries.remove(0);
        }
        self.entries.push((key, layout.clone()));
        layout
    }

    /// Veces que se devolvió un layout guardado.
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Veces que hubo que calcular.
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Descarta todo (por ejemplo, al abrir otro árbol).
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}
