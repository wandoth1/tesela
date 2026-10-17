//! Detección de navegadores abiertos. Es SOLO UX ("Cierra Chrome para liberar hasta N"): la garantía es la apertura
//! exclusiva de cada archivo (PLAN.md §3.7.7); si el navegador arranca a mitad de la limpieza, sus archivos fallan la
//! apertura exclusiva y se saltan.
#![allow(unsafe_code)]

use crate::rules::CategoryId;
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};

/// Navegadores cuyas cachés se limpian.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Browser {
    Chrome,
    Edge,
    Firefox,
}

impl Browser {
    pub const ALL: [Browser; 3] = [Browser::Chrome, Browser::Edge, Browser::Firefox];

    /// Nombre del ejecutable.
    pub fn exe(self) -> &'static str {
        match self {
            Browser::Chrome => "chrome.exe",
            Browser::Edge => "msedge.exe",
            Browser::Firefox => "firefox.exe",
        }
    }

    /// Nombre para mostrar.
    pub fn name(self) -> &'static str {
        match self {
            Browser::Chrome => "Chrome",
            Browser::Edge => "Edge",
            Browser::Firefox => "Firefox",
        }
    }

    /// La categoría de su caché.
    pub fn cache_category(self) -> CategoryId {
        match self {
            Browser::Chrome => CategoryId::ChromeCache,
            Browser::Edge => CategoryId::EdgeCache,
            Browser::Firefox => CategoryId::FirefoxCache,
        }
    }

    pub fn from_category(id: CategoryId) -> Option<Browser> {
        Browser::ALL.into_iter().find(|b| b.cache_category() == id)
    }
}

/// Qué navegadores aparecen en una lista de nombres de proceso (sin distinguir mayúsculas). Puro.
pub fn match_browsers<S: AsRef<str>>(names: &[S]) -> Vec<Browser> {
    Browser::ALL
        .into_iter()
        .filter(|b| {
            names
                .iter()
                .any(|n| n.as_ref().eq_ignore_ascii_case(b.exe()))
        })
        .collect()
}

/// Outlook process matching is only a warning; exclusive handles remain the safety check.
pub fn match_outlook<S: AsRef<str>>(names: &[S]) -> bool {
    names.iter().any(|n| {
        ["outlook.exe", "olk.exe"]
            .iter()
            .any(|exe| n.as_ref().eq_ignore_ascii_case(exe))
    })
}

pub fn outlook_open() -> bool {
    match_outlook(&process_names())
}

/// Nombres de los procesos en ejecución (instantánea de ToolHelp).
fn process_names() -> Vec<String> {
    // SAFETY: `CreateToolhelp32Snapshot` no tiene precondiciones.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE || snap.is_null() {
        return Vec::new();
    }
    let mut names = Vec::new();
    // SAFETY: PROCESSENTRY32W es un POD (enteros y un array de u16): todo-ceros es válido.
    let mut e: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: `snap` es un handle de instantánea válido; `e` es escribible y trae su `dwSize`.
    let mut ok = unsafe { Process32FirstW(snap, &mut e) };
    while ok != 0 {
        let len = e
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(e.szExeFile.len());
        names.push(String::from_utf16_lossy(&e.szExeFile[..len]));
        // SAFETY: igual que arriba.
        ok = unsafe { Process32NextW(snap, &mut e) };
    }
    // SAFETY: `snap` es el handle válido de arriba y se cierra una sola vez.
    unsafe { CloseHandle(snap) };
    names
}

/// Navegadores abiertos ahora mismo.
pub fn running_browsers() -> Vec<Browser> {
    match_browsers(&process_names())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconoce_los_ejecutables_sin_distinguir_mayusculas() {
        assert_eq!(
            match_browsers(&["explorer.exe", "CHROME.EXE", "x"]),
            vec![Browser::Chrome]
        );
        assert_eq!(
            match_browsers(&["msedge.exe", "Firefox.exe"]),
            vec![Browser::Edge, Browser::Firefox]
        );
        assert!(match_browsers::<&str>(&[]).is_empty());
        assert!(match_browsers(&["chrome.exe.bak", "mychrome.exe"]).is_empty());
        for b in Browser::ALL {
            assert_eq!(Browser::from_category(b.cache_category()), Some(b));
        }
        assert_eq!(Browser::from_category(CategoryId::UserTemp), None);
    }

    #[test]
    fn la_instantanea_de_procesos_funciona_y_no_inventa_navegadores() {
        let names = process_names();
        assert!(!names.is_empty(), "debe haber procesos");
        // El propio proceso de pruebas está en la lista.
        let me = std::env::current_exe().unwrap();
        let me = me.file_name().unwrap().to_string_lossy().to_string();
        assert!(names.iter().any(|n| n.eq_ignore_ascii_case(&me)), "{me}");
        let _ = running_browsers();
    }
}

#[cfg(test)]
mod outlook_tests {
    use super::*;
    #[test]
    fn outlook_matches_only_classic_and_new_process_names() {
        assert!(match_outlook(&["OUTLOOK.EXE"]));
        assert!(match_outlook(&["olk.exe"]));
        assert!(!match_outlook(&["outlook.exe.bak", "myolk.exe"]));
        assert!(!match_outlook::<&str>(&[]));
    }
}
