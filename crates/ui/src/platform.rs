//! Consultas a Windows que necesita la interfaz. Aquí vive TODO el `unsafe` del crate, en funciones pequeñas con
//! su `// SAFETY:` y un test que las ejecuta de verdad (Windows es el único destino del producto).
//!
//! Todo es de SOLO LECTURA o abre una ventana del propio sistema (Propiedades); nada escribe ni borra archivos.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{FALSE, TRUE};
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::{
    FOLDERID_Fonts, SEE_MASK_FLAG_NO_UI, SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW,
    SHGetKnownFolderPath, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SPI_GETCLIENTAREAANIMATION, SW_SHOW, SystemParametersInfoW,
};

// Valores de `GetDriveTypeW` (están en `Win32_System_WindowsProgramming`, que no se necesita para nada más).
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;

/// Tipo de unidad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    /// Disco fijo.
    Fixed,
    /// Extraíble (USB, tarjeta): no tiene Papelera.
    Removable,
    /// Unidad de red: no tiene Papelera.
    Network,
}

/// Una unidad de disco para la pantalla de inicio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveInfo {
    /// Letra de unidad (`C`).
    pub letter: char,
    /// Etiqueta del volumen (vacía si no tiene).
    pub label: String,
    pub kind: DriveKind,
    /// Bytes totales.
    pub total: u64,
    /// Bytes libres.
    pub free: u64,
}

impl DriveInfo {
    /// Raíz de la unidad (`C:\`), construida en tiempo de ejecución.
    pub fn root(&self) -> PathBuf {
        PathBuf::from(format!("{}:\\", self.letter))
    }

    /// Porcentaje usado, 0..=100.
    pub fn used_percent(&self) -> u8 {
        if self.total == 0 {
            return 0;
        }
        let used = self.total.saturating_sub(self.free);
        ((used as u128 * 100 + self.total as u128 / 2) / self.total as u128).min(100) as u8
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_path(p: &Path) -> Vec<u16> {
    p.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Carpeta de fuentes del sistema (`SHGetKnownFolderPath(FOLDERID_Fonts)`), o `None` si Windows no la da.
pub fn fonts_dir() -> Option<PathBuf> {
    let mut p: *mut u16 = std::ptr::null_mut();
    // SAFETY: `FOLDERID_Fonts` es una constante válida; el token nulo significa "el usuario actual" y `p` es
    // escribible. Si devuelve éxito, `p` apunta a una cadena UTF-16 terminada en NUL reservada por COM.
    let hr = unsafe { SHGetKnownFolderPath(&FOLDERID_Fonts, 0, std::ptr::null_mut(), &mut p) };
    if hr != 0 || p.is_null() {
        if !p.is_null() {
            // SAFETY: según la documentación hay que liberar el puntero también cuando la llamada falla.
            unsafe { CoTaskMemFree(p as *const c_void) };
        }
        return None;
    }
    let mut len = 0usize;
    // SAFETY: `p` apunta a una cadena terminada en NUL (la acaba de reservar la llamada correcta de arriba), así que
    // no se lee más allá del terminador.
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: los `len` primeros elementos son válidos y están inicializados.
    let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) });
    // SAFETY: `p` lo reservó COM y no se ha liberado antes; no se vuelve a usar después.
    unsafe { CoTaskMemFree(p as *const c_void) };
    Some(PathBuf::from(s))
}

/// ¿Tiene el usuario activadas las animaciones del área de cliente (`SPI_GETCLIENTAREAANIMATION`)?
/// Si Windows no responde, se asume que sí.
pub fn client_area_animation_enabled() -> bool {
    let mut enabled: i32 = TRUE;
    // SAFETY: para este `SPI_*` el cuarto argumento es un `BOOL*` escribible; `enabled` ocupa 4 bytes.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            &mut enabled as *mut i32 as *mut c_void,
            0,
        )
    };
    ok == FALSE || enabled != FALSE
}

/// Unidades de disco del equipo con su espacio. Consulta cada letra presente; las que no responden (sin medio, red
/// desconectada) se omiten. Puede tardar con una unidad de red caída: llamar desde un hilo.
pub fn drives() -> Vec<DriveInfo> {
    // SAFETY: no recibe punteros; devuelve una máscara de bits con una letra por bit.
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = wide(&format!("{letter}:\\"));
        // SAFETY: `root` es una cadena UTF-16 terminada en NUL.
        let kind = match unsafe { GetDriveTypeW(root.as_ptr()) } {
            DRIVE_FIXED => DriveKind::Fixed,
            DRIVE_REMOVABLE => DriveKind::Removable,
            DRIVE_REMOTE => DriveKind::Network,
            _ => continue, // CD-ROM, disco RAM, desconocidas
        };
        let (mut free_caller, mut total, mut free) = (0u64, 0u64, 0u64);
        // SAFETY: `root` termina en NUL y los tres punteros son escribibles.
        let ok =
            unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut free_caller, &mut total, &mut free) };
        if ok == FALSE || total == 0 {
            continue;
        }
        let mut name = [0u16; 261];
        // SAFETY: `root` termina en NUL; `name` tiene la longitud indicada; el resto de salidas son opcionales (nulas).
        let got = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                name.as_mut_ptr(),
                name.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        let label = if got != FALSE {
            let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
            String::from_utf16_lossy(&name[..end])
        } else {
            String::new()
        };
        out.push(DriveInfo {
            letter,
            label,
            kind,
            total,
            free,
        });
    }
    out
}

/// Abre la hoja de Propiedades del Explorador para `path`. Devuelve `false` si Windows no pudo (ruta inexistente...).
/// Sin interfaz de error (`SEE_MASK_FLAG_NO_UI`): si falla, no sale ningún cuadro.
pub fn show_properties(path: &Path) -> bool {
    let file = wide_path(path);
    let verb = wide("properties");
    // SAFETY: `SHELLEXECUTEINFOW` es una estructura de enteros y punteros para la que todo cero es un valor válido
    // (punteros nulos); se rellenan a continuación los campos que usa esta llamada.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_INVOKEIDLIST | SEE_MASK_FLAG_NO_UI;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.nShow = SW_SHOW;
    // SAFETY: `info` está inicializada con su tamaño real; `verb` y `file` terminan en NUL y viven hasta que la
    // llamada vuelve (`SEE_MASK_INVOKEIDLIST` sin `NOASYNC` no los usa después).
    unsafe { ShellExecuteExW(&mut info) != FALSE }
}

/// Abre el Explorador con `path` seleccionado (o su carpeta, si es un directorio). No usa `unsafe`.
pub fn reveal_in_explorer(path: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new("explorer.exe");
    cmd.raw_arg(format!("/select,\"{}\"", path.display()));
    cmd.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_carpeta_de_fuentes_existe() {
        let d = fonts_dir().expect("carpeta de fuentes");
        assert!(d.is_dir(), "{d:?}");
    }

    #[test]
    fn las_animaciones_se_consultan_sin_fallar() {
        // Cualquiera de los dos valores es válido; lo que se prueba es que la llamada se ejecuta.
        let _ = client_area_animation_enabled();
    }

    #[test]
    fn las_unidades_incluyen_la_del_sistema_con_espacio_coherente() {
        let drives = drives();
        let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let letter = sys.chars().next().unwrap().to_ascii_uppercase();
        let d = drives
            .iter()
            .find(|d| d.letter == letter)
            .expect("unidad del sistema");
        assert_eq!(d.kind, DriveKind::Fixed);
        assert!(d.total > 0 && d.free <= d.total);
        assert!(d.used_percent() <= 100);
        assert_eq!(d.root(), PathBuf::from(format!("{letter}:\\")));
    }

    #[test]
    fn propiedades_de_una_ruta_inexistente_falla_sin_abrir_nada() {
        // Una ruta que no existe bajo %TEMP%: la llamada se ejecuta y Windows la rechaza sin mostrar ningún cuadro.
        let p = std::env::temp_dir().join(format!("tesela-test-{}-no-existe", std::process::id()));
        assert!(!p.exists());
        assert!(!show_properties(&p));
    }

    #[test]
    fn porcentaje_usado() {
        let d = DriveInfo {
            letter: 'C',
            label: String::new(),
            kind: DriveKind::Fixed,
            total: 1000,
            free: 250,
        };
        assert_eq!(d.used_percent(), 75);
        let z = DriveInfo { total: 0, ..d };
        assert_eq!(z.used_percent(), 0);
    }
}
