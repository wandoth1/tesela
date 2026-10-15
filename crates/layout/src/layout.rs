//! El layout propiamente dicho: de un nodo del árbol a una lista de celdas con su rectángulo.
//!
//! # Dos capas distintas (PLAN.md §2.9)
//! * **Agrupación interactiva** ("Otros"): al repartir los hijos del nodo raíz, los de menos de 24×24 px (área
//!   inferior a `min_group_px²`) se funden en UNA celda "Otros (N elementos, X)". Como los hijos van ordenados por
//!   tamaño, esos hijos son siempre un SUFIJO de la lista ordenada, y la celda se puede abrir: es otra raíz
//!   ([`Root`] con `start` > 0) que reparte solo ese sufijo. Solo se agrupa si hay al menos 2 elementos y alguno pesa.
//! * **LOD de dibujo**: en el nivel insinuado no se dibuja nada por debajo de ~3×3 px (`lod_px`) y el total de
//!   rectángulos tiene un tope (`max_rects`, ~15 000 por frame).
//!
//! # Profundidad
//! Nivel 0: los hijos del nodo, con etiqueta. Nivel 1 (solo con [`Depth::Two`]): los hijos de cada carpeta del
//! nivel 0, dentro de ella, insinuados.
//!
//! # Tamaños
//! Se usa `alloc` (tamaño en disco). Un hijo con `alloc == 0` no ocupa área. El campo `version` de la clave es de
//! quien llama: lo cambia cuando cambian los tamaños que ve el layout (un árbol nuevo y, en el incremento 3, la
//! versión del overlay de limpieza) para invalidar la caché.

use crate::geom::Rect;
use crate::grid::Grid;
use crate::order::Children;
use crate::squarify::squarify;
use std::sync::OnceLock;
use tesela_model::{NodeId, Tree};

/// Profundidad del layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Depth {
    /// Solo los hijos del nodo.
    One,
    /// Los hijos y, dentro de cada carpeta, una insinuación de sus hijos.
    Two,
}

/// Qué se reparte: los hijos visibles de `node` en las posiciones `start..end` (todos = `0..u32::MAX`; otra cosa es
/// una agrupación "Otros" abierta, que puede ser un sufijo o una página).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Root {
    pub node: NodeId,
    pub start: u32,
    pub end: u32,
}

impl Root {
    /// Todos los hijos de `node`.
    pub fn node(node: NodeId) -> Root {
        Root {
            node,
            start: 0,
            end: u32::MAX,
        }
    }

    /// ¿Es una agrupación "Otros" abierta (y no la carpeta entera)?
    pub fn is_group(&self) -> bool {
        self.start > 0 || self.end != u32::MAX
    }
}

/// Parámetros del layout. Entran en la clave de la caché.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    /// Una celda con menos área que `min_group_px²` se agrupa en "Otros" (24 px, PLAN.md §2.9).
    pub min_group_px: f32,
    /// En el nivel insinuado no se dibuja una celda con algún lado menor de esto (3 px).
    pub lod_px: f32,
    /// Tope de celdas del layout (15 000).
    pub max_rects: usize,
    /// Alto de la banda de la etiqueta (nombre y tamaño) dentro de una celda que la lleva.
    pub header_px: f32,
    /// Margen entre el borde de una celda y las celdas que insinúa.
    pub pad_px: f32,
    /// Una celda de al menos este ancho y alto lleva etiqueta.
    pub label_min_w: f32,
    pub label_min_h: f32,
}

impl Default for Params {
    fn default() -> Params {
        Params {
            min_group_px: 24.0,
            lod_px: 3.0,
            max_rects: 15_000,
            header_px: 38.0,
            pad_px: 6.0,
            label_min_w: 64.0,
            label_min_h: 40.0,
        }
    }
}

impl Params {
    /// ¿Cabe la etiqueta (nombre y tamaño) en una celda de este rectángulo?
    pub fn label_fits(&self, r: &Rect) -> bool {
        r.w >= self.label_min_w && r.h >= self.label_min_h
    }

    /// Rectángulo interior de una celda donde se insinúan sus hijos: bajo la etiqueta si cabe; si no, casi todo.
    /// `None` si queda demasiado pequeño para dibujar algo.
    pub fn inner_rect(&self, r: &Rect) -> Option<Rect> {
        let inner = if self.label_fits(r) {
            Rect::new(
                r.x + self.pad_px,
                r.y + self.header_px,
                r.w - 2.0 * self.pad_px,
                r.h - self.header_px - self.pad_px,
            )
        } else {
            r.inset(2.0)
        };
        (inner.w >= 2.0 * self.lod_px && inner.h >= 2.0 * self.lod_px).then_some(inner)
    }
}

/// Clave de la caché: `(raíz, viewport, profundidad, versión)` más los parámetros.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub root: Root,
    /// Tamaño del viewport en px lógicos enteros (el origen lo decide quien dibuja).
    pub size: (u32, u32),
    pub depth: Depth,
    /// Versión de los datos (árbol y overlay): la pone quien llama.
    pub version: u64,
    pub params: Params,
}

/// Qué es una celda.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    /// Un nodo del árbol.
    Node(NodeId),
    /// La agrupación "Otros"; el índice lo es de [`Layout::others`].
    Others(u32),
}

/// Una celda del mapa.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    pub rect: Rect,
    pub kind: CellKind,
    /// 0 = con etiqueta; 1 = insinuada dentro de una celda de nivel 0.
    pub level: u8,
    /// Índice de la celda de nivel 0 que la contiene ([`NO_CELL`] en el nivel 0).
    pub parent: u32,
}

/// Valor de [`Cell::parent`] en las celdas de nivel 0.
pub const NO_CELL: u32 = u32::MAX;

/// Agrupación "Otros": los hijos visibles `start..start + count` de `parent` (un sufijo o una página).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Others {
    pub parent: NodeId,
    pub start: u32,
    pub count: u32,
    /// Suma de `alloc` de los agrupados.
    pub bytes: u64,
}

impl Others {
    /// La raíz que abre esta agrupación.
    pub fn root(&self) -> Root {
        Root {
            node: self.parent,
            start: self.start,
            end: self.start + self.count,
        }
    }
}

/// Cifras del cálculo (para los tests y para el informe de rendimiento).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutStats {
    /// Celdas de nivel 0.
    pub level0: usize,
    /// Celdas de nivel 1.
    pub level1: usize,
    /// Elementos que se agruparon en "Otros".
    pub grouped: usize,
    /// Se alcanzó `max_rects` y se dejaron celdas sin crear.
    pub truncated: bool,
}

/// Resultado del layout: celdas, agrupación y rejilla de hit-test (que se construye al primer uso).
#[derive(Debug)]
pub struct Layout {
    key: Key,
    cells: Vec<Cell>,
    others: Vec<Others>,
    stats: LayoutStats,
    grid: OnceLock<Grid>,
}

impl Layout {
    /// Calcula el layout de `key` sobre `tree`.
    pub fn compute(tree: &Tree, key: Key) -> Layout {
        let (w, h) = (key.size.0 as f32, key.size.1 as f32);
        let mut cells: Vec<Cell> = Vec::new();
        let mut others: Vec<Others> = Vec::new();
        let mut stats = LayoutStats::default();
        let params = key.params;
        let viewport = Rect::new(0.0, 0.0, w, h);

        let kids = Children::of(tree, key.root.node);
        let end = (key.root.end as usize).min(kids.len());
        let start = (key.root.start as usize).min(end);
        let alloc_of = |i: usize| tree.node(kids.get(i)).alloc;
        let area = (w as f64) * (h as f64);
        let total: f64 = (start..end).map(|i| alloc_of(i) as f64).sum();

        if total > 0.0 && area > 0.0 {
            let scale = area / total;
            let min_area = (params.min_group_px as f64).powi(2);
            let n = end - start;
            // Los hijos van de mayor a menor: los que miden menos de `min_area` son un sufijo (búsqueda binaria).
            let big = start + partition(n, |i| alloc_of(start + i) as f64 * scale >= min_area);
            // Hijos con tamaño: los de `alloc == 0` (al final) no tienen área y no se reparten.
            let sized = start + partition(n, |i| alloc_of(start + i) > 0);
            // Entradas del nivel 0 antes de repartirlas: (valor, qué es).
            let mut items: Vec<(f64, CellKind)> = Vec::new();
            let add_group =
                |others: &mut Vec<Others>, first: usize, last: usize, stats: &mut LayoutStats| {
                    let bytes: u64 = (first..last).map(alloc_of).fold(0u64, u64::saturating_add);
                    others.push(Others {
                        parent: key.root.node,
                        start: first as u32,
                        count: (last - first) as u32,
                        bytes,
                    });
                    stats.grouped += last - first;
                    (bytes as f64, CellKind::Others(others.len() as u32 - 1))
                };
            // Páginas para el caso "ningún hijo llega al área mínima" (ver abajo): `fit` hijos caben en el área mínima.
            let fit = ((area / min_area).floor() as usize).max(2);
            let page = fit.max((sized - start).div_ceil(fit));
            let none_fits = big == start && sized > start;
            // Con un viewport DIMINUTO (menos de ~34×34 px) `page` alcanza a todo el conjunto: paginar daría una única
            // "Otros" idéntica a la raíz y abrirla no avanzaría. Entonces no se agrupa nada y se reparten todos los hijos
            // (aunque midan menos de 24×24): abrir "Otros" siempre reduce el conjunto, o no hay "Otros" que abrir.
            let single_page = none_fits && page >= sized - start;
            if none_fits && !single_page {
                // NINGÚN hijo llega a 24×24 px (muchos hijos de tamaño parecido): agruparlos a todos daría una raíz
                // idéntica y "Otros" no avanzaría nunca. Se PAGINA: páginas de `page` hijos consecutivos, cada una un
                // "Otros" que, al abrirse, reparte solo sus elementos. Con `page >= area / min_area` el mayor de una
                // página mide al menos el área mínima, así que abrir una página siempre muestra elementos, y como la
                // página es más pequeña que el conjunto, abrir "Otros" siempre reduce el conjunto.
                let mut pages: Vec<(usize, usize, u64)> = Vec::new();
                let mut s = start;
                while s < sized {
                    let e = (s + page).min(sized);
                    let bytes: u64 = (s..e).map(alloc_of).fold(0u64, u64::saturating_add);
                    pages.push((s, e, bytes));
                    s = e;
                }
                // Las páginas van de mayor a menor (los hijos van ordenados): las que no llegan al área mínima se
                // funden en una sola agrupación final (la primera siempre llega: tiene al menos 1/páginas del total).
                let big_pages =
                    partition(pages.len(), |p| pages[p].2 as f64 * scale >= min_area).max(1);
                for &(s, e, _) in &pages[..big_pages] {
                    items.push(add_group(&mut others, s, e, &mut stats));
                }
                if big_pages < pages.len() {
                    let (s, e) = (pages[big_pages].0, pages[pages.len() - 1].1);
                    items.push(add_group(&mut others, s, e, &mut stats));
                }
            } else {
                // El sufijo candidato a "Otros" son los que quedan por debajo del área mínima, con tamaño o sin él.
                let group_count = end - big;
                let group_bytes: u64 = (big..end).map(alloc_of).fold(0u64, u64::saturating_add);
                // Un grupo de un solo elemento no tiene sentido, ni uno que no pesa nada.
                let grouped = group_count >= 2 && group_bytes > 0 && !single_page;
                // Sin grupo, se reparten todos los que tienen tamaño (aunque alguno mida menos de 24×24 px).
                let prefix_end = if grouped { big } else { sized };
                for i in start..prefix_end {
                    items.push((alloc_of(i) as f64, CellKind::Node(kids.get(i))));
                }
                if grouped {
                    items.push(add_group(&mut others, big, end, &mut stats));
                }
            }
            let vals: Vec<f64> = items.iter().map(|it| it.0).collect();
            let mut rects = Vec::with_capacity(vals.len());
            squarify(&vals, total, viewport, &mut rects);
            for (r, it) in rects.iter().zip(&items) {
                cells.push(Cell {
                    rect: *r,
                    kind: it.1,
                    level: 0,
                    parent: NO_CELL,
                });
            }
            stats.level0 = cells.len();

            if key.depth == Depth::Two {
                hint_level(tree, &params, &mut cells, &mut stats);
            }
        }
        Layout {
            key,
            cells,
            others,
            stats,
            grid: OnceLock::new(),
        }
    }

    /// La clave con la que se calculó.
    pub fn key(&self) -> &Key {
        &self.key
    }

    /// Todas las celdas: primero las de nivel 0 (de mayor a menor) y después las insinuadas.
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// Agrupaciones "Otros" (la celda [`CellKind::Others`]`(i)` es `others()[i]`).
    pub fn others(&self) -> &[Others] {
        &self.others
    }

    /// Cifras del cálculo.
    pub fn stats(&self) -> LayoutStats {
        self.stats
    }

    /// Celda más profunda bajo el punto `(x, y)` (en el espacio del layout: origen en la esquina del viewport).
    /// Usa la rejilla espacial, que se construye en la primera consulta.
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        let (w, h) = (self.key.size.0 as f32, self.key.size.1 as f32);
        let grid = self.grid.get_or_init(|| Grid::build(&self.cells, w, h));
        grid.hit(&self.cells, x, y).map(|i| i as usize)
    }

    /// Entradas de la rejilla (0 si aún no se ha usado).
    pub fn grid_entries(&self) -> usize {
        self.grid.get().map_or(0, Grid::entries)
    }

    /// Índice de la celda de nivel 0 de `node`, si está en este layout.
    pub fn cell_of_node(&self, node: NodeId) -> Option<usize> {
        self.cells[..self.stats.level0]
            .iter()
            .position(|c| c.kind == CellKind::Node(node))
    }
}

/// Primer índice `i` en `0..n` para el que `pred(i)` es falso (`pred` debe ser verdadero y luego falso).
fn partition(n: usize, pred: impl Fn(usize) -> bool) -> usize {
    let (mut lo, mut hi) = (0usize, n);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if pred(mid) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Nivel insinuado: los hijos de cada carpeta del nivel 0, de las más grandes a las más pequeñas (así el tope de
/// rectángulos se gasta primero donde más se ve).
fn hint_level(tree: &Tree, params: &Params, cells: &mut Vec<Cell>, stats: &mut LayoutStats) {
    let level0 = cells.len();
    let lod_area = (params.lod_px as f64).powi(2);
    let mut rects: Vec<Rect> = Vec::new();
    let mut vals: Vec<f64> = Vec::new();
    for pi in 0..level0 {
        let CellKind::Node(node) = cells[pi].kind else {
            continue;
        };
        let n = tree.node(node);
        if !n.is_dir() || n.child_count == 0 || n.alloc == 0 {
            continue;
        }
        let Some(inner) = params.inner_rect(&cells[pi].rect) else {
            continue;
        };
        if cells.len() >= params.max_rects {
            stats.truncated = true;
            break;
        }
        let kids = tree.children(node);
        let scale = (inner.w as f64) * (inner.h as f64) / n.alloc as f64;
        // Los hijos que quedarían por debajo del área mínima de dibujo se dejan sin crear.
        let mut count = partition(kids.len(), |i| {
            tree.node(kids.start + i as u32).alloc as f64 * scale >= lod_area
        });
        let budget = params.max_rects - cells.len();
        if count > budget {
            count = budget;
            stats.truncated = true;
        }
        if count == 0 {
            continue;
        }
        vals.clear();
        vals.extend((0..count).map(|i| tree.node(kids.start + i as u32).alloc as f64));
        rects.clear();
        // `total` es el `alloc` de la carpeta: lo que no se reparte (la cola diminuta y el espacio propio de la
        // carpeta) queda como hueco al final, sin cambiar la escala de las celdas que sí se dibujan.
        squarify(&vals, n.alloc as f64, inner, &mut rects);
        for (i, r) in rects.iter().enumerate() {
            if r.w < params.lod_px || r.h < params.lod_px {
                continue;
            }
            cells.push(Cell {
                rect: *r,
                kind: CellKind::Node(kids.start + i as u32),
                level: 1,
                parent: pi as u32,
            });
            stats.level1 += 1;
        }
    }
}
