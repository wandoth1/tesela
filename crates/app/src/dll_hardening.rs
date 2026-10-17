//! Hardens DLL loading for the process (called FIRST in `main`, before eframe and wgpu).
//!
//! `wgpu` carga `d3d12.dll`, `dxgi.dll`, `dcomp.dll` y `d3dcompiler_47.dll` POR NOMBRE con `LoadLibraryExW(nombre, NULL, 0)`
//! (`libloading`): the default Windows search order checks the `.exe` directory before `System32`, and none of these DLLs
//! are `KnownDLLs`. A DLL with one of those names placed beside the `.exe` (in an install folder or zip writable by others)
//! would be loaded instead of the system DLL. `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` limits searches for
//! ALL later loads without a path or flags (including wgpu, winit, and accesskit) to `System32`. It does not affect loads
//! with absolute paths (such as GPU drivers loaded by DXGI) or loads already performed by the system loader before `main`
//! (the `.exe`'s own imports).
//!
//! Complemento: `Dx12Compiler::Fxc` fijado en `main.rs` evita que wgpu sondee `dxcompiler.dll`, que no existe en un
//! standard Windows installation, so there would be no legitimate DLL to win the search.
#![allow(unsafe_code)]

use windows_sys::Win32::System::LibraryLoader::{
    LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories, SetDllDirectoryW,
};

/// Empty NUL-terminated UTF-16 string for `SetDllDirectoryW`.
const EMPTY: [u16; 1] = [0];

/// Restricts the process DLL search to `System32`. Returns `false` if Windows rejects either call (the process keeps the
/// default behavior for that part: this is not fatal, but the hardening is lost).
///
/// 1. `SetDllDirectoryW("")` removes the current working directory from the classic search order (used by loads that do not
///    use `LOAD_LIBRARY_SEARCH_*` flags), in case any load does not honor the default set in step 2.
/// 2. `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` sets the default for all later loads without a path or flags.
pub fn restrict_dll_search() -> bool {
    // SAFETY: `EMPTY` is a NUL-terminated UTF-16 string that remains alive for the call. Calling this more than once is safe.
    let no_cwd = unsafe { SetDllDirectoryW(EMPTY.as_ptr()) != 0 };
    // SAFETY: `SetDefaultDllDirectories` receives only a constant flag mask; there are no pointers or shared state. Calling
    // this more than once is safe.
    let system32 = unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) != 0 };
    no_cwd && system32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::FreeLibrary;
    use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;

    const PROBE: &str = "tesela_probe_dll.dll";

    /// Loads `PROBE` by name (without a path or flags, as wgpu does) and reports whether the system found it.
    fn load_probe_by_name() -> bool {
        let wide: Vec<u16> = std::ffi::OsStr::new(PROBE)
            .encode_wide()
            .chain(Some(0))
            .collect();
        // SAFETY: `wide` is NUL-terminated and remains alive for the call; the loaded module is released immediately.
        unsafe {
            let h = LoadLibraryW(wide.as_ptr());
            if h.is_null() {
                return false;
            }
            FreeLibrary(h);
            true
        }
    }

    /// Test child (a COPY of the test executable in a temporary directory that also contains `PROBE`, the "application
    /// directory"): without hardening, the DLL is loaded by name from there; with hardening, it is not. This distinguishes
    /// `LOAD_LIBRARY_SEARCH_SYSTEM32` from `LOAD_LIBRARY_SEARCH_DEFAULT_DIRS`, which DOES include the application directory.
    #[test]
    fn hijo_busqueda_restringida() {
        if std::env::var_os("TESELA_DLL_PROBE_CHILD").is_none() {
            return; // Only `la_busqueda_de_dll_se_limita_a_system32` launches it.
        }
        let exe_dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        assert!(
            exe_dir.join(PROBE).exists(),
            "la DLL de prueba debe estar junto al ejecutable hijo"
        );
        assert!(
            load_probe_by_name(),
            "sin el endurecimiento la DLL de la carpeta de la aplicación se encuentra por nombre (la prueba no detectaría nada)"
        );
        assert!(restrict_dll_search());
        assert!(
            !load_probe_by_name(),
            "con el endurecimiento una DLL de la carpeta de la aplicación NO se carga por nombre"
        );
    }

    /// The real test runs in a child process: `SetDefaultDllDirectories` affects the entire process and must not change other
    /// tests' loads. The child is a copy of this test executable in a temporary directory, next to a renamed copy of a
    /// legitimate system DLL (nothing of our own is executed); the directory is deleted afterward.
    #[test]
    fn la_busqueda_de_dll_se_limita_a_system32() {
        // Launch a child process (a copy of the test executable): only with TESELA_PROCESS_TESTS=1 (set by CI).
        if !std::env::var("TESELA_PROCESS_TESTS").is_ok_and(|v| v == "1") {
            // In CI, skipping must not go unnoticed (for example, if a workflow variable is lost): fail.
            assert!(
                std::env::var_os("CI").is_none(),
                "CI sin TESELA_PROCESS_TESTS=1: las pruebas con procesos hijo no pueden saltarse en CI (revisa el workflow)"
            );
            eprintln!(
                "pruebas con procesos hijo desactivadas: defina TESELA_PROCESS_TESTS=1 para ejecutar esta prueba"
            );
            return;
        }
        let sys = std::env::var_os("SystemRoot").map(std::path::PathBuf::from);
        let src = sys.map(|s| s.join("System32").join("msimg32.dll"));
        let Some(src) = src.filter(|p| p.exists()) else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        std::fs::copy(&src, dir.join(PROBE)).unwrap();
        let child = dir.join("tesela-hijo.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &child).unwrap();
        let out = std::process::Command::new(&child)
            .args([
                "--exact",
                "dll_hardening::tests::hijo_busqueda_restringida",
                "--nocapture",
            ])
            .env("TESELA_DLL_PROBE_CHILD", "1")
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{text}");
        assert!(
            text.contains("1 passed"),
            "el hijo no ejecutó la prueba: {text}"
        );
    }

    /// GM-S2: hardening makes BOTH calls: removes the working directory from the classic search order (`SetDllDirectoryW("")`)
    /// and sets the default to `System32`. Changing the test process state would affect other tests (so actual behavior is
    /// tested in a child process); here we ensure both calls remain in the code.
    #[test]
    fn el_endurecimiento_quita_tambien_el_directorio_de_trabajo() {
        let src = include_str!("dll_hardening.rs");
        let body = &src[src.find("pub fn restrict_dll_search").unwrap()..];
        let body = &body[..body
            .find(
                "
}
",
            )
            .unwrap()];
        let cwd = body
            .find("SetDllDirectoryW(EMPTY.as_ptr())")
            .expect("SetDllDirectoryW(\"\")");
        let sys32 = body
            .find("SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)")
            .expect("SetDefaultDllDirectories");
        assert!(cwd < sys32);
        assert_eq!(EMPTY, [0u16], "cadena vacía terminada en NUL");
        assert!(
            body.contains("no_cwd && system32"),
            "las dos llamadas cuentan"
        );
    }

    /// `main` calls hardening BEFORE anything else (before parsing arguments and before eframe/wgpu).
    #[test]
    fn main_lo_llama_lo_primero() {
        let main = include_str!("main.rs");
        let body = &main[main.find("fn main()").expect("fn main")..];
        let call = body
            .find("dll_hardening::restrict_dll_search()")
            .expect("main llama a restrict_dll_search");
        // First statement in `main` (skipping comments).
        let after_brace = &body[body.find('{').unwrap() + 1..];
        let first_stmt = after_brace
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("//"))
            .unwrap();
        assert!(
            first_stmt.contains("dll_hardening::restrict_dll_search()"),
            "la llamada tiene que ser la primera sentencia de main: {first_stmt}"
        );
        for later in ["cli::parse", "eframe::run_native", "tesela_ui::fonts"] {
            if let Some(i) = body.find(later) {
                assert!(call < i, "el endurecimiento debe ir antes de {later}");
            }
        }
        // The DX12 shader compiler is also fixed to FXC: `dxcompiler.dll` is never probed.
        assert!(main.contains("Dx12Compiler::Fxc"));
    }
}
