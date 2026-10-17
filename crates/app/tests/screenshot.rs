//! `--screenshot` exits with a nonzero code if it cannot save the capture, BEFORE opening any window.
//! Compiled only with the `capture` feature (without it, the binary rejects `--screenshot`).
#![cfg(feature = "capture")]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Temp {
        let p = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&p).unwrap();
        Temp(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(args: &[&Path]) -> (i32, Vec<u8>) {
    // Defense in depth: the process profile points to a test-owned temporary folder, never the user's profile.
    let profile = Temp::new();
    let out = Command::new(env!("CARGO_BIN_EXE_tesela"))
        .args(["--demo", "--screenshot"])
        .args(args)
        .env("APPDATA", &profile.0)
        .env("LOCALAPPDATA", &profile.0)
        .output()
        .expect("lanza tesela");
    (out.status.code().unwrap_or(-1), out.stdout)
}

#[test]
fn si_no_puede_crear_la_captura_sale_con_error_y_no_abre_ventana() {
    if !common::process_tests_enabled() {
        return;
    }
    let t = Temp::new();
    // Destination directory does not exist.
    let (code, _) = run(&[&t.0.join("no-existe").join("x.png")]);
    assert_eq!(code, 1);
    assert!(!t.0.join("no-existe").exists());
    // The destination is a folder.
    let (code, _) = run(&[&t.0]);
    assert_eq!(code, 1);
    // The file already exists and --overwrite was not requested: leave it untouched.
    let existing = t.0.join("c.png");
    fs::write(&existing, b"previo").unwrap();
    let (code, _) = run(&[&existing]);
    assert_eq!(code, 1);
    assert_eq!(fs::read(&existing).unwrap(), b"previo");
}
