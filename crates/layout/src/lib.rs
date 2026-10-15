//! Layout del treemap de Tesela (PLAN.md §2.9 y §3.8). Puro: sin GPU, sin E/S y sin `unsafe`.
//!
//! * [`squarify`]: el algoritmo squarified de Bruls et al., lineal porque los hijos llegan ordenados.
//! * [`Layout::compute`]: profundidad 2 (un nivel con etiqueta y uno insinuado), agrupación "Otros" para los
//!   bloques de menos de 24×24 px (un sufijo de la lista ordenada, que se puede abrir) y LOD de dibujo (nada por
//!   debajo de ~3×3 px, tope de ~15 000 rectángulos).
//! * [`Layout::hit`]: hit-test con una rejilla espacial.
//! * [`LayoutCache`]: caché por `(raíz, viewport, profundidad, versión)`.
//! * [`Children`]: los hijos de un directorio como los ven el mapa y la lista (con el bloque del sistema en su
//!   sitio por tamaño y oculto si pesa menos del 1 %).

mod cache;
mod geom;
mod grid;
mod layout;
mod order;
mod squarify;

pub use cache::LayoutCache;
pub use geom::Rect;
pub use layout::{Cell, CellKind, Depth, Key, Layout, LayoutStats, NO_CELL, Others, Params, Root};
pub use order::{Children, SYSTEM_BLOCK_MIN_PERCENT};
pub use squarify::squarify;

#[cfg(test)]
mod tests;
