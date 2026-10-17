//! A DLL named after a static import and placed next to the `.exe` is not loaded: this is checked using EMPTY files (not
//! DLLs, and they execute nothing) named after the DLLs imported by `tesela.exe` and those `wgpu` loads by name.
//!
//! Without `/DEPENDENTLOADFLAG:0x800` (build.rs), the Windows loader resolves static imports in the default order,
//! which checks the `.exe` folder BEFORE `System32`: an empty `dxgi.dll` beside it prevents the process from starting
//! (nonzero exit code before reaching `main`). With the flag, it starts and exits with 0.

mod common;

use std::path::PathBuf;
use std::process::Command;

/// DLL names that `tesela.exe` imports statically (and that are not `KnownDLLs`) or that are loaded by name.
const PLANTED: [&str; 12] = [
    "dxgi",
    "dwmapi",
    "uxtheme",
    "propsys",
    "uiautomationcore",
    "bcryptprimitives",
    "winhttp",
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

/// Does the window open and render? `--demo --bench-hover 1` prints the measured frames; if it does not render, there are no
/// figures. Retry because the measurement is intermittent (the window may not render at startup), even without fake DLLs.
fn paints(exe: &std::path::Path) -> bool {
    // Defense in depth: the process profile (APPDATA and LOCALAPPDATA) points to test-owned temporary folders; no test
    // can touch the user's real profile.
    let base = exe.parent().unwrap();
    let (roaming, local) = (base.join("Roaming"), base.join("Local"));
    std::fs::create_dir_all(&roaming).unwrap();
    std::fs::create_dir_all(&local).unwrap();
    (0..3).any(|_| {
        let out = Command::new(exe)
            .args(["--demo", "--bench-hover", "1"])
            .env("APPDATA", &roaming)
            .env("LOCALAPPDATA", &local)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stderr).to_string()
            + &String::from_utf8_lossy(&out.stdout);
        out.status.success() && text.contains("bench-hover:") && text.contains(" frames ")
    })
}

/// With empty files named after DLLs placed next to the `.exe` (`d3d12`, `dcomp`, `d3dcompiler_47`, `dxcompiler`, `dxil`... the ones
/// `wgpu` loads BY NAME when opening the window), the program still opens and renders: the copies from `System32` are loaded.
/// This verifies `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)`: with `LOAD_LIBRARY_SEARCH_DEFAULT_DIRS` (which includes
/// the application folder) or without the call, `wgpu` would load the empty file and fail to create the surface. (`--version` is
/// not useful: it exits before anything is loaded by name.) Without a graphical desktop or DX12, the CONTROL (without fake files)
/// does not render either, so there is nothing to compare and the test is skipped.
#[test]
fn con_dll_falsas_junto_al_exe_la_ventana_abre_y_pinta() {
    if !common::process_tests_enabled() {
        return;
    }
    let control = Tmp(std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&control.0).unwrap();
    let control_exe = control.0.join("tesela-control.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_tesela"), &control_exe).unwrap();
    if !paints(&control_exe) {
        // Do not skip in CI: without this, a runner with no graphical desktop would pass the test without checking anything.
        assert!(
            std::env::var_os("CI").is_none(),
            "en CI el control (sin archivos falsos) tiene que abrir y pintar la ventana: la prueba no puede omitirse"
        );
        eprintln!("sin ventana que pinte en este entorno (el control tampoco): se omite");
        return;
    }
    let planted = Tmp(std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&planted.0).unwrap();
    let planted_exe = planted.0.join("tesela-copia.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_tesela"), &planted_exe).unwrap();
    for name in PLANTED {
        std::fs::write(planted.0.join(format!("{name}.dll")), b"").unwrap();
    }
    assert!(
        paints(&planted_exe),
        "con archivos vacíos con nombre de DLL junto al .exe la ventana no abre o no pinta: se cargó algo de su carpeta"
    );
}

fn u16_at(b: &[u8], o: usize) -> usize {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap()) as usize
}

fn u32_at(b: &[u8], o: usize) -> usize {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as usize
}

/// `DependentLoadFlags` from the PE load-configuration directory (what `/DEPENDENTLOADFLAG` writes), if present.
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
