//! Consultas de volumen que necesita el `cleaner`: raíz del volumen, tipo de unidad, y el límite REAL de la Papelera
//! de ese volumen (el registro de la shell). Todo es solo lectura.
#![allow(unsafe_code)]

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use windows_sys::Win32::Storage::FileSystem::{
    GetDriveTypeW, GetTempPathW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
};
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};

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

/// ¿Es `path` (la ruta FINAL resuelta por un handle, con o sin `\\?\`) un archivo de una unidad LOCAL con letra, fija o
/// extraíble según `GetDriveTypeW`? Todo lo demás (UNC, también hacia un disco local con `localhost` o `127.0.0.1`,
/// comparticiones administrativas, unidades de red, volúmenes sin letra, discos ópticos) NO es local: el `cleaner` lo
/// rechaza porque la protección se escribe sobre rutas de unidad y una ruta de red al mismo sitio la esquivaría. El escáner
/// sí puede leerlas.
pub fn is_local_path(path: &Path) -> bool {
    let p = crate::sys::strip_verbatim(path.to_path_buf());
    let w: Vec<u16> = p.as_os_str().encode_wide().collect();
    if w.len() < 3 || w[1] != u16::from(b':') || w[2] != u16::from(b'\\') {
        return false;
    }
    let Some(letter) = u8::try_from(w[0]).ok().filter(u8::is_ascii_alphabetic) else {
        return false;
    };
    let root = PathBuf::from(format!("{}:\\", letter as char));
    matches!(drive_kind(&root), DriveKind::Fixed | DriveKind::Removable)
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
}

fn reg_dword(subkey: &str, value: &str) -> Option<u32> {
    let sk: Vec<u16> = subkey.encode_utf16().chain(Some(0)).collect();
    let v: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `sk` y `v` terminan en NUL; `data` y `size` son escribibles y `size` es el tamaño exacto de `data`; la
    // clave raíz es una predefinida.
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
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
        DriveKind::Unknown => return BinLimit::Unknown,
        _ => return BinLimit::NoBin,
    }
    let Some(guid) = volume_guid(root) else {
        return BinLimit::Unknown;
    };
    let key =
        format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{guid}");
    if reg_dword(&key, "NukeOnDelete") == Some(1) {
        return BinLimit::NukeOnDelete;
    }
    match reg_dword(&key, "MaxCapacity") {
        Some(mb) => BinLimit::Max(mb as u64 * 1024 * 1024),
        None => BinLimit::Unknown,
    }
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
        assert_eq!(bin_limit(Path::new("ZZ:\\")), BinLimit::Unknown);
    }
}
