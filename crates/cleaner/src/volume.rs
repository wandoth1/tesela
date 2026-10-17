//! Consultas de volumen que necesita el `cleaner`: raíz del volumen, tipo de unidad, y el límite REAL de la Papelera
//! de ese volumen (el registro de la shell). Todo es solo lectura.
#![allow(unsafe_code)]

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::{
    FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, GetDriveTypeW, GetTempPathW,
    GetVolumeNameForVolumeMountPointW, GetVolumePathNameW, GetVolumePathNamesForVolumeNameW,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW,
};

/// `%TEMP%` según Windows (`GetTempPathW`).
pub fn temp_path() -> Option<PathBuf> {
    let mut buf = vec![0u16; 1024];
    // SAFETY: `buf` es escribible con `buf.len()` unidades UTF-16.
    let n = unsafe { GetTempPathW(buf.len() as u32, buf.as_mut_ptr()) } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    Some(OsString::from_wide(&buf[..n]).into())
}

/// Tipo de unidad (`GetDriveTypeW`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    Fixed,
    Removable,
    Network,
    CdRom,
    RamDisk,
    Unknown,
}

fn wide_nul(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// Raíz del volumen que contiene `path` (`C:\`), según Windows.
pub fn volume_root(path: &Path) -> Option<PathBuf> {
    let w = wide_nul(path);
    let mut buf = vec![0u16; 1024];
    // SAFETY: `w` termina en NUL; `buf` es escribible con `buf.len()` unidades UTF-16.
    let ok = unsafe { GetVolumePathNameW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if ok == 0 {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(OsString::from_wide(&buf[..n]).into())
}

/// Tipo de la unidad cuya raíz es `root` (con barra final).
pub fn drive_kind(root: &Path) -> DriveKind {
    let w = wide_nul(root);
    // SAFETY: `w` es una cadena UTF-16 terminada en NUL que vive durante la llamada.
    match unsafe { GetDriveTypeW(w.as_ptr()) } {
        2 => DriveKind::Removable,
        3 => DriveKind::Fixed,
        4 => DriveKind::Network,
        5 => DriveKind::CdRom,
        6 => DriveKind::RamDisk,
        _ => DriveKind::Unknown,
    }
}

/// ¿Es `path` (la ruta FINAL resuelta por un handle, con o sin `\?\`) un archivo de un volumen LOCAL, fijo o extraíble según
/// `GetDriveTypeW`? Todo lo demás (UNC, también hacia un disco local con `localhost` o `127.0.0.1`, comparticiones
/// administrativas, unidades de red, volúmenes sin letra, discos ópticos) NO es local: el `cleaner` lo rechaza porque la
/// protección se escribe sobre rutas de unidad y una ruta de red al mismo sitio la esquivaría. El escáner sí puede leerlas.
///
/// GM-M6: el tipo es el del VOLUMEN que contiene la ruta (`GetVolumePathNameW`), no el de la letra con la que empieza: una
/// carpeta de `C:` puede ser el punto de montaje de otro volumen (extraíble, óptico) y se juzga por ese.
pub fn is_local_path(path: &Path) -> bool {
    is_local_with(path, volume_root, drive_kind)
}

/// [`is_local_path`] con la raíz del volumen y su tipo inyectados (así se prueba sin montar nada). Falla cerrado: sin raíz de
/// volumen conocida no es local.
pub(crate) fn is_local_with(
    path: &Path,
    root_of: impl Fn(&Path) -> Option<PathBuf>,
    kind_of: impl Fn(&Path) -> DriveKind,
) -> bool {
    let p = crate::sys::strip_verbatim(path.to_path_buf());
    let w: Vec<u16> = p.as_os_str().encode_wide().collect();
    if w.len() < 3 || w[1] != u16::from(b':') || w[2] != u16::from(b'\\') {
        return false;
    }
    if u8::try_from(w[0])
        .ok()
        .filter(u8::is_ascii_alphabetic)
        .is_none()
    {
        return false;
    }
    let Some(root) = root_of(&p) else {
        return false;
    };
    matches!(kind_of(&root), DriveKind::Fixed | DriveKind::Removable)
}

/// Los volúmenes montados en CARPETAS (no en una letra): raíces como `C:\montajes\datos\`. Dentro de ellos `$Recycle.Bin`,
/// `System Volume Information`, `$MFT` y `pagefile.sys` cuelgan de la raíz del volumen, que no es la de una unidad con letra
/// (GM-M6). Lista vacía si no se puede enumerar (entonces solo se conocen las raíces con letra).
pub fn mounted_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut vol = vec![0u16; 64];
    // SAFETY: `vol` es escribible con `vol.len()` unidades UTF-16.
    let h = unsafe { FindFirstVolumeW(vol.as_mut_ptr(), vol.len() as u32) };
    if h == INVALID_HANDLE_VALUE {
        return out;
    }
    loop {
        let mut names = vec![0u16; 1024];
        let mut needed = 0u32;
        // SAFETY: `vol` es el nombre de volumen (terminado en NUL) que dio la enumeración; `names` es escribible con
        // `names.len()` unidades y `needed` también.
        let mut ok = unsafe {
            GetVolumePathNamesForVolumeNameW(
                vol.as_ptr(),
                names.as_mut_ptr(),
                names.len() as u32,
                &mut needed,
            )
        };
        if ok == 0 && needed as usize > names.len() {
            names = vec![0u16; needed as usize];
            // SAFETY: igual que arriba con el búfer ampliado.
            ok = unsafe {
                GetVolumePathNamesForVolumeNameW(
                    vol.as_ptr(),
                    names.as_mut_ptr(),
                    names.len() as u32,
                    &mut needed,
                )
            };
        }
        if ok != 0 {
            // Una lista de cadenas terminadas en NUL que acaba con una vacía.
            for s in names.split(|&c| c == 0).take_while(|s| !s.is_empty()) {
                let root = PathBuf::from(OsString::from_wide(s));
                // Solo las que NO son una letra de unidad con su barra (tres unidades UTF-16).
                if s.len() > 3 {
                    out.push(root);
                }
            }
        }
        // SAFETY: `h` es el handle de la enumeración y `vol` es escribible con `vol.len()` unidades.
        if unsafe { FindNextVolumeW(h, vol.as_mut_ptr(), vol.len() as u32) } == 0 {
            break;
        }
    }
    // SAFETY: `h` es el handle de la enumeración, que se cierra una sola vez.
    unsafe { FindVolumeClose(h) };
    out
}

/// GUID del volumen (`{xxxxxxxx-...}`), a partir de su raíz.
pub fn volume_guid(root: &Path) -> Option<String> {
    let mut r = wide_nul(root);
    if r.len() >= 2 && r[r.len() - 2] != b'\\' as u16 {
        r.insert(r.len() - 1, b'\\' as u16);
    }
    let mut buf = vec![0u16; 64];
    // SAFETY: `r` termina en NUL; `buf` es escribible con `buf.len()` unidades UTF-16.
    let ok = unsafe {
        GetVolumeNameForVolumeMountPointW(r.as_ptr(), buf.as_mut_ptr(), buf.len() as u32)
    };
    if ok == 0 {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..n]);
    let start = s.find('{')?;
    let end = s.find('}')?;
    (end > start).then(|| s[start..=end].to_string())
}

/// Límite de la Papelera de un volumen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinLimit {
    /// El volumen no tiene Papelera (red, extraíble, CD).
    NoBin,
    /// "No mover los archivos a la Papelera": el usuario lo desactivó para el volumen.
    NukeOnDelete,
    /// Tamaño máximo configurado, en bytes.
    Max(u64),
    /// No hay valor en el registro: la shell usa su porcentaje por defecto, que no se documenta.
    Unknown,
    /// No se pudo averiguar nada del volumen (tipo de unidad o GUID desconocidos): NUNCA se da por bueno.
    Indeterminate,
}

fn reg_dword(hive: HKEY, subkey: &str, value: &str) -> Option<u32> {
    let sk: Vec<u16> = subkey.encode_utf16().chain(Some(0)).collect();
    let v: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `sk` y `v` terminan en NUL; `data` y `size` son escribibles y `size` es el tamaño exacto de `data`; la
    // clave raíz es una predefinida.
    let rc = unsafe {
        RegGetValueW(
            hive,
            sk.as_ptr(),
            v.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut data as *mut u32 as *mut _,
            &mut size,
        )
    };
    (rc == 0).then_some(data)
}

/// El límite de la Papelera del volumen cuya raíz es `root`, leído del registro del usuario
/// (`Explorer\BitBucket\Volume\{guid}`: `MaxCapacity` en MB y `NukeOnDelete`).
pub fn bin_limit(root: &Path) -> BinLimit {
    match drive_kind(root) {
        DriveKind::Fixed | DriveKind::RamDisk => {}
        DriveKind::Unknown => return BinLimit::Indeterminate,
        _ => return BinLimit::NoBin,
    }
    let Some(guid) = volume_guid(root) else {
        return BinLimit::Indeterminate;
    };
    let key =
        format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{guid}");
    if reg_dword(HKEY_CURRENT_USER, &key, "NukeOnDelete") == Some(1) {
        return BinLimit::NukeOnDelete;
    }
    match reg_dword(HKEY_CURRENT_USER, &key, "MaxCapacity") {
        Some(mb) => BinLimit::Max(mb as u64 * 1024 * 1024),
        None => BinLimit::Unknown,
    }
}

/// ¿Hay una directiva "No mover los archivos eliminados a la Papelera" (`NoRecycleFiles`) en el usuario (HKCU) o en el
/// equipo (HKLM)? Con ella la shell elimina de verdad lo que se "envía a la Papelera": todo cuenta como "no cabe".
pub fn recycle_policy_disabled() -> bool {
    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer";
    [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
        .into_iter()
        .any(|h| reg_dword(h, KEY, "NoRecycleFiles").is_some_and(|v| v != 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raiz_tipo_y_guid_del_volumen_del_temporal() {
        let tmp = dunce::canonicalize(std::env::temp_dir()).unwrap();
        let root = volume_root(&tmp).expect("raíz del volumen");
        assert!(root.to_string_lossy().ends_with('\\'));
        assert_ne!(drive_kind(&root), DriveKind::Unknown);
        if let Some(g) = volume_guid(&root) {
            assert!(g.starts_with('{') && g.ends_with('}'), "{g}");
        }
        // No panica y da una de las cuatro respuestas.
        let _ = bin_limit(&root);
    }

    #[test]
    fn solo_las_rutas_de_unidades_locales_con_letra_son_locales() {
        let tmp = dunce::canonicalize(std::env::temp_dir()).unwrap();
        // El temporal de la prueba está en una unidad local, con o sin prefijo verbatim.
        assert!(is_local_path(&tmp), "{tmp:?}");
        assert!(is_local_path(&PathBuf::from(format!(
            r"\\?\{}",
            tmp.display()
        ))));
        let sub = tmp.join("tesela-test-x").join("a b");
        assert!(is_local_path(&sub));
        let letter = tmp.to_string_lossy().chars().next().unwrap();
        let rest: String = tmp.to_string_lossy().chars().skip(3).collect();
        // La misma carpeta por la compartición administrativa o por una ruta de red: NO es local.
        for host in ["localhost", "127.0.0.1", "[::1]", "equipo.invalid"] {
            let sep = '\\';
            for p in [
                format!("{sep}{sep}{host}{sep}{letter}${sep}{rest}"),
                format!("{sep}{sep}?{sep}UNC{sep}{host}{sep}{letter}${sep}{rest}"),
                format!("{sep}{sep}{host}{sep}compartida{sep}{rest}"),
            ] {
                assert!(!is_local_path(Path::new(&p)), "{p}");
            }
        }
        // Volumen sin letra, rutas relativas, raíces sin barra, dispositivos y unidades que no existen.
        for p in [
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\x",
            r"\\.\C:\x",
            r"relativa\x",
            r"C:",
            r"C:x",
            r"1:\x",
            "",
        ] {
            assert!(!is_local_path(Path::new(p)), "{p}");
        }
        let nonexistent = ["Q", r":\x"].concat();
        if drive_kind(Path::new(&["Q", r":\"].concat())) == DriveKind::Unknown {
            assert!(!is_local_path(Path::new(&nonexistent)));
        }
    }

    #[test]
    fn una_unidad_de_red_o_extraible_no_tiene_papelera_por_su_tipo() {
        // Sin crear ninguna: el mapeo de tipos es el de GetDriveTypeW.
        assert_eq!(drive_kind(Path::new("ZZ:\\")), DriveKind::Unknown);
        // Lo que no se puede averiguar NUNCA se da por bueno.
        assert_eq!(bin_limit(Path::new("ZZ:\\")), BinLimit::Indeterminate);
    }

    /// GM-M6: el tipo de unidad es el del VOLUMEN que contiene la ruta, no el de su letra: una carpeta de `C:` que es el punto
    /// de montaje de un disco óptico o de red no es local aunque `C:` sí lo sea. Sin raíz de volumen conocida, no es local.
    #[test]
    fn el_tipo_de_unidad_es_el_del_volumen_que_contiene_la_ruta() {
        let root_of = |p: &Path| -> Option<PathBuf> {
            let s = p.to_string_lossy().to_lowercase();
            if s.starts_with(r"c:\montajes\optico") {
                Some(PathBuf::from(r"C:\montajes\optico\"))
            } else if s.starts_with(r"c:\montajes\ext") {
                Some(PathBuf::from(r"C:\montajes\ext\"))
            } else if s.starts_with(r"c:\") {
                Some(PathBuf::from(r"C:\"))
            } else {
                None
            }
        };
        let kind_of = |r: &Path| -> DriveKind {
            match r.to_string_lossy().to_lowercase().as_str() {
                r"c:\" => DriveKind::Fixed,
                r"c:\montajes\ext\" => DriveKind::Removable,
                r"c:\montajes\optico\" => DriveKind::CdRom,
                _ => DriveKind::Unknown,
            }
        };
        let local = |p: &str| is_local_with(Path::new(p), root_of, kind_of);
        assert!(local(r"C:\datos\a.txt"));
        assert!(local(r"C:\montajes\ext\a.txt"), "extraíble montado: local");
        assert!(
            !local(r"C:\montajes\optico\a.txt"),
            "óptico montado en una carpeta de C:"
        );
        // Sin raíz de volumen conocida (o con letra desconocida) falla cerrado.
        assert!(!local(r"ZZ:.txt"));
        // Lo que no tiene forma de unidad con letra no es local, sepa lo que se sepa del volumen.
        assert!(!local(r"relativa\a.txt"));
        assert!(!local(r"\servidor\recurso\a.txt"));
    }

    /// GM-M6: la enumeración de volúmenes montados en carpetas no falla y solo devuelve raíces que no son una letra de unidad.
    #[test]
    fn los_montajes_en_carpetas_no_incluyen_letras_de_unidad() {
        for r in mounted_roots() {
            let s = r.to_string_lossy().into_owned();
            assert!(s.len() > 3, "{s}");
            assert!(s.ends_with('\\'), "{s}");
        }
    }
}
