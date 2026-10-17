//! Volume-root path handling (pure functions that do not touch the disk).

use std::path::{Component, Path, PathBuf, Prefix};

/// The drive letter if `p` is exactly a drive root (`C:\` or `\\?\C:\`), or the path itself if it is a
/// GUID volume root (`\\?\Volume{...}\`: a volume mounted in a folder without a drive letter is scanned by that path).
pub(crate) fn drive_root(p: &Path) -> Option<PathBuf> {
    let mut it = p.components();
    let Some(Component::Prefix(prefix)) = it.next() else {
        return None;
    };
    let letter = match prefix.kind() {
        Prefix::Disk(l) | Prefix::VerbatimDisk(l) => l,
        _ => return volume_guid_root(p),
    };
    (it.next() == Some(Component::RootDir) && it.next().is_none())
        .then(|| PathBuf::from(format!("{}:\\", letter as char)))
}

/// Drive-letter root that contains `p`, even if `p` is not itself the root.
pub(crate) fn drive_of(p: &Path) -> Option<PathBuf> {
    match p.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(l) | Prefix::VerbatimDisk(l) => {
                Some(PathBuf::from(format!("{}:\\", l as char)))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Does `name` match the form `Volume{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}` (case-insensitive)?
fn is_volume_guid_name(name: &str) -> bool {
    let Some(rest) = name
        .get(..7)
        .filter(|p| p.eq_ignore_ascii_case("Volume{"))
        .map(|_| &name[7..])
    else {
        return false;
    };
    let Some(guid) = rest.strip_suffix('}') else {
        return false;
    };
    guid.len() == 36
        && guid.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// `\\?\Volume{GUID}\` if `p` is EXACTLY a GUID volume root (with nothing after it), or `None`.
pub(crate) fn volume_guid_root(p: &Path) -> Option<PathBuf> {
    let mut it = p.components();
    let Some(Component::Prefix(prefix)) = it.next() else {
        return None;
    };
    let Prefix::Verbatim(name) = prefix.kind() else {
        return None;
    };
    let name = name.to_str()?;
    (is_volume_guid_name(name) && it.next() == Some(Component::RootDir) && it.next().is_none())
        .then(|| PathBuf::from(format!(r"\\?\{name}\")))
}

/// Select the scan root from the two names Windows returns for the same handle: the DOS name
/// (`VOLUME_NAME_DOS`) and the GUID name (`VOLUME_NAME_GUID`).
///
/// A volume mounted ONLY in a folder (without a drive letter) and opened through `\\?\Volume{GUID}\` has the
/// mount folder as its DOS name. Traversing that path would reopen the parent volume's (empty) mount-point directory
/// rather than the volume itself.
/// If the GUID form is exactly a volume root and the DOS form is not a drive-letter root, keep
/// the GUID form. Otherwise (normal paths, a drive-letter root, and so on), use the DOS form.
pub(crate) fn choose_root_path(dos: PathBuf, guid: &Path) -> PathBuf {
    match volume_guid_root(guid) {
        Some(g) if drive_root(&dos).is_none_or(|d| volume_guid_root(&d).is_some()) => g,
        _ => dos,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUID: &str = r"\\?\Volume{0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9}\";

    #[test]
    fn std_trocea_la_ruta_de_volumen_como_prefijo_verbatim_y_raiz() {
        // La premisa de `volume_guid_root`: así trocea `std` esta ruta.
        let mut it = Path::new(GUID).components();
        match it.next() {
            Some(Component::Prefix(p)) => assert_eq!(
                p.kind(),
                Prefix::Verbatim(std::ffi::OsStr::new(
                    "Volume{0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9}"
                ))
            ),
            other => panic!("{other:?}"),
        }
        assert_eq!(it.next(), Some(Component::RootDir));
        assert_eq!(it.next(), None);
    }

    #[test]
    fn la_raiz_de_un_volumen_por_guid_se_reconoce_y_lo_demas_no() {
        assert_eq!(volume_guid_root(Path::new(GUID)), Some(PathBuf::from(GUID)));
        // Sin la barra final es el dispositivo del volumen (no su sistema de archivos): no es una raíz recorrible.
        assert_eq!(
            volume_guid_root(Path::new(GUID.trim_end_matches('\\'))),
            None
        );
        // Mayúsculas en `Volume` y en el GUID.
        assert!(
            volume_guid_root(Path::new(
                r"\\?\VOLUME{0A1B2C3D-4E5F-6071-8293-A4B5C6D7E8F9}\"
            ))
            .is_some()
        );
        // Con algo detrás NO es la raíz.
        assert_eq!(volume_guid_root(Path::new(&format!("{GUID}carpeta"))), None);
        // No es un GUID bien formado, ni un volumen, ni una unidad.
        for p in [
            r"\\?\Volume{zzzz}\",
            r"\\?\Volume{0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f}\",
            r"\\?\Volume0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9\",
            r"\\?\C:\",
            r"C:\",
            r"\\?\UNC\<servidor>\<recurso>\",
            r"\\<servidor>\<recurso>\",
            r"\\?\GLOBALROOT\Device\X\",
            "relativa",
        ] {
            assert_eq!(volume_guid_root(Path::new(p)), None, "{p}");
        }
    }

    #[test]
    fn drive_root_reconoce_la_unidad_y_el_volumen_por_guid() {
        assert_eq!(drive_root(Path::new(r"C:\")), Some(PathBuf::from(r"C:\")));
        assert_eq!(
            drive_root(Path::new(r"\\?\c:\")),
            Some(PathBuf::from(r"c:\"))
        );
        assert_eq!(drive_root(Path::new(r"C:\Users")), None);
        assert_eq!(drive_root(Path::new(r"\\?\C:\Users")), None);
        assert_eq!(drive_root(Path::new(GUID)), Some(PathBuf::from(GUID)));
        assert_eq!(drive_root(Path::new(&format!("{GUID}x"))), None);
        assert_eq!(drive_root(Path::new(r"\\?\UNC\<s>\<r>\")), None);
        // `drive_of` solo conoce las letras: un volumen por GUID no tiene "unidad" (no hay Papelera por letra).
        assert_eq!(
            drive_of(Path::new(r"\\?\C:\a\b")),
            Some(PathBuf::from(r"C:\"))
        );
        assert_eq!(drive_of(Path::new(GUID)), None);
    }

    #[test]
    fn un_volumen_montado_en_una_carpeta_conserva_su_ruta_de_guid() {
        // Raíz de volumen abierta por GUID cuyo nombre DOS es la carpeta de montaje: se queda la ruta de GUID.
        let dos = PathBuf::from(r"\\?\C:\Montajes\datos");
        assert_eq!(
            choose_root_path(dos.clone(), Path::new(GUID)),
            Path::new(GUID)
        );
        // Con letra asignada, el nombre DOS es la raíz de la unidad: se usa.
        let letter = PathBuf::from(r"\\?\C:\");
        assert_eq!(choose_root_path(letter.clone(), Path::new(GUID)), letter);
        // Una ruta que no es la raíz del volumen (GUID con algo detrás) usa la de DOS aunque cuelgue de una carpeta de montaje.
        let deep_guid = format!("{GUID}datos\\sub");
        let deep_dos = PathBuf::from(r"\\?\C:\Montajes\datos\datos\sub");
        assert_eq!(
            choose_root_path(deep_dos.clone(), Path::new(&deep_guid)),
            deep_dos
        );
        // Una carpeta normal: DOS.
        let normal = PathBuf::from(r"\\?\C:\Datos\a");
        assert_eq!(
            choose_root_path(
                normal.clone(),
                Path::new(r"\\?\Volume{0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9}\Users\a")
            ),
            normal
        );
        // La forma de GUID que no es de GUID (mal formada) nunca sustituye a la de DOS.
        assert_eq!(
            choose_root_path(dos.clone(), Path::new(r"\\?\Volume{zzz}\")),
            dos
        );
    }
}
