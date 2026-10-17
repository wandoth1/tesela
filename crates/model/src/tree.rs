//! The immutable final tree (CSR) produced by `TreeBuilder` (PLAN.md §3.5).
//!
//! # Definitions
//! * Ids are positions in a BFS traversal; the root is id 0 and its `parent` is [`NO_PARENT`].
//! * A directory's children occupy contiguous ids `first_child..first_child + child_count` and are sorted by descending
//!   `alloc`; ties are sorted by name (WTF-8 bytes) ascending for deterministic order. [`flags::SYSTEM_INFO`] is always last.
//! * A file has `first_child = 0`, `child_count = 0`, and `files_below = 0`. A directory's `files_below` is the number of
//!   non-directory entries in its subtree (deduplicated hard links count; the synthetic node does not).
//! * A directory's `alloc` and `logical` include its own values and those of everything it contains. A hard link that
//!   does not retain the size ([`flags::HARDLINK`]) has both set to 0: `logical` is deduplicated like `alloc`, since it is
//!   the same file and otherwise `logical` would be overcounted.
//! * Side columns by id: `mtime` and `hist`. `hist` is empty for files.
//! * In addition to percentages (for drawing only), `Hist` stores the RESULT of applying thresholds to the `u64`
//!   accumulators (see [`Hist::dominant_type`] and [`Hist::dominant_age`]); it is 14 B, not the 12 B in the plan.

use crate::flags;
use crate::record::NO_PARENT;
use crate::types::{AGE_BUCKETS, FileType};
use crate::wtf8;
use std::borrow::Cow;
use std::ops::Range;

/// Identifier of a node in the final `Tree` (BFS position).
pub type NodeId = u32;

/// Tree node (40 B).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Node {
    /// Bytes allocated on disk, including the entire subtree.
    pub alloc: u64,
    /// Logical size, including the entire subtree.
    pub logical: u64,
    /// Parent ([`NO_PARENT`] for the root).
    pub parent: u32,
    /// First child id (0 if there are none).
    pub first_child: u32,
    /// Number of children.
    pub child_count: u32,
    /// Name position in the name arena.
    pub name_off: u32,
    /// Name length in WTF-8 bytes.
    pub name_len: u16,
    /// Flags from [`crate::flags`].
    pub flags: u16,
    /// Descendant entries that are not directories (0 for files).
    pub files_below: u32,
}

const _: () = assert!(size_of::<Node>() == 40, "Node must be 40 B (PLAN.md §3.5)");

impl Node {
    /// Is this a directory?
    pub fn is_dir(&self) -> bool {
        self.flags & flags::DIR != 0
    }
}

/// Result for "no dominant type" / "no age band".
pub const NONE: u8 = 255;

/// Directory histograms: byte distribution by type and age (files only, weighted by `alloc`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Hist {
    /// Percentage of bytes by [`FileType`] (sums to 100 if the directory has bytes). For drawing only.
    pub type_pct: [u8; FileType::COUNT],
    /// Percentage of bytes by age band (sums to 100 if there are bytes). For drawing only.
    pub age_pct: [u8; AGE_BUCKETS],
    /// Dominant type: the one exceeding 50% of bytes (calculated from `u64` values), or [`NONE`] ("Mixed").
    pub(crate) dominant_type: u8,
    /// Age band (see [`Hist::dominant_age`]), or [`NONE`] if there are no bytes.
    pub(crate) dominant_age: u8,
}

const _: () = assert!(size_of::<Hist>() == 14);

impl Hist {
    /// Empty histogram (files and directories with no bytes).
    pub const EMPTY: Hist = Hist {
        type_pct: [0; FileType::COUNT],
        age_pct: [0; AGE_BUCKETS],
        dominant_type: NONE,
        dominant_age: NONE,
    };

    /// Type that STRICTLY exceeds 50% of the directory's bytes; `None` means "Mixed" (or no bytes).
    pub fn dominant_type(&self) -> Option<FileType> {
        FileType::from_index(self.dominant_type)
    }

    /// Directory age band (0 = less than 1 month ... 4 = more than 3 years): the OLDEST band such that at least 80% of
    /// bytes (calculated from `u64` values) fall in that band or an older one. Files with `mtime` 0 (unknown or before
    /// 1970) do not count toward age (they do count toward type). Thus, "92% of this has not changed since 2022" maps to
    /// the band. A directory split evenly between recent and old data gets the recent band (we never claim more age than
    /// the data supports). `None` if there are no bytes.
    pub fn dominant_age(&self) -> Option<usize> {
        (self.dominant_age != NONE).then_some(self.dominant_age as usize)
    }
}

/// Aggregate scan statistics, calculated by the builder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeStats {
    /// Non-directory entries (includes deduplicated hard links; excludes the synthetic node).
    pub files: u64,
    /// Real directories (excluding reparse points), including the root.
    pub dirs: u64,
    /// Directory reparse points (not followed).
    pub reparse_dirs: u64,
    /// Files with the `REPARSE` flag.
    pub reparse_files: u64,
    /// Entries with the `CLOUD` flag.
    pub cloud_entries: u64,
    /// Directories marked `INCOMPLETE`.
    pub incomplete_dirs: u64,
    /// Directories marked `ERROR`.
    pub error_dirs: u64,
    /// Hard-link groups with more than one entry.
    pub hardlink_groups: u64,
    /// Entries that did NOT retain their size (duplicate links).
    pub hardlink_extra_entries: u64,
    /// `alloc` bytes that would have been overcounted without deduplication.
    pub hardlink_alloc_saved: u64,
    /// `logical` bytes that would have been overcounted without deduplication.
    pub hardlink_logical_saved: u64,
    /// Total `alloc` scanned (the root, without the synthetic node).
    pub scanned_alloc: u64,
    /// `alloc` of the synthetic "unexplained space" node (0 if not added).
    pub unexplained_alloc: u64,
    /// Received entries not attached to the root (their parent never arrived): they are not in the tree.
    pub orphans: u64,
}

/// Immutable tree in CSR format.
#[derive(Clone, Debug)]
pub struct Tree {
    pub(crate) nodes: Vec<Node>,
    pub(crate) names: Vec<u8>,
    pub(crate) mtime: Vec<u32>,
    pub(crate) hist: Vec<Hist>,
    pub(crate) stats: TreeStats,
}

impl Tree {
    /// Tree with only an unnamed, `INCOMPLETE` root (not even the root arrived).
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

    /// Number of nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Always `false`: a `Tree` has at least the root.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Root id.
    pub fn root(&self) -> NodeId {
        0
    }

    /// All nodes, indexed by id.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Node `id` (panics if it does not exist).
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    /// Parent of `id` (`None` for the root).
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        let p = self.node(id).parent;
        (p != NO_PARENT).then_some(p)
    }

    /// Child ids of `id`, contiguous and sorted by size.
    pub fn children(&self, id: NodeId) -> Range<NodeId> {
        let n = self.node(id);
        if n.child_count == 0 {
            0..0
        } else {
            n.first_child..n.first_child + n.child_count
        }
    }

    /// Child of `parent` with that exact name (linear search among its children).
    pub fn child_by_name(&self, parent: NodeId, name: &[u8]) -> Option<NodeId> {
        self.children(parent).find(|&c| self.name_bytes(c) == name)
    }

    /// Name as WTF-8 bytes.
    pub fn name_bytes(&self, id: NodeId) -> &[u8] {
        let n = self.node(id);
        &self.names[n.name_off as usize..n.name_off as usize + n.name_len as usize]
    }

    /// Complete name arena in WTF-8 (for scanning it at once, e.g. to detect writes).
    pub fn names_bytes(&self) -> &[u8] {
        &self.names
    }

    /// Name as text (unpaired surrogates are shown as U+FFFD).
    pub fn name_lossy(&self, id: NodeId) -> Cow<'_, str> {
        String::from_utf8_lossy(self.name_bytes(id))
    }

    /// Name in UTF-16, exactly as stored by Windows (including unpaired surrogates).
    pub fn name_utf16(&self, id: NodeId) -> Vec<u16> {
        let mut v = Vec::new();
        wtf8::decode_to_utf16(self.name_bytes(id), &mut v);
        v
    }

    /// Last modification time in Unix seconds.
    pub fn mtime(&self, id: NodeId) -> u32 {
        self.mtime[id as usize]
    }

    /// Node histograms.
    pub fn hist(&self, id: NodeId) -> &Hist {
        &self.hist[id as usize]
    }

    /// Aggregate statistics.
    pub fn stats(&self) -> &TreeStats {
        &self.stats
    }

    /// Full path of `id` in UTF-16: the root name (the scanned path) followed by names through `id`, separated by `\`.
    /// The synthetic node has a fictitious path.
    pub fn path_utf16(&self, id: NodeId) -> Vec<u16> {
        let mut chain = Vec::new();
        let mut cur = id;
        while let Some(p) = self.parent(cur) {
            chain.push(cur);
            cur = p;
            if chain.len() > self.nodes.len() {
                break; // Guard against a corrupt tree.
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

    /// Full path as text (with U+FFFD for unpaired surrogates). For display only.
    pub fn path_lossy(&self, id: NodeId) -> String {
        String::from_utf16_lossy(&self.path_utf16(id))
    }

    /// Full path as `PathBuf`, exact even with unpaired surrogates.
    #[cfg(windows)]
    pub fn path(&self, id: NodeId) -> std::path::PathBuf {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&self.path_utf16(id)).into()
    }

    /// Dynamic memory used by the tree (reserved capacity of its vectors).
    pub fn heap_bytes(&self) -> usize {
        self.nodes.capacity() * size_of::<Node>()
            + self.names.capacity()
            + self.mtime.capacity() * size_of::<u32>()
            + self.hist.capacity() * size_of::<Hist>()
    }

    /// Do the two trees have the same shape and data? Compares each id (name by content, not position in the name arena),
    /// `mtime`, histograms, and statistics. Scans of the same logical tree are structurally equal even if provisional ids
    /// or the name arena differ.
    pub fn structural_eq(&self, other: &Tree) -> bool {
        self.first_difference(other).is_none()
    }

    /// First structural difference between two trees (see [`Tree::structural_eq`]), described as text, or `None`.
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

    /// Check format invariants; returns the first failure. Used by builder and scanner tests. Does not require a
    /// directory's `alloc` to equal the sum of its children (the directory also occupies space itself), only that it is
    /// not smaller.
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
                sum_alloc = sum_alloc.saturating_add(ch.alloc);
                sum_logical = sum_logical.saturating_add(ch.logical);
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
