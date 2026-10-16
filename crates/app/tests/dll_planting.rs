//! Una DLL con el nombre de una importación estática dejada junto al `.exe` no se carga: se comprueba con archivos VACÍOS (no
//! son DLL y no ejecutan nada) con los nombres de las DLL que `tesela.exe` importa y de las que `wgpu` carga por nombre.
//!
//! Sin `/DEPENDENTLOADFLAG:0x800` (build.rs) el cargador de Windows resuelve las importaciones estáticas con el orden
//! predeterminado, que mira la carpeta del `.exe` ANTES que `System32`: un `dxgi.dll` vacío al lado hace que el proceso ni
//! arranque (código de salida distinto de 0 antes de llegar a `main`). Con la bandera arranca y sale con 0.

use std::path::PathBuf;
use std::process::Command;

/// Nombres de las DLL que `tesela.exe` importa de forma estática (y no son `KnownDLLs`) o que se cargan por nombre.
const PLANTED: [&str; 11] = [
    "dxgi",
    "dwmapi",
    "uxtheme",
    "propsys",
    "uiautomationcore",
    "bcryptprimitives",
    "d3d12",
    "dcomp",
    "d3dcompiler_47",
    "dxcompiler",
    "dxil",
];

struct Tmp(PathBuf);

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn archivos_con_nombre_de_dll_junto_al_exe_no_impiden_arrancar() {
    let dir = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let tmp = Tmp(dir);
    let exe = tmp.0.join("tesela-copia.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_tesela"), &exe).unwrap();
    for name in PLANTED {
        std::fs::write(tmp.0.join(format!("{name}.dll")), b"").unwrap();
    }
    let out = Command::new(&exe).arg("--version").output().unwrap();
    assert!(
        out.status.success(),
        "con DLL falsas (vacías) junto al .exe no arranca (código {:?}): la importación estática se resolvió desde su carpeta",
        out.status.code()
    );
    let text =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("tesela "), "{text}");
}

fn u16_at(b: &[u8], o: usize) -> usize {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap()) as usize
}

fn u32_at(b: &[u8], o: usize) -> usize {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as usize
}

/// `DependentLoadFlags` del directorio de configuración de carga del PE (lo que escribe `/DEPENDENTLOADFLAG`), si lo lleva.
fn dependent_load_flags(exe: &[u8]) -> Option<usize> {
    let pe = u32_at(exe, 0x3C);
    assert_eq!(&exe[pe..pe + 4], b"PE\0\0");
    let n_sections = u16_at(exe, pe + 6);
    let opt = pe + 24;
    assert_eq!(u16_at(exe, opt), 0x20B, "PE32+ (x64)");
    let lc_rva = u32_at(exe, opt + 112 + 10 * 8);
    if lc_rva == 0 {
        return None;
    }
    let sections = opt + u16_at(exe, pe + 20);
    for i in 0..n_sections {
        let s = sections + i * 40;
        let (vsize, va, raw) = (u32_at(exe, s + 8), u32_at(exe, s + 12), u32_at(exe, s + 20));
        if lc_rva >= va && lc_rva < va + vsize {
            let at = raw + (lc_rva - va);
            return (u32_at(exe, at) > 0x4E + 2).then(|| u16_at(exe, at + 0x4E));
        }
    }
    None
}

#[test]
fn el_exe_lleva_dependentloadflag_system32() {
    let exe = std::fs::read(env!("CARGO_BIN_EXE_tesela")).unwrap();
    assert_eq!(
        dependent_load_flags(&exe),
        Some(0x800),
        "falta /DEPENDENTLOADFLAG:0x800 (LOAD_LIBRARY_SEARCH_SYSTEM32) en el PE"
    );
}
