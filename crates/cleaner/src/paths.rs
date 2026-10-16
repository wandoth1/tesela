//! Claves de comparación de rutas.
//!
//! Toda comparación de rutas del `cleaner` (protección, normalización de la cesta, raíces de categoría) se hace sobre
//! [`PathKey`]: la forma CANÓNICA LARGA de la ruta, sin `\\?\`, con `\` como único separador, sin barra final (salvo la
//! raíz de unidad) y con las mayúsculas plegadas. Una clave de una ruta con nombres cortos 8.3 solo es fiable si la ruta
//! se canonicalizó antes con [`canonical_path`] (el CI de GitHub tiene `%TEMP%` en forma 8.3).
//!
//! El plegado es "a mayúsculas y luego a minúsculas" por carácter: iguala más nombres de los que iguala NTFS, que para
//! una lista de protección es el lado seguro (se protege de más, nunca de menos).

use crate::sys;
use std::path::{Path, PathBuf};

/// Clave de comparación de una ruta (ver el módulo).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PathKey(String);

fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase)
        .collect()
}

impl PathKey {
    /// Clave léxica de `path`: quita `\\?\`, unifica separadores, resuelve `.` y `..` y pliega mayúsculas. NO resuelve
    /// nombres 8.3 ni enlaces: para eso, [`canonical_key`].
    pub fn new(path: &Path) -> PathKey {
        let sys_path = sys::strip_verbatim(path.to_path_buf());
        let s = sys_path.to_string_lossy().replace('/', "\\");
        // Prefijo: letra de unidad (`c:`) o el inicio de una ruta de red (dos barras).
        let (prefix, rest, is_drive) = if s.len() >= 2 && s.as_bytes()[1] == b':' {
            (s[..2].to_string(), &s[2..], true)
        } else if let Some(r) = s.strip_prefix("\\\\") {
            ("\\\\".to_string(), r, false)
        } else {
            (String::new(), s.as_str(), false)
        };
        let mut comps: Vec<String> = Vec::new();
        for c in rest.split('\\') {
            match c {
                "" | "." => {}
                ".." => {
                    comps.pop();
                }
                other => comps.push(fold(other)),
            }
        }
        let mut out = fold(&prefix);
        if is_drive {
            out.push('\\');
        }
        out.push_str(&comps.join("\\"));
        PathKey(out)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// ¿Es la raíz de una unidad (`c:\`)?
    pub fn is_drive_root(&self) -> bool {
        self.0.len() == 3 && self.0.as_bytes()[1] == b':' && self.0.ends_with('\\')
    }

    /// Último componente (vacío en una raíz).
    pub fn file_name(&self) -> &str {
        if self.is_drive_root() {
            return "";
        }
        self.0.rsplit('\\').next().unwrap_or("")
    }

    /// Clave del directorio que lo contiene (`None` en una raíz).
    pub fn parent(&self) -> Option<PathKey> {
        if self.is_drive_root() || self.0.is_empty() {
            return None;
        }
        let cut = self.0.rfind('\\')?;
        let head = &self.0[..cut];
        if head.is_empty() || head == "\\" || head == "\\\\" {
            return None;
        }
        // En una ruta de red la raíz es la del recurso (dos barras, servidor y recurso): "servidor" no es un contenedor.
        if let Some(rest) = head.strip_prefix("\\\\")
            && !rest.contains('\\')
        {
            return None;
        }
        // `c:` + `\` = la raíz de la unidad.
        if head.len() == 2 && head.as_bytes()[1] == b':' {
            return Some(PathKey(format!("{head}\\")));
        }
        Some(PathKey(head.to_string()))
    }

    /// `self` es `root` o cuelga de él (con límite de componente: `c:\ab` no cuelga de `c:\a`).
    pub fn is_same_or_under(&self, root: &PathKey) -> bool {
        if self.0 == root.0 {
            return true;
        }
        if root.0.is_empty() {
            return false;
        }
        if root.0.ends_with('\\') {
            self.0.starts_with(&root.0)
        } else {
            self.0.len() > root.0.len()
                && self.0.starts_with(&root.0)
                && self.0.as_bytes()[root.0.len()] == b'\\'
        }
    }

    /// `self` es un ancestro PROPIO de `other`.
    pub fn is_proper_ancestor_of(&self, other: &PathKey) -> bool {
        self != other && other.is_same_or_under(self)
    }

    /// Clave del hijo de nombre `name` (un solo componente).
    pub fn join(&self, name: &str) -> PathKey {
        let folded = fold(name);
        if self.0.ends_with('\\') {
            PathKey(format!("{}{}", self.0, folded))
        } else {
            PathKey(format!("{}\\{}", self.0, folded))
        }
    }

    /// Ancestros propios, del más cercano al más lejano.
    pub fn ancestors(&self) -> Vec<PathKey> {
        let mut v = Vec::new();
        let mut cur = self.clone();
        while let Some(p) = cur.parent() {
            v.push(p.clone());
            cur = p;
        }
        v
    }
}

/// Ruta canónica larga de un objeto EXISTENTE (sin seguirlo si es un enlace): se abre por handle y se pregunta a
/// Windows (`GetFinalPathNameByHandleW`). Resuelve nombres 8.3, mayúsculas y los enlaces de los ANCESTROS.
pub fn canonical_path(path: &Path) -> Result<PathBuf, sys::Win32> {
    let h = sys::open_for_query(path)?;
    sys::final_path(&h)
}

/// Clave de la ruta canónica larga de `path` (ver [`canonical_path`]).
pub fn canonical_key(path: &Path) -> Result<PathKey, sys::Win32> {
    canonical_path(path).map(|p| PathKey::new(&p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Sandbox;

    fn k(s: &str) -> PathKey {
        PathKey::new(Path::new(s))
    }

    #[test]
    fn normaliza_prefijo_separadores_mayusculas_y_barra_final() {
        let a = k(r"C:\Users\<usuario>\AppData\");
        assert_eq!(a, k(r"\\?\c:\users\<usuario>\appdata"));
        assert_eq!(a, k("C:/Users/<usuario>/AppData"));
        assert_eq!(a.as_str(), r"c:\users\<usuario>\appdata");
        assert_eq!(k(r"C:\a\.\b\..\c"), k(r"c:\a\c"));
        assert!(k(r"C:\").is_drive_root());
        assert!(k("C:").is_drive_root());
        assert!(!k(r"C:\a").is_drive_root());
        assert_eq!(k(r"C:\").as_str(), r"c:\");
        // Plegado fuera de ASCII.
        assert_eq!(k(r"C:\Carpeta\ÁRBOL"), k(r"c:\carpeta\árbol"));
    }

    #[test]
    fn relaciones_con_limite_de_componente() {
        let a = k(r"C:\a");
        assert!(k(r"C:\a\b").is_same_or_under(&a));
        assert!(a.is_same_or_under(&a));
        assert!(
            !k(r"C:\ab").is_same_or_under(&a),
            "c:\\ab no cuelga de c:\\a"
        );
        assert!(a.is_proper_ancestor_of(&k(r"C:\a\b\c")));
        assert!(!a.is_proper_ancestor_of(&a));
        assert!(k(r"C:\x").is_same_or_under(&k(r"C:\")));
        assert!(k(r"C:\").is_proper_ancestor_of(&k(r"C:\x")));
        assert!(!k(&["D", r":\x"].concat()).is_same_or_under(&k(r"C:\")));
        assert_eq!(k(r"C:\a\b").parent(), Some(k(r"C:\a")));
        assert_eq!(k(r"C:\a").parent(), Some(k(r"C:\")));
        assert_eq!(k(r"C:\").parent(), None);
        assert_eq!(k(r"C:\a\b").file_name(), "b");
        assert_eq!(k(r"C:\a").join("B"), k(r"C:\a\b"));
        assert_eq!(k(r"C:\").join("X"), k(r"C:\x"));
        let anc = k(r"C:\a\b\c").ancestors();
        assert_eq!(anc, vec![k(r"C:\a\b"), k(r"C:\a"), k(r"C:\")]);
    }

    #[test]
    fn rutas_de_red_y_verbatim_con_unc() {
        let net = ["\\\\", "srv", "\\", "share", "\\", "d"].concat();
        let net_verbatim = ["\\\\?\\", "UNC", "\\", "srv", "\\", "share", "\\", "d"].concat();
        assert_eq!(k(&net), k(&net_verbatim));
        assert!(k(&net).is_same_or_under(&k(&["\\\\", "srv", "\\", "share"].concat())));
    }

    #[test]
    fn la_ruta_canonica_resuelve_8_3_y_mayusculas_y_no_sigue_el_enlace_final() {
        let mut sb = Sandbox::new();
        sb.mkdir("Carpeta Con Nombre Largo/Sub Carpeta Larga");
        let long = sb.long("Carpeta Con Nombre Largo/Sub Carpeta Larga");
        // Mayúsculas distintas: misma clave canónica.
        let upper = std::path::PathBuf::from(long.to_string_lossy().to_uppercase());
        assert_eq!(canonical_key(&upper).unwrap(), PathKey::new(&long));
        // La forma 8.3 (si el volumen la tiene activada) llega a la misma clave.
        let short = crate::testutil::short_path(&long);
        assert_eq!(canonical_key(&short).unwrap(), PathKey::new(&long));
        assert_eq!(
            canonical_key(&sb.path("Carpeta Con Nombre Largo")).unwrap(),
            PathKey::new(&sb.long("Carpeta Con Nombre Largo"))
        );
        // Un junction no se sigue: su clave es la del propio enlace.
        sb.junction("enlace", "Carpeta Con Nombre Largo");
        assert_eq!(
            canonical_key(&sb.path("enlace")).unwrap(),
            PathKey::new(&sb.long("enlace"))
        );
        // Un archivo inexistente falla.
        assert!(canonical_key(&sb.path("no-existe")).is_err());
    }
}
