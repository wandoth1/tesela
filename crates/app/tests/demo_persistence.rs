//! `--demo` does not read or write ANYTHING under `%APPDATA%` / `%LOCALAPPDATA%` ("nothing is read from your disk"): launch the real
//! executable with both variables pointing to a test-owned temporary folder, close the window as the user would (WM_CLOSE, when
//! `Shell::on_exit` would save settings), and verify that the folder remains EMPTY.
#![allow(unsafe_code)]

mod common;

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

/// `EnumWindows` callback: stores the first visible window belonging to the target process.
unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> i32 {
    // SAFETY: `lparam` is the `*mut Find` passed by `window_of`, which remains alive for the entire enumeration.
    let find = unsafe { &mut *(lparam as *mut Find) };
    let mut pid = 0u32;
    // SAFETY: `hwnd` is supplied by the system and `pid` is a writable local integer.
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    let mut title = [0u16; 16];
    // SAFETY: `title` is a writable local buffer of 16 UTF-16 units.
    let n = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
    let is_main = String::from_utf16_lossy(&title[..n.max(0) as usize]) == "Tesela";
    // SAFETY: `IsWindowVisible` only reads the window state. The main window ("Tesela") is visible only after rendering the
    // first frame; the process also has auxiliary windows (winit event target, IME) that do not qualify.
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
    // SAFETY: `visit` is a valid window callback and `find` remains alive until `EnumWindows` returns.
    unsafe { EnumWindows(Some(visit), &mut find as *mut Find as LPARAM) };
    (!find.hwnd.is_null()).then_some(find.hwnd)
}

fn count_entries(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |d| d.count())
}

/// Waits for the process to show its window (up to `limit`); returns `None` if it does not (no graphical desktop).
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
    if !common::process_tests_enabled() {
        return;
    }
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
    // Allow a few frames, then close as the user would: this is when settings and window size would be saved.
    std::thread::sleep(Duration::from_millis(2000));
    // SAFETY: `hwnd` belongs to THIS child process; WM_CLOSE carries no pointers.
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

/// The measurement tools (`--idle-memory`, `--bench-hover`) do not save anything either, even with `--demo` and no window
/// to close manually: they exit on their own. `--idle-memory` also works WITHOUT `--demo` (X1: measurement-tool isolation
/// does not depend on the demo; with `APPDATA`/`LOCALAPPDATA` redirected, nothing from settings or history remains).
#[test]
fn las_herramientas_de_medicion_no_dejan_nada_en_appdata() {
    if !common::process_tests_enabled() {
        return;
    }
    let casos: [&[&str]; 3] = [
        &["--demo", "--idle-memory", "1"],
        &["--demo", "--bench-hover", "1"],
        &["--idle-memory", "1"],
    ];
    for args in casos {
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
        // The tool may fail without a desktop, but it must not leave files. If it succeeds, it must have measured something
        // (so isolation is tested with the tool working, not with an argument error).
        if args.contains(&"--idle-memory") && out.status.success() {
            assert!(
                String::from_utf8_lossy(&out.stderr).contains("idle-memory:"),
                "{args:?}: no midió"
            );
        }
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
