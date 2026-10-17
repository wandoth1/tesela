//! Give a disk path to the scanned tree node (and back), without touching the disk.
//!
//! The tree root name is the path that was scanned; the rest are names of children. The comparisons use the
//! same folded key as `cleaner` (`PathKey`), so capitalization and `\\?\` prefixes don't matter.

use rustc_hash::FxHashMap;
use std::path::Path;
use tesela_cleaner::paths::PathKey;
use tesela_model::{NodeId, Tree};

/// Index of a [`Locator`]: by directory, its children by folded name. Can be retained between frames as long as
/// change the tree (see [`Locator::with_index`] and [`Locator::into_index`]).
pub type Index = FxHashMap<NodeId, FxHashMap<String, NodeId>>;

/// Node locator by path, with a per-directory index that is built using it (several folder searches
/// enormous are not quadratic).
pub struct Locator<'a> {
    tree: &'a Tree,
    root: PathKey,
    index: Index,
}

impl<'a> Locator<'a> {
    pub fn new(tree: &'a Tree) -> Locator<'a> {
        let root = PathKey::new(Path::new(&tree.name_lossy(tree.root()).into_owned()));
        Locator {
            tree,
            root,
            index: FxHashMap::default(),
        }
    }

    /// A locator that starts from an already constructed index (from a previous frame on the SAME tree).
    pub fn with_index(tree: &'a Tree, index: Index) -> Locator<'a> {
        let mut l = Locator::new(tree);
        l.index = index;
        l
    }

    /// The index built so far, to reuse it.
    pub fn into_index(self) -> Index {
        self.index
    }

    fn child(&mut self, dir: NodeId, folded: &str) -> Option<NodeId> {
        let tree = self.tree;
        let idx = self.index.entry(dir).or_insert_with(|| {
            tree.children(dir)
                .map(|c| {
                    let name = PathKey::new(Path::new(&tree.name_lossy(c).into_owned()));
                    (name.as_str().to_string(), c)
                })
                .collect()
        });
        idx.get(folded).copied()
    }

    /// Is `path` within the path that was scanned (or is it)? Without looking to see if it exists in the tree.
    pub fn covers(&self, path: &Path) -> bool {
        PathKey::new(path).is_same_or_under(&self.root)
    }

    /// The `path` node does hang from the scanned root and exists in the tree.
    pub fn locate(&mut self, path: &Path) -> Option<NodeId> {
        let key = PathKey::new(path);
        if !key.is_same_or_under(&self.root) {
            return None;
        }
        let mut cur = self.tree.root();
        // The components between the root and the path.
        let rest = &key.as_str()[self.root.as_str().len()..];
        for part in rest.split('\\').filter(|p| !p.is_empty()) {
            cur = self.child(cur, part)?;
        }
        Some(cur)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::{self, Variant};

    #[test]
    fn localiza_por_ruta_sin_distinguir_mayusculas_ni_prefijo() {
        let t = demo::tree(Variant::Complete);
        let mut loc = Locator::new(&t);
        let root = t.name_lossy(0).into_owned();
        let users = t.child_by_name(0, b"Users").unwrap();
        let me = t.child_by_name(users, demo::USER.as_bytes()).unwrap();
        let p = format!("{root}Users\\{}\\Descargas", demo::USER);
        assert_eq!(loc.locate(Path::new(&p)), t.child_by_name(me, b"Descargas"));
        let upper = p.to_uppercase();
        assert_eq!(
            loc.locate(Path::new(&upper)),
            t.child_by_name(me, b"Descargas")
        );
        let verbatim = format!("\\\\?\\{p}");
        assert_eq!(
            loc.locate(Path::new(&verbatim)),
            t.child_by_name(me, b"Descargas")
        );
        assert_eq!(loc.locate(Path::new(&root)), Some(0));
        assert_eq!(loc.locate(Path::new(&format!("{root}Users\\nadie"))), None);
        assert_eq!(loc.locate(Path::new(&["Z", ":", "\\fuera"].concat())), None);
        // `covers` does not look at the tree: only if the path hangs from what was scanned.
        assert!(loc.covers(Path::new(&root)));
        assert!(loc.covers(Path::new(&format!("{root}Users\\nadie"))));
        assert!(loc.covers(Path::new(&upper)));
        assert!(!loc.covers(Path::new(&["Z", ":", "\\fuera"].concat())));
    }
}
