//! `--screenshot` falla con un código distinto de 0 si no puede guardar la captura, ANTES de abrir ninguna ventana.
//! Solo se compila con la feature `capture` (sin ella el binario rechaza `--screenshot`).
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
    // Defensa en profundidad: el perfil del proceso apunta a una carpeta temporal propia, nunca al del usuario.
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
    // Directorio de destino inexistente.
    let (code, _) = run(&[&t.0.join("no-existe").join("x.png")]);
    assert_eq!(code, 1);
    assert!(!t.0.join("no-existe").exists());
    // El destino es una carpeta.
    let (code, _) = run(&[&t.0]);
    assert_eq!(code, 1);
    // El archivo ya existe y no se pidió --overwrite: no se toca.
    let existing = t.0.join("c.png");
    fs::write(&existing, b"previo").unwrap();
    let (code, _) = run(&[&existing]);
    assert_eq!(code, 1);
    assert_eq!(fs::read(&existing).unwrap(), b"previo");
}
