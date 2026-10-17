//! Text output from a GUI executable. In release, `tesela.exe` uses the Windows subsystem (so it has no console window
//! when opened from the Start menu); `--help`, command-line errors, and `--bench-hover` results reach a terminal only if
//! the process attaches to the one that launched it.
#![allow(unsafe_code)]

use std::io::Write;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE,
};

/// Does the process have a valid standard error handle (inherited from its launcher: terminal, pipe, or file)?
pub fn has_std_error() -> bool {
    // SAFETY: `GetStdHandle` only reads this process's standard handle; it takes no pointers.
    let h = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    !h.is_null() && h != INVALID_HANDLE_VALUE
}

/// Attaches to the parent process's console. Returns `false` if there is none (launched from Explorer) or if this process
/// already has a console.
pub fn attach_parent() -> bool {
    // SAFETY: `AttachConsole` takes no pointers; with `ATTACH_PARENT_PROCESS`, it only attempts to use the parent's console.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) != 0 }
}

/// Writes `text` followed by a newline where the user can see it: to standard error if available, or to the parent's
/// console (`CONOUT$`) after attaching to it.
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
