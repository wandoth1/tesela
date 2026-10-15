//! El árbol final inmutable (CSR) que produce el `TreeBuilder` (PLAN.md §3.5).
//!
//! # Definiciones
//! * Los ids son posiciones en un recorrido BFS; la raíz es el id 0 y su `parent` es [`NO_PARENT`].
//! * Los hijos de un directorio ocupan ids contiguos `first_child..first_child + child_count` y están ordenados por
//!   `alloc` descendente; a igual `alloc`, por nombre (bytes WTF-8) ascendente, para que el orden sea determinista.
//!   El nodo [`flags::SYSTEM_INFO`] va siempre el último.
//! * Un archivo tiene `first_child = 0`, `child_count = 0` y `files_below = 0`. Un directorio tiene en `files_below` el
//!   número de entradas que no son directorios en su subárbol (los enlaces duros deduplicados cuentan; el nodo
//!   sintético no).
//! * `alloc` y `logical` de un directorio son los suyos propios más los de todo lo que contiene. Un enlace duro que no
//!   conserva el tamaño (flag [`flags::HARDLINK`]) tiene ambos a 0: se deduplica `logical` igual que `alloc`, porque es
//!   el mismo archivo y de otro modo `logical` sumaría de más.
//! * Columnas laterales por id: `mtime` y `hist`. Para los archivos `hist` está vacío.
//! * `Hist` guarda, además de los porcentajes (solo para dibujar), el RESULTADO de aplicar los umbrales sobre los
//!   acumuladores `u64` (ver [`Hist::dominant_type`] y [`Hist::dominant_age`]); mide 14 B, no los 12 B del plan.

use crate::flags;
use crate::record::NO_PARENT;
use crate::types::{AGE_BUCKETS, FileType};
use crate::wtf8;
use std::borrow::Cow;
use std::ops::Range;

/// Identificador de un nodo del `Tree` final (posición BFS).
pub type NodeId = u32;

/// Nodo del árbol (40 B).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Node {
    /// Bytes ocupados en disco, contando todo el subárbol.
    pub alloc: u64,
    /// Tamaño lógico, contando todo el subárbol.
    pub logical: u64,
    /// Padre ([`NO_PARENT`] en la raíz).
    pub parent: u32,
    /// Id del primer hijo (0 si no tiene).
    pub first_child: u32,
    /// Número de hijos.
    pub child_count: u32,
    /// Posición del nombre en el arena de nombres.
    pub name_off: u32,
    /// Longitud del nombre en bytes WTF-8.
    pub name_len: u16,
    /// Flags de [`crate::flags`].
    pub flags: u16,
    /// Entradas que no son directorios por debajo (0 en los archivos).
    pub files_below: u32,
}

const _: () = assert!(
    size_of::<Node>() == 40,
    "Node debe medir 40 B (PLAN.md §3.5)"
);

impl Node {
    /// ¿Es un directorio?
    pub fn is_dir(&self) -> bool {
        self.flags & flags::DIR != 0
    }
}

/// Resultado de "sin tipo dominante" / "sin tramo de antigüedad".
pub const NONE: u8 = 255;

/// Histogramas de un directorio: reparto de sus bytes por tipo y por antigüedad (solo archivos, ponderado por `alloc`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Hist {
    /// Porcentaje de bytes por [`FileType`] (suma 100 si el directorio tiene bytes). Solo para dibujar.
    pub type_pct: [u8; FileType::COUNT],
    /// Porcentaje de bytes por tramo de antigüedad (suma 100 si hay bytes). Solo para dibujar.
    pub age_pct: [u8; AGE_BUCKETS],
    /// Tipo dominante: el que supera el 50 % de los bytes (calculado sobre los `u64`), o [`NONE`] ("Mixto").
    pub(crate) dominant_type: u8,
    /// Tramo de antigüedad (ver [`Hist::dominant_age`]), o [`NONE`] si no tiene bytes.
    pub(crate) dominant_age: u8,
}

const _: () = assert!(size_of::<Hist>() == 14);

impl Hist {
    /// Histograma vacío (archivos y directorios sin bytes).
    pub const EMPTY: Hist = Hist {
        type_pct: [0; FileType::COUNT],
        age_pct: [0; AGE_BUCKETS],
        dominant_type: NONE,
        dominant_age: NONE,
    };

    /// Tipo que supera ESTRICTAMENTE el 50 % de los bytes del directorio; `None` es "Mixto" (o sin bytes).
    pub fn dominant_type(&self) -> Option<FileType> {
        FileType::from_index(self.dominant_type)
    }

    /// Tramo de antigüedad (0 = menos de 1 mes ... 4 = más de 3 años) del directorio: el MÁS ANTIGUO tal que al menos
    /// el 80 % de los bytes (calculado sobre los `u64`) están en ese tramo o en uno más antiguo. Así "el 92 % de esto
    /// no se modifica desde 2022" se corresponde con el tramo. Un directorio mitad reciente, mitad antiguo da el
    /// tramo reciente (nunca se afirma más antigüedad de la que hay). `None` si no tiene bytes.
    pub fn dominant_age(&self) -> Option<usize> {
        (self.dominant_age != NONE).then_some(self.dominant_age as usize)
    }
}

/// Cifras agregadas del escaneo, calculadas por el builder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeStats {
    /// Entradas que no son directorios (incluye enlaces duros deduplicados; excluye el nodo sintético).
    pub files: u64,
    /// Directorios reales (sin los reparse points), incluida la raíz.
    pub dirs: u64,
    /// Reparse points de directorio (no seguidos).
    pub reparse_dirs: u64,
    /// Archivos con el flag `REPARSE`.
    pub reparse_files: u64,
    /// Entradas con el flag `CLOUD`.
    pub cloud_entries: u64,
    /// Directorios marcados `INCOMPLETE`.
    pub incomplete_dirs: u64,
    /// Directorios marcados `ERROR`.
    pub error_dirs: u64,
    /// Grupos de enlaces duros con más de una entrada.
    pub hardlink_groups: u64,
    /// Entradas que NO conservaron el tamaño (enlaces sobrantes).
    pub hardlink_extra_entries: u64,
    /// Bytes de `alloc` que se habrían contado de más sin deduplicar.
    pub hardlink_alloc_saved: u64,
    /// Bytes de `logical` que se habrían contado de más sin deduplicar.
    pub hardlink_logical_saved: u64,
    /// `alloc` total de lo escaneado (la raíz, sin el nodo sintético).
    pub scanned_alloc: u64,
    /// `alloc` del nodo sintético "espacio no explicado" (0 si no se añadió).
    pub unexplained_alloc: u64,
    /// Entradas recibidas que no cuelgan de la raíz (su padre no llegó): no están en el árbol.
    pub orphans: u64,
}

/// Árbol inmutable en formato CSR.
#[derive(Clone, Debug)]
pub struct Tree {
    pub(crate) nodes: Vec<Node>,
    pub(crate) names: Vec<u8>,
    pub(crate) mtime: Vec<u32>,
    pub(crate) hist: Vec<Hist>,
    pub(crate) stats: TreeStats,
}

impl Tree {
    /// Árbol con solo una raíz sin nombre, `INCOMPLETE` (no llegó ni la raíz).
    pub(crate) fn placeholder() -> Tree {
        let root = Node {
            alloc: 0,
            logical: 0,
            parent: NO_PARENT,
            first_child: 0,
            child_count: 0,
            name_off: 0,
            name_len: 0,
            flags: flags::DIR | flags::INCOMPLETE,
            files_below: 0,
        };
        Tree {
            nodes: vec![root],
            names: Vec::new(),
            mtime: vec![0],
            hist: vec![Hist::EMPTY],
            stats: TreeStats {
                dirs: 1,
                incomplete_dirs: 1,
                ..TreeStats::default()
            },
        }
    }

    /// Número de nodos.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Siempre `false`: un `Tree` tiene al menos la raíz.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Id de la raíz.
    pub fn root(&self) -> NodeId {
        0
    }

    /// Todos los nodos, indexados por id.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Nodo `id` (panica si no existe).
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    /// Padre de `id` (`None` en la raíz).
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        let p = self.node(id).parent;
        (p != NO_PARENT).then_some(p)
    }

    /// Ids de los hijos de `id`, contiguos y ordenados por tamaño.
    pub fn children(&self, id: NodeId) -> Range<NodeId> {
        let n = self.node(id);
        if n.child_count == 0 {
            0..0
        } else {
            n.first_child..n.first_child + n.child_count
        }
    }

    /// Hijo de `parent` con ese nombre exacto (búsqueda lineal entre sus hijos).
    pub fn child_by_name(&self, parent: NodeId, name: &[u8]) -> Option<NodeId> {
        self.children(parent).find(|&c| self.name_bytes(c) == name)
    }

    /// Nombre en bytes WTF-8.
    pub fn name_bytes(&self, id: NodeId) -> &[u8] {
        let n = self.node(id);
        &self.names[n.name_off as usize..n.name_off as usize + n.name_len as usize]
    }

    /// Nombre como texto (los sustitutos desparejados se muestran como U+FFFD).
    pub fn name_lossy(&self, id: NodeId) -> Cow<'_, str> {
        String::from_utf8_lossy(self.name_bytes(id))
    }

    /// Nombre en UTF-16, tal cual lo guarda Windows (con sustitutos desparejados si los hay).
    pub fn name_utf16(&self, id: NodeId) -> Vec<u16> {
        let mut v = Vec::new();
        wtf8::decode_to_utf16(self.name_bytes(id), &mut v);
        v
    }

    /// Última modificación en segundos Unix.
    pub fn mtime(&self, id: NodeId) -> u32 {
        self.mtime[id as usize]
    }

    /// Histogramas del nodo.
    pub fn hist(&self, id: NodeId) -> &Hist {
        &self.hist[id as usize]
    }

    /// Cifras agregadas.
    pub fn stats(&self) -> &TreeStats {
        &self.stats
    }

    /// Ruta completa de `id` en UTF-16: el nombre de la raíz (la ruta escaneada) más los nombres hasta `id`,
    /// separados por `\`. Para el nodo sintético es una ruta ficticia.
    pub fn path_utf16(&self, id: NodeId) -> Vec<u16> {
        let mut chain = Vec::new();
        let mut cur = id;
        while let Some(p) = self.parent(cur) {
            chain.push(cur);
            cur = p;
            if chain.len() > self.nodes.len() {
                break; // salvaguarda ante un árbol corrupto
            }
        }
        let mut out = self.name_utf16(self.root());
        for &c in chain.iter().rev() {
            if out.last() != Some(&(b'\\' as u16)) {
                out.push(b'\\' as u16);
            }
            wtf8::decode_to_utf16(self.name_bytes(c), &mut out);
        }
        out
    }

    /// Ruta completa como texto (con U+FFFD en los sustitutos desparejados). Solo para mostrar.
    pub fn path_lossy(&self, id: NodeId) -> String {
        String::from_utf16_lossy(&self.path_utf16(id))
    }

    /// Ruta completa como `PathBuf`, exacta incluso con sustitutos desparejados.
    #[cfg(windows)]
    pub fn path(&self, id: NodeId) -> std::path::PathBuf {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&self.path_utf16(id)).into()
    }

    /// Bytes de memoria dinámica que ocupa el árbol (capacidad reservada de sus vectores).
    pub fn heap_bytes(&self) -> usize {
        self.nodes.capacity() * size_of::<Node>()
            + self.names.capacity()
            + self.mtime.capacity() * size_of::<u32>()
            + self.hist.capacity() * size_of::<Hist>()
    }

    /// ¿Tienen los dos árboles la misma forma y los mismos datos? Compara cada id (nombre por contenido, no por
    /// posición en el arena de nombres), `mtime`, histogramas y cifras. Dos escaneos del mismo árbol lógico dan
    /// árboles estructuralmente iguales aunque los ids provisionales o el arena difieran.
    pub fn structural_eq(&self, other: &Tree) -> bool {
        self.first_difference(other).is_none()
    }

    /// Primera diferencia estructural entre dos árboles (ver [`Tree::structural_eq`]), descrita en texto, o `None`.
    pub fn first_difference(&self, other: &Tree) -> Option<String> {
        if self.nodes.len() != other.nodes.len() {
            return Some(format!(
                "{} nodos frente a {}",
                self.nodes.len(),
                other.nodes.len()
            ));
        }
        if self.stats != other.stats {
            return Some(format!(
                "cifras distintas: {:?} frente a {:?}",
                self.stats, other.stats
            ));
        }
        for i in 0..self.nodes.len() as u32 {
            let (a, b) = (self.node(i), other.node(i));
            let same = a.alloc == b.alloc
                && a.logical == b.logical
                && a.parent == b.parent
                && a.first_child == b.first_child
                && a.child_count == b.child_count
                && a.name_len == b.name_len
                && a.flags == b.flags
                && a.files_below == b.files_below
                && self.name_bytes(i) == other.name_bytes(i);
            if !same {
                return Some(format!(
                    "nodo {i} ({}): {a:?} frente a {b:?}",
                    self.name_lossy(i)
                ));
            }
            if self.mtime(i) != other.mtime(i) {
                return Some(format!(
                    "nodo {i} ({}): mtime {} frente a {}",
                    self.name_lossy(i),
                    self.mtime(i),
                    other.mtime(i)
                ));
            }
            if self.hist(i) != other.hist(i) {
                return Some(format!(
                    "nodo {i} ({}): histogramas distintos",
                    self.name_lossy(i)
                ));
            }
        }
        None
    }

    /// Comprueba las invariantes del formato; devuelve la primera que falle. Las usan los tests del builder y del
    /// escáner. No exige que `alloc` de un directorio sea exactamente la suma de sus hijos (el directorio ocupa
    /// también su propio espacio), solo que no sea menor.
    pub fn check_invariants(&self) -> Result<(), String> {
        let n = self.nodes.len();
        if n == 0 || self.mtime.len() != n || self.hist.len() != n {
            return Err("columnas de longitud distinta".into());
        }
        if self.nodes[0].parent != NO_PARENT {
            return Err("la raíz debe tener parent = NO_PARENT".into());
        }
        let mut next = 1u32;
        let mut total_files = 0u64;
        for id in 0..n as u32 {
            let node = self.node(id);
            if node.name_off as usize + node.name_len as usize > self.names.len() {
                return Err(format!("nodo {id}: nombre fuera del arena"));
            }
            if !node.is_dir() {
                if node.child_count != 0 || node.first_child != 0 || node.files_below != 0 {
                    return Err(format!(
                        "nodo {id}: un archivo no puede tener hijos ni files_below"
                    ));
                }
                if node.flags & flags::SYSTEM_INFO == 0 {
                    total_files += 1;
                }
                if self.hist[id as usize] != Hist::EMPTY {
                    return Err(format!("nodo {id}: un archivo tiene histograma"));
                }
                continue;
            }
            if node.child_count == 0 {
                if node.first_child != 0 || node.files_below != 0 {
                    return Err(format!(
                        "nodo {id}: directorio vacío con first_child o files_below"
                    ));
                }
                continue;
            }
            if node.first_child != next {
                return Err(format!(
                    "nodo {id}: first_child {} != {next} (los hijos deben ser contiguos en BFS)",
                    node.first_child
                ));
            }
            next += node.child_count;
            if next as usize > n {
                return Err(format!("nodo {id}: hijos fuera de rango"));
            }
            let (mut sum_alloc, mut sum_logical, mut files) = (0u64, 0u64, 0u64);
            let mut prev: Option<NodeId> = None;
            for c in self.children(id) {
                let ch = self.node(c);
                if ch.parent != id {
                    return Err(format!("nodo {c}: parent {} != {id}", ch.parent));
                }
                if let Some(p) = prev {
                    let a = self.node(p);
                    let (sa, sb) = (
                        a.flags & flags::SYSTEM_INFO != 0,
                        ch.flags & flags::SYSTEM_INFO != 0,
                    );
                    let ordered = sa
                        .cmp(&sb)
                        .then(ch.alloc.cmp(&a.alloc))
                        .then_with(|| self.name_bytes(p).cmp(self.name_bytes(c)))
                        == std::cmp::Ordering::Less;
                    if !ordered {
                        return Err(format!("hijos {p} y {c} de {id} mal ordenados"));
                    }
                }
                prev = Some(c);
                sum_alloc += ch.alloc;
                sum_logical += ch.logical;
                files += if ch.is_dir() {
                    ch.files_below as u64
                } else if ch.flags & flags::SYSTEM_INFO != 0 {
                    0
                } else {
                    1
                };
            }
            if node.alloc < sum_alloc || node.logical < sum_logical {
                return Err(format!(
                    "nodo {id}: su tamaño es menor que la suma de sus hijos"
                ));
            }
            if node.files_below as u64 != files {
                return Err(format!(
                    "nodo {id}: files_below {} != {files}",
                    node.files_below
                ));
            }
        }
        if next as usize != n {
            return Err(format!("hay {n} nodos pero los hijos solo suman {next}"));
        }
        if self.nodes[0].files_below as u64 != total_files {
            return Err("files_below de la raíz no coincide con el número de archivos".into());
        }
        Ok(())
    }
}
