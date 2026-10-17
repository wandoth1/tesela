//! A DLL named after a static import and placed beside the `.exe` is not loaded: verify this with EMPTY files (not DLLs and
//! they execute nothing) named after DLLs imported by `tesela-scan.exe` and those loaded by `wgpu` by name.
//!
//! Without `/DEPENDENTLOADFLAG:0x800` (build.rs), the Windows loader resolves static imports using the default order,
//! which checks the `.exe` directory BEFORE `System32`: an empty `dxgi.dll` beside it prevents the process from starting
//! (nonzero exit before reaching `main`). With the flag, it starts and exits with 0.

mod common;

use std::path::PathBuf;
use std::process::Command;

/// Names of DLLs statically imported by `tesela-scan.exe` (and not `KnownDLLs`) or loaded by name.
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
    if !common::process_tests_enabled() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let tmp = Tmp(dir);
    let exe = tmp.0.join("tesela-scan-copia.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_tesela-scan"), &exe).unwrap();
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
    assert!(text.contains("tesela-scan "), "{text}");
}

fn u16_at(b: &[u8], o: usize) -> usize {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap()) as usize
}

fn u32_at(b: &[u8], o: usize) -> usize {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as usize
}

/// PE load configuration directory's `DependentLoadFlags` (written by `/DEPENDENTLOADFLAG`), if present.
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
    let exe = std::fs::read(env!("CARGO_BIN_EXE_tesela-scan")).unwrap();
    assert_eq!(
        dependent_load_flags(&exe),
        Some(0x800),
        "falta /DEPENDENTLOADFLAG:0x800 (LOAD_LIBRARY_SEARCH_SYSTEM32) en el PE"
    );
}
