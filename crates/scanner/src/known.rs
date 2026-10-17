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
use windows_sys::core::GUID;

use crate::browser::{BrowserRoots, browser_cache_dirs_verified};
use crate::diagnostic::diagnostic_dirs_verified;

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

/// Una carpeta conocida de Windows (`SHGetKnownFolderPath`, usuario actual; puede estar redirigida), o `None` si falla.
pub fn known_folder(id: &GUID) -> Option<PathBuf> {
    let mut out: *mut u16 = std::ptr::null_mut();
    // SAFETY: `id` apunta a un GUID válido (una constante `FOLDERID_*`); flags 0 y token nulo = usuario actual; `out` es
    // escribible.
    let hr = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut out) };
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

/// Carpeta Descargas del usuario según Windows (puede estar redirigida), o `None` si falla.
pub fn downloads_dir() -> Option<PathBuf> {
    known_folder(&FOLDERID_Downloads)
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Qué es una ruta conocida (para que la interfaz la nombre).
///
/// `ChromeCache`, `EdgeCache` y `FirefoxCache` son SOLO directorios de caché concretos (ver [`browser_cache_dirs`]),
/// nunca el perfil del navegador (historial, marcadores, cookies, credenciales...).
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
    /// `ReportArchive` y `ReportQueue` de los informes de errores de Windows del usuario (ver [`crate::diagnostic_dirs`]).
    WerReports,
    /// `%LOCALAPPDATA%\CrashDumps`, los volcados locales de bloqueo de aplicaciones.
    CrashDumps,
}

/// Rutas conocidas candidatas, con su clase (sin comprobar que existan ni que cuelguen de la ruta escaneada):
/// `%TEMP%`, `Windows\Temp`, los directorios de CACHÉ de Chrome, Edge y Firefox ([`browser_cache_dirs`]; nunca sus
/// perfiles), las carpetas de informes de errores y de volcados ([`crate::diagnostic_dirs_verified`]), Descargas y la carpeta de la Papelera del usuario actual en el volumen `drive_root` (`C:\`). Esa carpeta
/// solo se LISTA, como todo el escaneo.
pub fn known_paths_labeled(drive_root: Option<&Path>) -> Vec<(KnownKind, PathBuf)> {
    known_paths_labeled_with(&BrowserRoots::from_system(), drive_root)
}

fn known_paths_labeled_with(
    roots: &BrowserRoots,
    drive_root: Option<&Path>,
) -> Vec<(KnownKind, PathBuf)> {
    let mut v = Vec::new();
    for var in ["TEMP", "TMP"] {
        v.extend(env_path(var).map(|p| (KnownKind::UserTemp, p)));
    }
    v.extend(env_path("SystemRoot").map(|p| (KnownKind::WindowsTemp, p.join("Temp"))));
    // Lo que el escáner recorre primero: solo cachés con TODOS sus ancestros reales (el `cleaner` usa las candidatas sin ese filtro).
    v.extend(browser_cache_dirs_verified(roots));
    // Informes de errores de Windows y volcados de bloqueo del usuario (solo los que existen y sin enlaces en el camino).
    v.extend(diagnostic_dirs_verified(roots));
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
    }

    #[test]
    fn las_rutas_conocidas_reales_de_navegador_son_cachés_concretas() {
        use crate::browser::{BrowserPath, classify_browser_path};
        // Con las carpetas reales del sistema: toda ruta de navegador es una caché concreta, y ninguna otra ruta
        // conocida cae en una.
        let roots = BrowserRoots::from_system();
        for (kind, p) in known_paths_labeled(Some(Path::new(r"C:\"))) {
            match kind {
                KnownKind::ChromeCache | KnownKind::EdgeCache | KnownKind::FirefoxCache => {
                    assert_eq!(
                        classify_browser_path(&roots, &p),
                        BrowserPath::Cache(kind),
                        "{p:?}"
                    );
                }
                _ => assert!(
                    !matches!(classify_browser_path(&roots, &p), BrowserPath::Cache(_)),
                    "{p:?}"
                ),
            }
        }
    }

    #[test]
    fn cada_ruta_conocida_lleva_su_clase() {
        use crate::browser::{BrowserPath, classify_browser_path};
        let labeled = known_paths_labeled(Some(Path::new(r"C:\")));
        // Mismo contenido y orden que `known_paths`.
        let plain: Vec<PathBuf> = labeled.iter().map(|(_, p)| p.clone()).collect();
        assert_eq!(plain, known_paths(Some(Path::new(r"C:\"))));
        assert!(labeled.iter().any(|(k, _)| *k == KnownKind::Downloads));
        assert!(labeled.iter().any(|(k, _)| *k == KnownKind::RecycleBin));
        assert!(labeled.iter().any(|(k, _)| *k == KnownKind::UserTemp));
        assert!(labeled.iter().any(|(k, _)| *k == KnownKind::WindowsTemp));
        // Con un entorno sintético, la lista de rutas conocidas lleva SOLO cachés concretas de navegador.
        let base = dunce::canonicalize(std::env::temp_dir()).unwrap();
        let root = base.join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
        // Guarda: se borra al soltarla, también si la prueba entra en pánico (solo bajo `%TEMP%`, nombre `tesela-test-*`).
        struct Guard(PathBuf);
        impl Drop for Guard {
            fn drop(&mut self) {
                if self
                    .0
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("tesela-test-"))
                {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
        }
        let _guard = Guard(root.clone());
        let mk = |rel: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(&p).unwrap();
        };
        mk("Local/Google/Chrome/User Data/Default/Cache");
        mk("Local/Google/Chrome/User Data/Default/History");
        mk("Local/Google/Chrome/User Data/Default/Service Worker/CacheStorage");
        mk("Roaming/Mozilla/Firefox/Profiles/p/cache2");
        mk("Local/Mozilla/Firefox/Profiles/p/cache2/entries");
        mk("Local/Microsoft/Windows/WER/ReportArchive/Informe1");
        mk("Local/Microsoft/Windows/WER/ERC");
        mk("Local/CrashDumps");
        let roots = BrowserRoots {
            local_appdata: Some(root.join("Local")),
            roaming_appdata: Some(root.join("Roaming")),
        };
        let v = known_paths_labeled_with(&roots, None);
        let browsers: Vec<&(KnownKind, PathBuf)> = v
            .iter()
            .filter(|(k, _)| {
                matches!(
                    k,
                    KnownKind::ChromeCache | KnownKind::EdgeCache | KnownKind::FirefoxCache
                )
            })
            .collect();
        // Informes de errores y volcados: solo `ReportArchive` (existe) y `CrashDumps`; nunca `ERC` ni la carpeta `WER`.
        let diag: Vec<&(KnownKind, PathBuf)> = v
            .iter()
            .filter(|(k, _)| matches!(k, KnownKind::WerReports | KnownKind::CrashDumps))
            .collect();
        assert_eq!(diag.len(), 2, "{diag:?}");
        assert!(diag.contains(&&(
            KnownKind::WerReports,
            root.join("Local/Microsoft/Windows/WER/ReportArchive")
        )));
        assert!(diag.contains(&&(KnownKind::CrashDumps, root.join("Local/CrashDumps"))));
        // Solo `Default\Cache` de Chrome y `cache2` de Firefox LOCAL.
        assert_eq!(browsers.len(), 2, "{browsers:?}");
        for (k, p) in browsers {
            assert_eq!(classify_browser_path(&roots, p), BrowserPath::Cache(*k));
            assert!(
                !p.ends_with("User Data") && !p.ends_with("Profiles"),
                "{p:?}"
            );
            assert!(!p.to_string_lossy().contains("Roaming"), "{p:?}");
        }
    }
}
