//! Hijos de un directorio tal como los ve la interfaz (mapa y lista): ordenados por tamaño descendente.
//!
//! El `TreeBuilder` ya los deja contiguos y ordenados, salvo el nodo sintético `SYSTEM_INFO` ("Windows y otros datos
//! del sistema"), que va SIEMPRE el último aunque sea enorme. Para el mapa y la lista se coloca en su sitio por
//! tamaño, y se oculta si pesa menos del 1 % del directorio (PLAN.md §2.8). Solo la raíz de un volumen lo tiene,
//! así que en el caso normal esto es el rango contiguo del árbol, sin reservar memoria.

use std::ops::Range;
use tesela_model::{NodeId, Tree, flags};

/// Por debajo de este porcentaje del tamaño del directorio, el bloque del sistema no se muestra.
pub const SYSTEM_BLOCK_MIN_PERCENT: u64 = 1;

/// Hijos visibles de un directorio, en orden de tamaño descendente.
#[derive(Clone, Debug)]
pub enum Children {
    /// El rango contiguo del árbol (sin bloque del sistema que recolocar).
    Range(Range<NodeId>),
    /// Orden propio: hay un bloque del sistema que se ha colocado por tamaño (o se ha ocultado).
    List(Vec<NodeId>),
}

impl Children {
    /// Hijos visibles de `node`.
    pub fn of(tree: &Tree, node: NodeId) -> Children {
        let r = tree.children(node);
        if r.is_empty() {
            return Children::Range(0..0);
        }
        let last = r.end - 1;
        if tree.node(last).flags & flags::SYSTEM_INFO == 0 {
            return Children::Range(r);
        }
        let sys = tree.node(last);
        let parent_alloc = tree.node(node).alloc;
        let visible = sys.alloc > 0
            && (sys.alloc as u128) * 100
                >= (parent_alloc as u128) * SYSTEM_BLOCK_MIN_PERCENT as u128;
        let mut ids: Vec<NodeId> = (r.start..last).collect();
        if visible {
            // `partition_point` exige que el criterio sea `true` primero y `false` después: es el caso, porque
            // los hijos van en orden descendente de `alloc`.
            let pos = ids.partition_point(|&c| tree.node(c).alloc >= sys.alloc);
            ids.insert(pos, last);
        }
        Children::List(ids)
    }

    /// Número de hijos visibles.
    pub fn len(&self) -> usize {
        match self {
            Children::Range(r) => r.len(),
            Children::List(v) => v.len(),
        }
    }

    /// ¿No hay ninguno?
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// El hijo en la posición `i` (panica si no existe).
    pub fn get(&self, i: usize) -> NodeId {
        match self {
            Children::Range(r) => r.start + i as u32,
            Children::List(v) => v[i],
        }
    }

    /// Iterador sobre los ids desde la posición `from`.
    pub fn iter_from(&self, from: usize) -> impl Iterator<Item = NodeId> + '_ {
        (from..self.len()).map(|i| self.get(i))
    }
}
