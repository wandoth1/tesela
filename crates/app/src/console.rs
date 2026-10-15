//! Salida de texto de un ejecutable de interfaz gráfica. En release `tesela.exe` usa el subsistema `windows` (sin
//! ventana de consola al abrirlo desde el menú Inicio), así que `--help`, los errores de línea de comandos y las
//! cifras de `--bench-hover` solo llegarían a un terminal si se engancha al de quien lo lanzó.
#![allow(unsafe_code)]

use std::io::Write;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE,
};

/// ¿Tiene el proceso una salida de error válida (heredada de quien lo lanzó: terminal, tubería o archivo)?
pub fn has_std_error() -> bool {
    // SAFETY: `GetStdHandle` solo lee el identificador estándar de este proceso; no recibe punteros.
    let h = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    !h.is_null() && h != INVALID_HANDLE_VALUE
}

/// Se engancha a la consola del proceso padre. Devuelve `false` si no hay (lanzado desde el Explorador) o si el
/// proceso ya tiene una consola.
pub fn attach_parent() -> bool {
    // SAFETY: `AttachConsole` no recibe punteros; con `ATTACH_PARENT_PROCESS` solo intenta usar la consola del padre.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) != 0 }
}

/// Escribe `text` (con salto de línea) donde el usuario pueda verlo: en la salida de error si existe; si no, en la
/// consola del padre (`CONOUT$`) tras engancharse a ella.
pub fn print(text: &str) {
    if has_std_error() {
        let _ = writeln!(std::io::stderr(), "{text}");
        return;
    }
    if attach_parent()
        && let Ok(mut f) = std::fs::OpenOptions::new().write(true).open("CONOUT$")
    {
        let _ = writeln!(f, "{text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn las_llamadas_a_windows_se_ejecutan() {
        // Cualquier resultado es válido (depende de cómo se lance el test); se prueba que las dos llamadas corren.
        let _ = has_std_error();
        let _ = attach_parent();
        print("tesela: prueba de la salida de texto");
    }
}
