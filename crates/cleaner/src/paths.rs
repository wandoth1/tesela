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
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// UTF-16 a `String` SIN perder información. NTFS admite nombres con unidades sueltas (un sustituto sin pareja) y
/// `to_string_lossy` los vuelve todos `U+FFFD`: dos nombres distintos darían la misma clave (y una comparación de
/// identidad o de protección acertaría sin deber). Aquí una unidad suelta pasa a un carácter del plano 15 de uso privado
/// (`U+F0000` + su desplazamiento), distinto para cada una.
pub fn wide_to_string(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied())
        .map(|r| {
            r.unwrap_or_else(|e| {
                char::from_u32(0xF0000 + (u32::from(e.unpaired_surrogate()) - 0xD800))
                    .unwrap_or('\u{FFFD}')
            })
        })
        .collect()
}

/// Prefijo de la clave de una ruta mal formada (lleva NUL: ninguna ruta de Windows lo contiene).
const INVALID_MARK: &str = "\u{0}invalid\u{0}";

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
        let wide: Vec<u16> = sys_path.as_os_str().encode_wide().collect();
        let s = wide_to_string(&wide).replace('/', "\\");
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
        // GM-S1: dos formas que NO son una ruta absoluta normal y que antes se "arreglaban" en silencio:
        // * `C:` sin separador detrás (`C:x` es relativo al directorio actual de `C:`, no `C:\x`);
        // * un `:` en un componente posterior (en NTFS abre un flujo de datos alternativo, `archivo:flujo`).
        // Su clave es una marca que no puede salir de ninguna ruta válida: no es una ruta de unidad (`is_drive_path`), no
        // coincide con ninguna otra y [`PathKey::is_valid`] lo dice (la protección la deniega).
        let drive_without_separator = is_drive && !rest.starts_with('\\');
        let colon_in_component = comps.iter().any(|c| c.contains(':'));
        if drive_without_separator || colon_in_component {
            return PathKey(format!("{INVALID_MARK}{}", fold(&s)));
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

    /// ¿Es la clave de una ruta bien formada? `false` para `C:x` (sin separador tras la unidad) y para rutas con `:` en un
    /// componente (flujos alternativos): ver [`PathKey::new`] (GM-S1).
    pub fn is_valid(&self) -> bool {
        !self.0.starts_with(INVALID_MARK)
    }

    /// ¿Tiene la forma de una unidad con letra (letra, dos puntos y barra)? UNC (también hacia un disco propio) y volúmenes sin letra, no.
    /// Es una comprobación de forma, sin consultar al sistema: el tipo de unidad se mira al aprobar.
    pub fn is_drive_path(&self) -> bool {
        let b = self.0.as_bytes();
        b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\'
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

/// A qué elemento de `candidates` corresponde `name` (GM-M5). Una [`PathKey`] pliega mayúsculas, así que en un directorio
/// sensible a mayúsculas (`fsutil file setCaseSensitiveInfo`) `Informe.txt` e `informe.txt` son DOS objetos con la misma clave y
/// una búsqueda por clave coge siempre el primero. Primero el nombre EXACTO; solo si ninguno coincide, por clave (la shell
/// puede dar la misma ruta con otras mayúsculas o forma), y entonces únicamente si hay UN solo candidato con esa clave: con
/// varios no se adivina (`None`: quien llama falla cerrado).
pub fn owner_index<S: AsRef<std::ffi::OsStr>>(
    candidates: &[S],
    name: &std::ffi::OsStr,
) -> Option<usize> {
    if let Some(i) = candidates.iter().position(|c| c.as_ref() == name) {
        return Some(i);
    }
    let key = PathKey::new(Path::new(name));
    let mut found = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| PathKey::new(Path::new(c.as_ref())) == key)
        .map(|(i, _)| i);
    match (found.next(), found.next()) {
        (Some(i), None) => Some(i),
        _ => None,
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
        // GM-S1: `C:` sin separador es relativo al directorio actual de `C:`, no la raíz.
        assert!(!k("C:").is_drive_root() && !k("C:").is_valid());
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
    fn los_nombres_con_unidades_sueltas_no_se_confunden() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;
        let a = OsString::from_wide(&[b'C' as u16, b':' as u16, b'\\' as u16, 0x61, 0xD800]);
        let b = OsString::from_wide(&[b'C' as u16, b':' as u16, b'\\' as u16, 0x61, 0xD801]);
        let c = OsString::from_wide(&[b'C' as u16, b':' as u16, b'\\' as u16, 0x61, 0xFFFD]);
        // Con `to_string_lossy` las tres darían la misma clave.
        assert_eq!(a.to_string_lossy(), b.to_string_lossy());
        let (ka, kb, kc) = (
            PathKey::new(Path::new(&a)),
            PathKey::new(Path::new(&b)),
            PathKey::new(Path::new(&c)),
        );
        assert_ne!(ka, kb);
        assert_ne!(ka, kc);
        assert_ne!(kb, kc);
        // Y el mismo nombre da siempre la misma clave (con y sin verbatim).
        let v = OsString::from_wide(
            &[
                r"\\?\".encode_utf16().collect::<Vec<_>>(),
                a.encode_wide().collect(),
            ]
            .concat(),
        );
        assert_eq!(PathKey::new(Path::new(&v)), ka);
        // Los nombres de hijo del listado van por el mismo camino.
        let child = wide_to_string(&[0x61, 0xD800]);
        assert_eq!(k(r"C:\").join(&child), ka);
        assert_ne!(k(r"C:\").join(&wide_to_string(&[0x61, 0xD801])), ka);
        // Un par válido (fuera del plano básico) se conserva tal cual.
        assert_eq!(wide_to_string(&[0xD83D, 0xDE00]), "\u{1F600}");
    }

    #[test]
    fn rutas_de_red_y_verbatim_con_unc() {
        let net = ["\\\\", "srv", "\\", "share", "\\", "d"].concat();
        let net_verbatim = ["\\\\?\\", "UNC", "\\", "srv", "\\", "share", "\\", "d"].concat();
        assert_eq!(k(&net), k(&net_verbatim));
        assert!(k(&net).is_same_or_under(&k(&["\\\\", "srv", "\\", "share"].concat())));
    }

    /// GM-S1: `C:x` (sin separador tras la unidad) y los `:` en componentes posteriores (flujos alternativos de NTFS) no son
    /// rutas bien formadas. Antes `C:Windows` se "arreglaba" en `c:\windows`; ahora su clave es una marca que no coincide con
    /// ninguna ruta válida, no es una ruta de unidad y `is_valid` lo dice. Lo válido no cambia.
    #[test]
    fn las_rutas_mal_formadas_no_se_arreglan_en_silencio() {
        for malas in [
            r"C:",
            r"C:x",
            r"C:Windows\System32",
            r"C:\a\b:flujo",
            r"C:\archivo.txt:Zone.Identifier",
            r"C:\a:b\c",
        ] {
            let key = k(malas);
            assert!(!key.is_valid(), "{malas}");
            assert!(!key.is_drive_path() && !key.is_drive_root(), "{malas}");
            // No se confunde con la ruta "arreglada".
            assert_ne!(key, k(r"C:\x"), "{malas}");
        }
        assert_ne!(k(r"C:Windows"), k(r"C:\Windows"));
        assert_ne!(k(r"C:\a:b"), k(r"C:\a\b"));
        // Dos distintas mal formadas tampoco coinciden, y la misma sí.
        assert_ne!(k(r"C:x"), k(r"C:y"));
        assert_eq!(k(r"C:x"), k("c:X"));
        // Lo bien formado sigue igual (con barra normal, con `.`/`..`, verbatim y UNC).
        for buenas in [
            r"C:\",
            r"C:\a\b",
            "C:/a/b",
            r"C:\a\..\b",
            r"C:\carpeta con espacios\x",
        ] {
            assert!(k(buenas).is_valid(), "{buenas}");
        }
        assert!(k(r"C:\a\b").is_drive_path());
        // Con `:` en un componente de una ruta de red y con la forma de dispositivo (sin literales de red en el repositorio).
        let net = ["\\", "\\", "servidor", "\\", "recurso", "\\", "a:b"].concat();
        let dev = ["\\", "\\", ".", "\\", "C:", "\\", "x"].concat();
        for malas in [&net, &dev] {
            assert!(!k(malas).is_valid(), "{malas}");
        }
        let verbatim = ["\\", "\\", "?", "\\", "C:", "\\", "a"].concat();
        assert!(k(&verbatim).is_valid(), "{verbatim}");
        assert_eq!(k(&verbatim), k(r"C:\a"));
        // La protección deniega lo mal formado (no lo deja pasar como "permitido").
        let p = crate::protect::Protection::new(&crate::protect::ProtectionRoots::default());
        assert_eq!(
            p.target_verdict(&k(r"C:Windows\x")),
            crate::protect::Verdict::Denied
        );
        assert_eq!(
            p.child_action(&k(r"C:\a:flujo")),
            crate::protect::ChildAction::Skip
        );
    }

    /// GM-M5: `owner_index` busca el nombre exacto, después la clave si es inequívoca, y no adivina entre varios.
    #[test]
    fn el_dueno_de_un_nombre_es_el_exacto_o_el_unico_por_clave() {
        use std::ffi::OsStr;
        let v = [r"C:\d\Informe.txt", r"C:\d\informe.txt", r"C:\d\otro.txt"];
        let own = |n: &str| owner_index(&v, OsStr::new(n));
        assert_eq!(own(r"C:\d\Informe.txt"), Some(0));
        assert_eq!(own(r"C:\d\informe.txt"), Some(1));
        assert_eq!(own(r"C:\d\otro.txt"), Some(2));
        // Sin exacto: por clave, solo si es único (aquí hay dos con la clave `informe.txt`).
        assert_eq!(own(r"C:\d\INFORME.TXT"), None);
        assert_eq!(own(r"c:\D\OTRO.txt"), Some(2));
        assert_eq!(own(r"C:\d\nada.txt"), None);
        assert_eq!(owner_index::<&str>(&[], OsStr::new("x")), None);
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
