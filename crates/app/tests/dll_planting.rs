//! Una DLL con el nombre de una importación estática dejada junto al `.exe` no se carga: se comprueba con archivos VACÍOS (no
//! son DLL y no ejecutan nada) con los nombres de las DLL que `tesela.exe` importa y de las que `wgpu` carga por nombre.
//!
//! Sin `/DEPENDENTLOADFLAG:0x800` (build.rs) el cargador de Windows resuelve las importaciones estáticas con el orden
//! predeterminado, que mira la carpeta del `.exe` ANTES que `System32`: un `dxgi.dll` vacío al lado hace que el proceso ni
//! arranque (código de salida distinto de 0 antes de llegar a `main`). Con la bandera arranca y sale con 0.

mod common;

use std::path::PathBuf;
use std::process::Command;

/// Nombres de las DLL que `tesela.exe` importa de forma estática (y no son `KnownDLLs`) o que se cargan por nombre.
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

/// ¿Abre la ventana y pinta? `--demo --bench-hover 1` imprime los frames medidos; si no llega a pintar, no hay cifras. Se
/// reintenta porque la medición es intermitente (ventana sin pintar al arrancar) también sin ninguna DLL falsa.
fn paints(exe: &std::path::Path) -> bool {
    // Defensa en profundidad: el perfil del proceso (APPDATA y LOCALAPPDATA) apunta a carpetas temporales propias; ninguna prueba
    // puede tocar el perfil real del usuario.
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

/// Con archivos vacíos con nombre de DLL junto al `.exe` (`d3d12`, `dcomp`, `d3dcompiler_47`, `dxcompiler`, `dxil`... las que
/// `wgpu` carga POR NOMBRE al abrir la ventana), el programa abre y pinta igual: se cargan las de `System32`. Es la prueba de
/// `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)`: con `LOAD_LIBRARY_SEARCH_DEFAULT_DIRS` (que incluye la carpeta de
/// la aplicación) o sin la llamada, `wgpu` cargaría el archivo vacío y no podría crear la superficie. (`--version` no sirve: sale
/// antes de que nada cargue por nombre.) Sin escritorio gráfico o sin DX12 el CONTROL (sin archivos falsos) tampoco pinta: entonces
/// no hay nada que comparar y la prueba se omite.
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
        // En CI no se omite: sin esto, un runner sin escritorio gráfico dejaría la prueba en verde sin comprobar nada.
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
