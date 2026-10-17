//! De una ruta del disco al nodo del árbol escaneado (y de vuelta), sin tocar el disco.
//!
//! El nombre de la raíz del árbol es la ruta que se escaneó; el resto son nombres de hijos. Las comparaciones usan la
//! misma clave plegada que el `cleaner` (`PathKey`), así que mayúsculas y prefijos `\\?\` no importan.

use rustc_hash::FxHashMap;
use std::path::Path;
use tesela_cleaner::paths::PathKey;
use tesela_model::{NodeId, Tree};

/// Localizador de nodos por ruta, con un índice por directorio que se construye al usarlo (varias búsquedas en carpetas
/// enormes no son cuadráticas).
pub struct Locator<'a> {
    tree: &'a Tree,
    root: PathKey,
    index: FxHashMap<NodeId, FxHashMap<String, NodeId>>,
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

    /// ¿Está `path` dentro de la ruta que se escaneó (o es ella)? Sin mirar si existe en el árbol.
    pub fn covers(&self, path: &Path) -> bool {
        PathKey::new(path).is_same_or_under(&self.root)
    }

    /// El nodo de `path` si cuelga de la raíz escaneada y existe en el árbol.
    pub fn locate(&mut self, path: &Path) -> Option<NodeId> {
        let key = PathKey::new(path);
        if !key.is_same_or_under(&self.root) {
            return None;
        }
        let mut cur = self.tree.root();
        // Los componentes que hay entre la raíz y la ruta.
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
        // `covers` no mira el árbol: solo si la ruta cuelga de lo escaneado.
        assert!(loc.covers(Path::new(&root)));
        assert!(loc.covers(Path::new(&format!("{root}Users\\nadie"))));
        assert!(loc.covers(Path::new(&upper)));
        assert!(!loc.covers(Path::new(&["Z", ":", "\\fuera"].concat())));
    }
}
