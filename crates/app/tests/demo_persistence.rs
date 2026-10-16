//! `--demo` no lee ni guarda NADA bajo `%APPDATA%` / `%LOCALAPPDATA%` ("nada se lee de tu disco"): se lanza el ejecutable real con
//! las dos variables apuntando a una carpeta temporal propia, se cierra la ventana como lo haría el usuario (WM_CLOSE, que es
//! cuando `Shell::on_exit` guardaría la configuración) y la carpeta tiene que seguir VACÍA.
#![allow(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
};

struct Tmp(PathBuf);

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Find {
    pid: u32,
    hwnd: HWND,
}

/// Función de ventana de `EnumWindows`: guarda la primera ventana visible del proceso buscado.
unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> i32 {
    // SAFETY: `lparam` es el `*mut Find` que pasa `window_of`, vivo durante toda la enumeración.
    let find = unsafe { &mut *(lparam as *mut Find) };
    let mut pid = 0u32;
    // SAFETY: `hwnd` lo da el sistema y `pid` es un entero local escribible.
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    let mut title = [0u16; 16];
    // SAFETY: `title` es un búfer local escribible de 16 unidades UTF-16.
    let n = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
    let is_main = String::from_utf16_lossy(&title[..n.max(0) as usize]) == "Tesela";
    // SAFETY: `IsWindowVisible` solo lee el estado de la ventana. La ventana principal ("Tesela") solo es visible tras pintar el
    // primer fotograma; el proceso tiene además ventanas auxiliares (destino de eventos de winit, IME) que no sirven.
    if pid == find.pid && is_main && unsafe { IsWindowVisible(hwnd) } != 0 {
        find.hwnd = hwnd;
        return 0; // encontrada: se para
    }
    1
}

fn window_of(pid: u32) -> Option<HWND> {
    let mut find = Find {
        pid,
        hwnd: std::ptr::null_mut(),
    };
    // SAFETY: `visit` es una función de ventana válida y `find` vive hasta que `EnumWindows` vuelve.
    unsafe { EnumWindows(Some(visit), &mut find as *mut Find as LPARAM) };
    (!find.hwnd.is_null()).then_some(find.hwnd)
}

fn count_entries(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |d| d.count())
}

/// Espera a que el proceso muestre su ventana (hasta `limit`); `None` si no llega (sin escritorio gráfico).
fn wait_for_window(child: &mut Child, limit: Duration) -> Option<HWND> {
    let start = Instant::now();
    while start.elapsed() < limit {
        if child.try_wait().ok().flatten().is_some() {
            return None;
        }
        if let Some(h) = window_of(child.id()) {
            return Some(h);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    None
}

#[test]
fn una_sesion_de_demostracion_no_deja_nada_en_appdata() {
    let dir = Tmp(std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4())));
    let (roaming, local) = (dir.0.join("Roaming"), dir.0.join("Local"));
    std::fs::create_dir_all(&roaming).unwrap();
    std::fs::create_dir_all(&local).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tesela"))
        .arg("--demo")
        .env("APPDATA", &roaming)
        .env("LOCALAPPDATA", &local)
        .spawn()
        .unwrap();
    let Some(hwnd) = wait_for_window(&mut child, Duration::from_secs(40)) else {
        let _ = child.kill();
        let _ = child.wait();
        eprintln!("sin ventana en este entorno (no hay escritorio gráfico o DX12): se omite");
        return;
    };
    // Unos fotogramas, y se cierra como lo haría el usuario: ahí se guardaría la configuración y el tamaño de la ventana.
    std::thread::sleep(Duration::from_millis(2000));
    // SAFETY: `hwnd` es una ventana de ESTE proceso hijo; WM_CLOSE no lleva punteros.
    unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("la demostración no se cerró con WM_CLOSE en 30 s");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(status.success(), "salida: {status:?}");
    assert_eq!(
        count_entries(&roaming),
        0,
        "--demo escribió bajo %APPDATA%: {:?}",
        std::fs::read_dir(&roaming)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        count_entries(&local),
        0,
        "--demo escribió bajo %LOCALAPPDATA%"
    );
}

/// Las herramientas de medición (`--idle-memory`, `--bench-hover`) tampoco guardan nada, ni siquiera con `--demo` y sin ventana
/// que cerrar a mano: terminan solas.
#[test]
fn las_herramientas_de_medicion_no_dejan_nada_en_appdata() {
    for args in [
        ["--demo", "--idle-memory", "1"],
        ["--demo", "--bench-hover", "1"],
    ] {
        let dir = Tmp(std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4())));
        let (roaming, local) = (dir.0.join("Roaming"), dir.0.join("Local"));
        std::fs::create_dir_all(&roaming).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_tesela"))
            .args(args)
            .env("APPDATA", &roaming)
            .env("LOCALAPPDATA", &local)
            .output()
            .unwrap();
        // Si no hay escritorio la herramienta puede fallar; lo que no puede es dejar archivos.
        let _ = out;
        assert_eq!(
            count_entries(&roaming),
            0,
            "{args:?} escribió bajo %APPDATA%"
        );
        assert_eq!(
            count_entries(&local),
            0,
            "{args:?} escribió bajo %LOCALAPPDATA%"
        );
    }
}
