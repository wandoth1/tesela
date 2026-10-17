//! Known paths scanned first (priority queue, PLAN.md §3.3) and the two Windows queries that
//! require the current user's SID and Downloads folder. READ-ONLY.
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

/// Length of a NUL-terminated UTF-16 string.
///
/// # Safety
/// `p` must point to a valid NUL-terminated UTF-16 string.
unsafe fn wcslen(p: *const u16) -> usize {
    let mut n = 0;
    // SAFETY: the caller guarantees the string ends in NUL, so no reads pass the terminator.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    n
}

/// Current process user's SID as text (`S-1-5-21-...`), or `None` if Windows does not provide it.
pub fn current_user_sid() -> Option<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the current process pseudo-handle is always valid; `token` is writable.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let sid = (|| {
        // First call: obtain the required size (intentionally fails with ERROR_INSUFFICIENT_BUFFER).
        let mut needed = 0u32;
        // SAFETY: `token` is valid; a null buffer with length 0 is the documented way to query the required size.
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
        if needed == 0 {
            return None;
        }
        // Use `u64` to guarantee the alignment required by TOKEN_USER.
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        // SAFETY: `buf` is writable, aligned, and at least `needed` bytes long; `token` is valid.
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
        // SAFETY: the previous call filled `buf` with a TOKEN_USER (the memory is valid and aligned to 8 bytes).
        let sid_ptr = unsafe { (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid };
        let mut wide: *mut u16 = std::ptr::null_mut();
        // SAFETY: `sid_ptr` points to a valid SID inside `buf`; `wide` is writable.
        if unsafe { ConvertSidToStringSidW(sid_ptr, &mut wide) } == 0 || wide.is_null() {
            return None;
        }
        // SAFETY: `wide` is a NUL-terminated UTF-16 string allocated by Windows; it is read before being freed.
        let text = unsafe { OsString::from_wide(std::slice::from_raw_parts(wide, wcslen(wide))) };
        // SAFETY: `wide` was allocated with LocalAlloc inside ConvertSidToStringSidW and is freed exactly once.
        unsafe { LocalFree(wide as *mut _) };
        text.into_string().ok()
    })();
    // SAFETY: `token` is the valid handle obtained above and is closed exactly once.
    unsafe { CloseHandle(token) };
    sid
}

/// A Windows known folder (`SHGetKnownFolderPath`, current user; may be redirected), or `None` on failure.
pub fn known_folder(id: &GUID) -> Option<PathBuf> {
    let mut out: *mut u16 = std::ptr::null_mut();
    // SAFETY: `id` points to a valid GUID (a `FOLDERID_*` constant); flags 0 and a null token mean the current user; `out`
    // is writable.
    let hr = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut out) };
    if hr < 0 || out.is_null() {
        if !out.is_null() {
            // SAFETY: the documentation requires freeing any pointer returned, even on failure.
            unsafe { CoTaskMemFree(out as *const _) };
        }
        return None;
    }
    // SAFETY: after a successful HRESULT, `out` is a NUL-terminated UTF-16 string allocated with CoTaskMemAlloc.
    let path = unsafe { OsString::from_wide(std::slice::from_raw_parts(out, wcslen(out))) };
    // SAFETY: `out` was allocated with the correct allocator and is freed exactly once.
    unsafe { CoTaskMemFree(out as *const _) };
    Some(path.into())
}

/// User's Downloads folder according to Windows (may be redirected), or `None` on failure.
pub fn downloads_dir() -> Option<PathBuf> {
    known_folder(&FOLDERID_Downloads)
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Kind of known path (used by the interface to label it).
///
/// `ChromeCache`, `EdgeCache`, and `FirefoxCache` are specific cache directories ONLY (see [`browser_cache_dirs`]),
/// never the browser profile (history, bookmarks, cookies, credentials...).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KnownKind {
    /// The user's `%TEMP%` directory.
    UserTemp,
    /// `Windows\Temp`.
    WindowsTemp,
    ChromeCache,
    EdgeCache,
    FirefoxCache,
    /// The Downloads folder.
    Downloads,
    /// The user's Recycle Bin folder on the volume.
    RecycleBin,
    /// The user's Windows error reports in `ReportArchive` and `ReportQueue` (see [`crate::diagnostic_dirs`]).
    WerReports,
    /// `%LOCALAPPDATA%\CrashDumps`, local application crash dumps.
    CrashDumps,
}

/// Candidate known paths and their kinds (without checking that they exist or are under the scanned path):
/// `%TEMP%`, `Windows\Temp`, Chrome, Edge, and Firefox CACHE directories ([`browser_cache_dirs`]; never their
/// profiles), error-report and crash-dump folders ([`crate::diagnostic_dirs_verified`]), Downloads, and the current user's Recycle Bin folder on volume `drive_root` (`C:\`). That folder
/// is only LISTED, like everything else in the scan.
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
    // Paths the scanner visits first: caches whose full ancestor chains are real directories (`cleaner` uses candidates without this filter).
    v.extend(browser_cache_dirs_verified(roots));
    // Windows error reports and user crash dumps (only existing paths with no links in the path).
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

/// The same paths without their kinds (used by the scanner's priority queue).
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
        // With real system folders, every browser path is a specific cache, and no other known path is classified as one.
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
        // Same contents and order as `known_paths`.
        let plain: Vec<PathBuf> = labeled.iter().map(|(_, p)| p.clone()).collect();
        assert_eq!(plain, known_paths(Some(Path::new(r"C:\"))));
        assert!(labeled.iter().any(|(k, _)| *k == KnownKind::Downloads));
        assert!(labeled.iter().any(|(k, _)| *k == KnownKind::RecycleBin));
        assert!(labeled.iter().any(|(k, _)| *k == KnownKind::UserTemp));
        assert!(labeled.iter().any(|(k, _)| *k == KnownKind::WindowsTemp));
        // In a synthetic environment, the known-path list contains ONLY specific browser caches.
        let base = dunce::canonicalize(std::env::temp_dir()).unwrap();
        let root = base.join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
        // Guard: removed on drop, including if the test panics (only under `%TEMP%`, with a `tesela-test-*` name).
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
        // Error reports and dumps: only `ReportArchive` (which exists) and `CrashDumps`; never `ERC` or the `WER` folder.
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
        // Only Chrome's `Default\Cache` and Firefox's LOCAL `cache2`.
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
