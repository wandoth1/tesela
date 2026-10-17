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
    LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories, SetDllDirectoryW,
};

/// Cadena vacía terminada en NUL (UTF-16) para `SetDllDirectoryW`.
const EMPTY: [u16; 1] = [0];

/// Limita la búsqueda de DLL del proceso a `System32`. Devuelve `false` si Windows rechaza alguna de las dos llamadas (el
/// proceso sigue con el orden predeterminado en esa parte: no es un error fatal, pero se pierde el endurecimiento).
///
/// 1. `SetDllDirectoryW("")` quita el directorio de trabajo actual del orden de búsqueda clásico (el de las cargas que no
///    usan los indicadores `LOAD_LIBRARY_SEARCH_*`), por si alguna carga no respetase el valor predeterminado del paso 2.
/// 2. `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` fija el valor predeterminado de todas las cargas
///    posteriores sin ruta ni indicadores.
pub fn restrict_dll_search() -> bool {
    // SAFETY: `EMPTY` es una cadena UTF-16 terminada en NUL que vive durante toda la llamada. Llamarla más de una vez es
    // inofensivo.
    let no_cwd = unsafe { SetDllDirectoryW(EMPTY.as_ptr()) != 0 };
    // SAFETY: `SetDefaultDllDirectories` solo recibe una máscara de indicadores constante; no hay punteros ni estado
    // compartido. Llamarla más de una vez es inofensivo.
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

    /// Hijo de la prueba (una COPIA del ejecutable de pruebas dentro de una carpeta temporal que también contiene `PROBE`: esa
    /// es "la carpeta de la aplicación"): SIN el endurecimiento la DLL se carga por nombre desde ahí y CON él no. Así se
    /// distingue `LOAD_LIBRARY_SEARCH_SYSTEM32` de `LOAD_LIBRARY_SEARCH_DEFAULT_DIRS`, que SÍ incluye la carpeta de la aplicación.
    #[test]
    fn hijo_busqueda_restringida() {
        if std::env::var_os("TESELA_DLL_PROBE_CHILD").is_none() {
            return; // solo lo lanza `la_busqueda_de_dll_se_limita_a_system32`
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

    /// La prueba real corre en un proceso hijo: `SetDefaultDllDirectories` es del proceso entero y no debe cambiar las
    /// cargas de los demás tests. El hijo es una copia de este ejecutable de pruebas en una carpeta temporal, con al lado una
    /// copia de una DLL legítima del sistema con otro nombre (no ejecuta nada propio); la carpeta se borra al acabar.
    #[test]
    fn la_busqueda_de_dll_se_limita_a_system32() {
        // Lanza un proceso hijo (copia del ejecutable de pruebas): solo con TESELA_PROCESS_TESTS=1 (el CI la define).
        if !std::env::var("TESELA_PROCESS_TESTS").is_ok_and(|v| v == "1") {
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

    /// GM-S2: el endurecimiento hace las DOS llamadas: quita el directorio de trabajo del orden clásico (`SetDllDirectoryW("")`)
    /// y fija el valor predeterminado a `System32`. Cambiar el estado del proceso de pruebas afectaría a las demás pruebas
    /// (por eso el comportamiento real se prueba en un proceso hijo): aquí se vigila que las dos llamadas siguen en el código.
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
