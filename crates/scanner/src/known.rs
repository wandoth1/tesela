//! Rutas conocidas que se escanean primero (cola prioritaria, PLAN.md §3.3) y las dos consultas de Windows que
//! necesitan: el SID del usuario actual y la carpeta Descargas. SOLO LECTURA.
#![allow(unsafe_code)]

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath};

/// Longitud de una cadena UTF-16 terminada en NUL.
///
/// # Safety
/// `p` debe apuntar a una cadena UTF-16 válida terminada en NUL.
unsafe fn wcslen(p: *const u16) -> usize {
    let mut n = 0;
    // SAFETY: lo garantiza quien llama: la cadena termina en NUL, así que no se lee más allá del terminador.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    n
}

/// SID del usuario del proceso en texto (`S-1-5-21-...`), o `None` si Windows no lo da.
pub fn current_user_sid() -> Option<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: el pseudo-handle del proceso actual siempre es válido; `token` es escribible.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let sid = (|| {
        // Primera llamada: averigua el tamaño necesario (falla a propósito con ERROR_INSUFFICIENT_BUFFER).
        let mut needed = 0u32;
        // SAFETY: `token` es válido; búfer nulo de longitud 0 es la forma documentada de preguntar el tamaño.
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
        if needed == 0 {
            return None;
        }
        // `u64` para garantizar la alineación que necesita TOKEN_USER.
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        // SAFETY: `buf` es escribible, alineado y de al menos `needed` bytes; `token` es válido.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buf.as_mut_ptr() as *mut _,
                (buf.len() * 8) as u32,
                &mut needed,
            )
        };
        if ok == 0 {
            return None;
        }
        // SAFETY: la llamada anterior rellenó `buf` con un TOKEN_USER (la memoria está alineada a 8 y es válida).
        let sid_ptr = unsafe { (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid };
        let mut wide: *mut u16 = std::ptr::null_mut();
        // SAFETY: `sid_ptr` apunta a un SID válido dentro de `buf`; `wide` es escribible.
        if unsafe { ConvertSidToStringSidW(sid_ptr, &mut wide) } == 0 || wide.is_null() {
            return None;
        }
        // SAFETY: `wide` es una cadena UTF-16 terminada en NUL que reservó Windows; se lee antes de liberarla.
        let text = unsafe { OsString::from_wide(std::slice::from_raw_parts(wide, wcslen(wide))) };
        // SAFETY: `wide` se reservó con LocalAlloc dentro de ConvertSidToStringSidW y se libera una sola vez.
        unsafe { LocalFree(wide as *mut _) };
        text.into_string().ok()
    })();
    // SAFETY: `token` es el handle válido de arriba y se cierra una sola vez.
    unsafe { CloseHandle(token) };
    sid
}

/// Carpeta Descargas del usuario según Windows (puede estar redirigida), o `None` si falla.
pub fn downloads_dir() -> Option<PathBuf> {
    let mut out: *mut u16 = std::ptr::null_mut();
    // SAFETY: `FOLDERID_Downloads` es una constante válida; flags 0 y token nulo = usuario actual; `out` es escribible.
    let hr =
        unsafe { SHGetKnownFolderPath(&FOLDERID_Downloads, 0, std::ptr::null_mut(), &mut out) };
    if hr < 0 || out.is_null() {
        if !out.is_null() {
            // SAFETY: incluso si falla, la documentación pide liberar el puntero que se haya devuelto.
            unsafe { CoTaskMemFree(out as *const _) };
        }
        return None;
    }
    // SAFETY: tras un HRESULT correcto, `out` es una cadena UTF-16 terminada en NUL reservada con CoTaskMemAlloc.
    let path = unsafe { OsString::from_wide(std::slice::from_raw_parts(out, wcslen(out))) };
    // SAFETY: `out` se liberó con el asignador correcto y se libera una sola vez.
    unsafe { CoTaskMemFree(out as *const _) };
    Some(path.into())
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Qué es una ruta conocida (para que la interfaz la nombre).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KnownKind {
    /// `%TEMP%` del usuario.
    UserTemp,
    /// `Windows\Temp`.
    WindowsTemp,
    ChromeCache,
    EdgeCache,
    FirefoxCache,
    /// La carpeta Descargas.
    Downloads,
    /// La carpeta de la Papelera del usuario en el volumen.
    RecycleBin,
}

/// Rutas conocidas candidatas, con su clase (sin comprobar que existan ni que cuelguen de la ruta escaneada):
/// `%TEMP%`, `Windows\Temp`, los perfiles de Chrome, Edge y Firefox, Descargas y la carpeta de la Papelera del
/// usuario actual en el volumen `drive_root` (`C:\`). Esa carpeta solo se LISTA, como todo el escaneo.
pub fn known_paths_labeled(drive_root: Option<&Path>) -> Vec<(KnownKind, PathBuf)> {
    let mut v = Vec::new();
    for var in ["TEMP", "TMP"] {
        v.extend(env_path(var).map(|p| (KnownKind::UserTemp, p)));
    }
    v.extend(env_path("SystemRoot").map(|p| (KnownKind::WindowsTemp, p.join("Temp"))));
    if let Some(local) = env_path("LOCALAPPDATA") {
        v.push((
            KnownKind::ChromeCache,
            local.join(r"Google\Chrome\User Data"),
        ));
        v.push((
            KnownKind::EdgeCache,
            local.join(r"Microsoft\Edge\User Data"),
        ));
        v.push((
            KnownKind::FirefoxCache,
            local.join(r"Mozilla\Firefox\Profiles"),
        ));
    }
    if let Some(roaming) = env_path("APPDATA") {
        v.push((
            KnownKind::FirefoxCache,
            roaming.join(r"Mozilla\Firefox\Profiles"),
        ));
    }
    v.extend(
        downloads_dir()
            .or_else(|| env_path("USERPROFILE").map(|p| p.join("Downloads")))
            .map(|p| (KnownKind::Downloads, p)),
    );
    if let (Some(root), Some(sid)) = (drive_root, current_user_sid()) {
        v.push((KnownKind::RecycleBin, root.join(r"$Recycle.Bin").join(sid)));
    }
    v
}

/// Las mismas rutas sin su clase (lo que usa el escáner para la cola prioritaria).
pub fn known_paths(drive_root: Option<&Path>) -> Vec<PathBuf> {
    known_paths_labeled(drive_root)
        .into_iter()
        .map(|(_, p)| p)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_sid_del_usuario_tiene_la_forma_esperada() {
        let sid = current_user_sid().expect("SID");
        assert!(
            sid.starts_with("S-1-5-21-") || sid.starts_with("S-1-5-"),
            "forma inesperada"
        );
        assert!(
            sid.chars()
                .all(|c| c.is_ascii_digit() || c == 'S' || c == '-')
        );
    }

    #[test]
    fn descargas_es_una_ruta_absoluta() {
        let d = downloads_dir().expect("Descargas");
        assert!(d.is_absolute());
    }

    #[test]
    fn las_rutas_conocidas_incluyen_la_papelera_solo_con_unidad() {
        let with = known_paths(Some(Path::new(r"C:\")));
        assert!(
            with.iter()
                .any(|p| p.to_string_lossy().contains("$Recycle.Bin"))
        );
        let without = known_paths(None);
        assert!(
            !without
                .iter()
                .any(|p| p.to_string_lossy().contains("$Recycle.Bin"))
        );
        assert!(
            without
                .iter()
                .any(|p| p.ends_with(r"Google\Chrome\User Data"))
                || std::env::var_os("LOCALAPPDATA").is_none()
        );
    }

    #[test]
    fn cada_ruta_conocida_lleva_su_clase() {
        let labeled = known_paths_labeled(Some(Path::new(r"C:\")));
        // Mismo contenido y orden que `known_paths`.
        let plain: Vec<PathBuf> = labeled.iter().map(|(_, p)| p.clone()).collect();
        assert_eq!(plain, known_paths(Some(Path::new(r"C:\"))));
        let kind_of = |suffix: &str| {
            labeled
                .iter()
                .find(|(_, p)| p.ends_with(suffix))
                .map(|(k, _)| *k)
        };
        assert_eq!(
            kind_of(r"Google\Chrome\User Data"),
            Some(KnownKind::ChromeCache)
        );
        assert_eq!(
            kind_of(r"Microsoft\Edge\User Data"),
            Some(KnownKind::EdgeCache)
        );
        assert_eq!(
            kind_of(r"Mozilla\Firefox\Profiles"),
            Some(KnownKind::FirefoxCache)
        );
        assert!(labeled.iter().any(|(k, _)| *k == KnownKind::Downloads));
        assert!(labeled.iter().any(|(k, _)| *k == KnownKind::RecycleBin));
        assert!(labeled.iter().any(|(k, _)| *k == KnownKind::UserTemp));
        assert!(labeled.iter().any(|(k, _)| *k == KnownKind::WindowsTemp));
    }
}
