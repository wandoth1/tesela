//! Endurecimiento de la carga de DLL del proceso (se llama lo PRIMERO de `main`).
//!
//! En `tesela-app`, `wgpu` carga `d3d12.dll`, `dxgi.dll`, `dcomp.dll` y `d3dcompiler_47.dll` POR NOMBRE con `LoadLibraryExW(nombre, NULL, 0)`
//! (`libloading`): el orden de búsqueda predeterminado de Windows mira la carpeta del `.exe` antes que `System32`, y ninguna
//! de ellas está en `KnownDLLs`. Una DLL con ese nombre dejada junto al `.exe` (una carpeta de instalación o de un zip
//! escribible por otros) se cargaría en vez de la del sistema. `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)`
//! limita la búsqueda de TODAS las cargas posteriores sin ruta ni indicadores (las de wgpu, winit, accesskit...) a
//! `System32`. No toca las cargas con ruta absoluta (los controladores de la GPU que carga DXGI por la suya) ni las que ya
//! hizo el cargador del sistema antes de `main` (las importaciones del propio `.exe`).
//!
//! (Copia del módulo del mismo nombre de `tesela-app`: `tesela-scan` no carga DLL por nombre a propósito, pero la biblioteca
//! estándar de Rust puede cargar `dbghelp.dll` al generar un *backtrace*. Las importaciones estáticas las cubre
//! `/DEPENDENTLOADFLAG:0x800` en `build.rs`.)
#![allow(unsafe_code)]

use windows_sys::Win32::System::LibraryLoader::{
    LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories,
};

/// Limita la búsqueda de DLL del proceso a `System32`. Devuelve `false` si Windows lo rechaza (el proceso sigue con el
/// orden predeterminado: no es un error fatal, pero se pierde el endurecimiento).
pub fn restrict_dll_search() -> bool {
    // SAFETY: `SetDefaultDllDirectories` solo recibe una máscara de indicadores constante; no hay punteros ni estado
    // compartido. Llamarla más de una vez es inofensivo.
    unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) != 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::FreeLibrary;
    use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;

    const PROBE: &str = "tesela_probe_dll.dll";

    /// Carga `PROBE` por nombre (sin ruta ni indicadores, como hace wgpu) y dice si el sistema la encontró.
    fn load_probe_by_name() -> bool {
        let wide: Vec<u16> = std::ffi::OsStr::new(PROBE)
            .encode_wide()
            .chain(Some(0))
            .collect();
        // SAFETY: `wide` termina en NUL y vive durante la llamada; el módulo cargado se libera enseguida.
        unsafe {
            let h = LoadLibraryW(wide.as_ptr());
            if h.is_null() {
                return false;
            }
            FreeLibrary(h);
            true
        }
    }

    /// Hijo de la prueba: con el directorio actual en una carpeta que contiene `PROBE`, comprueba que SIN el
    /// endurecimiento se carga por nombre y CON él no. (El directorio actual forma parte del orden predeterminado.)
    #[test]
    fn hijo_busqueda_restringida() {
        let Some(dir) = std::env::var_os("TESELA_DLL_PROBE_DIR") else {
            return; // solo lo lanza `la_busqueda_de_dll_se_limita_a_system32`
        };
        std::env::set_current_dir(dir).unwrap();
        assert!(
            load_probe_by_name(),
            "sin el endurecimiento la DLL de la carpeta actual se encuentra por nombre (la prueba no detectaría nada)"
        );
        assert!(restrict_dll_search());
        assert!(
            !load_probe_by_name(),
            "con el endurecimiento una DLL de fuera de System32 NO se carga por nombre"
        );
    }

    /// La prueba real corre en un proceso hijo: `SetDefaultDllDirectories` es del proceso entero y no debe cambiar las
    /// cargas de los demás tests. La DLL de prueba es una copia de una DLL legítima del sistema con otro nombre (no ejecuta
    /// nada propio) dentro de una carpeta temporal que se borra.
    #[test]
    fn la_busqueda_de_dll_se_limita_a_system32() {
        let sys = std::env::var_os("SystemRoot").map(std::path::PathBuf::from);
        let src = sys.map(|s| s.join("System32").join("msimg32.dll"));
        let Some(src) = src.filter(|p| p.exists()) else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        std::fs::copy(&src, dir.join(PROBE)).unwrap();
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "dll_hardening::tests::hijo_busqueda_restringida",
                "--nocapture",
            ])
            .env("TESELA_DLL_PROBE_DIR", &dir)
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

    /// `main` llama al endurecimiento ANTES que a cualquier otra cosa (antes de analizar argumentos y de escanear).
    #[test]
    fn main_lo_llama_lo_primero() {
        let main = include_str!("main.rs");
        let body = &main[main.find("fn main()").expect("fn main")..];
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
    }
}
