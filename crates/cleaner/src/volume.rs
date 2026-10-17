//! Volume queries needed by `cleaner`: volume root, drive type, and the REAL Recycle Bin limit
//! for that volume (the shell registry). Read-only.
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

/// Windows' `%TEMP%` path (`GetTempPathW`).
pub fn temp_path() -> Option<PathBuf> {
    let mut buf = vec![0u16; 1024];
    // SAFETY: `buf` is writable for `buf.len()` UTF-16 units.
    let n = unsafe { GetTempPathW(buf.len() as u32, buf.as_mut_ptr()) } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    Some(OsString::from_wide(&buf[..n]).into())
}

/// Drive type (`GetDriveTypeW`).
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

/// Root of the volume containing `path` (`C:\`), as reported by Windows.
pub fn volume_root(path: &Path) -> Option<PathBuf> {
    let w = wide_nul(path);
    let mut buf = vec![0u16; 1024];
    // SAFETY: `w` is NUL-terminated; `buf` is writable for `buf.len()` UTF-16 units.
    let ok = unsafe { GetVolumePathNameW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if ok == 0 {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(OsString::from_wide(&buf[..n]).into())
}

/// Drive type for the drive whose root is `root` (with a trailing backslash).
pub fn drive_kind(root: &Path) -> DriveKind {
    let w = wide_nul(root);
    // SAFETY: `w` is a NUL-terminated UTF-16 string that remains alive for the call.
    match unsafe { GetDriveTypeW(w.as_ptr()) } {
        2 => DriveKind::Removable,
        3 => DriveKind::Fixed,
        4 => DriveKind::Network,
        5 => DriveKind::CdRom,
        6 => DriveKind::RamDisk,
        _ => DriveKind::Unknown,
    }
}

/// Is `path` (the FINAL path resolved from a handle, with or without `\\?\`) on a LOCAL fixed or removable volume according
/// to `GetDriveTypeW`? Everything else (UNC, including to a local disk via `localhost` or `127.0.0.1`, administrative
/// shares, network drives, volumes without drive letters, and optical disks) is NOT local: `cleaner` rejects it because
/// protection is defined on drive paths, and a network path to the same location could bypass it. The scanner may still read them.
///
/// GM-M6: this is the type of the VOLUME containing the path (`GetVolumePathNameW`), not the drive letter at its start: a
/// folder on `C:` may be a mount point for another volume (removable or optical), and is classified by that volume.
pub fn is_local_path(path: &Path) -> bool {
    is_local_with(path, volume_root, drive_kind)
}

/// [`is_local_path`] with the volume root and type injected (so it can be tested without mounting anything). Fails closed:
/// without a known volume root, the path is not local.
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

/// Volumes mounted in FOLDERS (not on drive letters): roots such as `C:\mounts\data\`. Their `$Recycle.Bin`,
/// `System Volume Information`, `$MFT`, and `pagefile.sys` entries are children of the volume root, which is not a
/// drive-letter root (GM-M6). Returns an empty list if enumeration fails (then only drive-letter roots are known).
pub fn mounted_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut vol = vec![0u16; 64];
    // SAFETY: `vol` is writable for `vol.len()` UTF-16 units.
    let h = unsafe { FindFirstVolumeW(vol.as_mut_ptr(), vol.len() as u32) };
    if h == INVALID_HANDLE_VALUE {
        return out;
    }
    loop {
        let mut names = vec![0u16; 1024];
        let mut needed = 0u32;
        // SAFETY: `vol` is the NUL-terminated volume name returned by enumeration; `names` is writable for `names.len()` units,
        // and `needed` is writable as well.
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
            // SAFETY: same as above, with the enlarged buffer.
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
            // A list of NUL-terminated strings ending with an empty string.
            for s in names.split(|&c| c == 0).take_while(|s| !s.is_empty()) {
                let root = PathBuf::from(OsString::from_wide(s));
                // Keep only roots that are NOT a drive letter followed by a backslash (three UTF-16 units).
                if s.len() > 3 {
                    out.push(root);
                }
            }
        }
        // SAFETY: `h` is the enumeration handle and `vol` is writable for `vol.len()` units.
        if unsafe { FindNextVolumeW(h, vol.as_mut_ptr(), vol.len() as u32) } == 0 {
            break;
        }
    }
    // SAFETY: `h` is the enumeration handle and is closed exactly once.
    unsafe { FindVolumeClose(h) };
    out
}

/// Volume GUID (`{xxxxxxxx-...}`) for the given root.
pub fn volume_guid(root: &Path) -> Option<String> {
    let mut r = wide_nul(root);
    if r.len() >= 2 && r[r.len() - 2] != b'\\' as u16 {
        r.insert(r.len() - 1, b'\\' as u16);
    }
    let mut buf = vec![0u16; 64];
    // SAFETY: `r` is NUL-terminated; `buf` is writable for `buf.len()` UTF-16 units.
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

/// Recycle Bin limit for a volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinLimit {
    /// The volume has no Recycle Bin (network, removable, CD).
    NoBin,
    /// "Don't move files to the Recycle Bin": the user disabled it for this volume.
    NukeOnDelete,
    /// Configured maximum size, in bytes.
    Max(u64),
    /// No registry value: the shell uses its undocumented default percentage.
    Unknown,
    /// The volume could not be identified (unknown drive type or GUID): NEVER treat it as valid.
    Indeterminate,
}

fn reg_dword(hive: HKEY, subkey: &str, value: &str) -> Option<u32> {
    let sk: Vec<u16> = subkey.encode_utf16().chain(Some(0)).collect();
    let v: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `sk` and `v` are NUL-terminated; `data` and `size` are writable, and `size` is exactly the size of `data`;
    // the root key is a predefined constant.
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

/// Recycle Bin limit for the volume rooted at `root`, read from the user's registry
/// (`Explorer\BitBucket\Volume\{guid}`: `MaxCapacity` in MB and `NukeOnDelete`).
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

/// Is the "Don't move deleted files to the Recycle Bin" policy (`NoRecycleFiles`) set for the user (HKCU) or
/// machine (HKLM)? When enabled, the shell permanently deletes anything "sent to the Recycle Bin": treat everything as "won't fit".
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
        // It does not panic and returns one of the four results.
        let _ = bin_limit(&root);
    }

    #[test]
    fn solo_las_rutas_de_unidades_locales_con_letra_son_locales() {
        let tmp = dunce::canonicalize(std::env::temp_dir()).unwrap();
        // The test's temporary directory is on a local drive, with or without the verbatim prefix.
        assert!(is_local_path(&tmp), "{tmp:?}");
        assert!(is_local_path(&PathBuf::from(format!(
            r"\\?\{}",
            tmp.display()
        ))));
        let sub = tmp.join("tesela-test-x").join("a b");
        assert!(is_local_path(&sub));
        let letter = tmp.to_string_lossy().chars().next().unwrap();
        let rest: String = tmp.to_string_lossy().chars().skip(3).collect();
        // The same directory through an administrative share or network path is NOT local.
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
        // Volumes without drive letters, relative paths, roots without separators, devices, and nonexistent drives.
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
        // No volumes are created: type mapping follows GetDriveTypeW.
        assert_eq!(drive_kind(Path::new("ZZ:\\")), DriveKind::Unknown);
        // Anything that cannot be determined is NEVER treated as valid.
        assert_eq!(bin_limit(Path::new("ZZ:\\")), BinLimit::Indeterminate);
    }

    /// GM-M6: the drive type belongs to the VOLUME containing the path, not its drive letter: a directory on `C:` that is a
    /// mount point for an optical or network drive is not local even if `C:` is. Without a known volume root, it is not local.
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
        // Without a known volume root (or with an unknown drive letter), fail closed.
        assert!(!local(r"ZZ:\a.txt"));
        // Anything that is not shaped like a drive-letter path is not local, regardless of what is known about the volume.
        assert!(!local(r"relativa\a.txt"));
        assert!(!local(
            &["\\", "\\", "servidor", "\\", "recurso", "\\", "a.txt"].concat()
        ));
    }

    /// GM-M6: enumerating volumes mounted in directories does not fail and returns only roots that are not drive letters.
    #[test]
    fn los_montajes_en_carpetas_no_incluyen_letras_de_unidad() {
        for r in mounted_roots() {
            let s = r.to_string_lossy().into_owned();
            assert!(s.len() > 3, "{s}");
            assert!(s.ends_with('\\'), "{s}");
        }
    }
}
