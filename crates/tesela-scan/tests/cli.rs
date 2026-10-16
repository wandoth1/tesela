//! Prueba de la CLI `tesela-scan` sobre un árbol sintético en `%TEMP%\tesela-test-<uuid>` (nunca sobre un disco real).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Árbol temporal con guarda RAII: solo se borra algo si es `%TEMP%\tesela-test-*` creado aquí.
struct TempTree(PathBuf);

impl TempTree {
    fn new() -> TempTree {
        let p = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&p).unwrap();
        TempTree(p)
    }
    fn write(&self, rel: &str, size: usize) {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![0xA5u8; size]).unwrap();
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let ok = self.0.parent() == Some(std::env::temp_dir().as_path())
            && self
                .0
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("tesela-test-"));
        if ok {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

fn run(args: &[&Path]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_tesela-scan"))
        .args(args)
        .output()
        .expect("lanza tesela-scan");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn escanea_un_arbol_sintetico_e_imprime_totales_top_y_json() {
    let t = TempTree::new();
    t.write("grande/a.bin", 3_000_000);
    t.write("grande/sub/b.bin", 1_000_000);
    t.write("pequena/c.txt", 50_000);
    t.write("d.zip", 200_000);
    fs::hard_link(t.0.join("d.zip"), t.0.join("pequena/enlace.zip")).unwrap();
    let json_path = t.0.join("salida.scan.json");
    let (code, out, err) = run(&[
        &t.0,
        Path::new("--threads"),
        Path::new("3"),
        Path::new("--top"),
        Path::new("3"),
        Path::new("--json"),
        &json_path,
        Path::new("--mem"),
    ]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("Archivos: 5 · directorios: 4"), "{out}");
    assert!(
        out.contains("Hard links deduplicados: 1 entradas en 1 grupos"),
        "{out}"
    );
    assert!(out.contains("Errores: 0"));
    assert!(out.contains("Espacio no explicado: no aplica"));
    assert!(out.contains("Memoria (private working set)"));
    let top_lines = out
        .lines()
        .skip_while(|l| !l.starts_with("Top 3"))
        .skip(1)
        .take_while(|l| !l.is_empty())
        .count();
    assert_eq!(top_lines, 3, "{out}");
    assert!(
        out.find("grande").unwrap() < out.find("pequena").unwrap(),
        "el mayor va primero:
{out}"
    );

    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&json_path).unwrap()).unwrap();
    assert_eq!(doc["totals"]["files"], 5);
    assert_eq!(doc["totals"]["dirs"], 4);
    assert_eq!(doc["hardlinks"]["extra_entries"], 1);
    assert_eq!(doc["top"].as_array().unwrap().len(), 3);
    assert_eq!(doc["top"][0]["name"], "grande", "el mayor va primero");
    assert_eq!(doc["cancelled"], false);
    assert_eq!(doc["errors_by_code"].as_object().unwrap().len(), 0);
}

#[test]
fn top_cero_no_imprime_el_listado_y_el_respaldo_se_indica() {
    let t = TempTree::new();
    t.write("x/a.bin", 10_000);
    let (code, out, _) = run(&[
        &t.0,
        Path::new("--top"),
        Path::new("0"),
        Path::new("--fallback"),
    ]);
    assert_eq!(code, 0);
    assert!(!out.contains("Top "), "{out}");
    assert!(out.contains("FindFirstFileExW"), "{out}");
}

#[test]
fn errores_de_uso_y_de_ruta_dan_codigos_de_salida() {
    let (code, _, err) = run(&[]);
    assert_eq!(code, 2);
    assert!(err.contains("falta la ruta"));
    // El uso documenta `--overwrite` y `--json -`.
    assert!(
        err.contains("--overwrite") && err.contains("--json -"),
        "{err}"
    );
    let (code, _, _) = run(&[Path::new("--nada")]);
    assert_eq!(code, 2);
    let t = TempTree::new();
    let (code, _, err) = run(&[&t.0.join("no-existe")]);
    assert_eq!(code, 1);
    assert!(err.starts_with("error:"), "{err}");
}

#[test]
fn parar_y_ver_a_los_cero_segundos_da_un_arbol_valido() {
    let t = TempTree::new();
    for i in 0..30 {
        t.write(&format!("d{i}/f.bin"), 5000);
    }
    let (code, out, _) = run(&[
        &t.0,
        Path::new("--stop-after"),
        Path::new("0"),
        Path::new("--top"),
        Path::new("0"),
    ]);
    assert_eq!(code, 0);
    // Con 0 s la cancelación llega casi siempre antes del final; en cualquier caso el resultado es coherente.
    assert!(out.contains("Archivos: "), "{out}");
}

/// Un archivo de salida preexistente que NO debe pisarse sin `--overwrite`.
const PREVIO: &str = "contenido previo que no debe perderse";

#[test]
fn json_no_pisa_un_archivo_existente_y_no_escanea() {
    let t = TempTree::new();
    t.write("x/a.bin", 10_000);
    let salida = t.0.join("salida.json");
    fs::write(&salida, PREVIO).unwrap();
    let (code, out, err) = run(&[
        &t.0.join("x"),
        Path::new("--top"),
        Path::new("0"),
        Path::new("--json"),
        &salida,
    ]);
    assert_eq!(code, 1, "stdout: {out}");
    assert!(
        err.contains("ya existe") && err.contains("--overwrite"),
        "{err}"
    );
    assert!(out.is_empty(), "no debe haber escaneado: {out}");
    assert_eq!(fs::read_to_string(&salida).unwrap(), PREVIO);
}

#[test]
fn json_con_overwrite_reemplaza_y_sin_el_crea_el_archivo_nuevo() {
    let t = TempTree::new();
    t.write("x/a.bin", 10_000);
    let salida = t.0.join("salida.json");
    // Archivo nuevo: se crea sin --overwrite.
    let (code, _, err) = run(&[
        &t.0.join("x"),
        Path::new("--top"),
        Path::new("0"),
        Path::new("--json"),
        &salida,
    ]);
    assert_eq!(code, 0, "{err}");
    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&salida).unwrap()).unwrap();
    assert_eq!(doc["totals"]["files"], 1);
    // Con --overwrite se reemplaza.
    fs::write(&salida, PREVIO).unwrap();
    let (code, out, err) = run(&[
        &t.0.join("x"),
        Path::new("--top"),
        Path::new("0"),
        Path::new("--json"),
        &salida,
        Path::new("--overwrite"),
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("JSON escrito en"), "{out}");
    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&salida).unwrap()).unwrap();
    assert_eq!(doc["totals"]["files"], 1);
}

#[test]
fn json_menos_escribe_el_json_por_la_salida_estandar_y_el_informe_por_la_de_errores() {
    let t = TempTree::new();
    t.write("x/a.bin", 10_000);
    t.write("x/b.bin", 20_000);
    let (code, out, err) = run(&[
        &t.0.join("x"),
        Path::new("--top"),
        Path::new("1"),
        Path::new("--json"),
        Path::new("-"),
    ]);
    assert_eq!(code, 0, "{err}");
    // La salida estándar es SOLO el JSON.
    let doc: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    assert_eq!(doc["totals"]["files"], 2);
    assert!(err.contains("Archivos: 2"), "el informe va a stderr: {err}");
    // No se creó ningún archivo llamado `-`.
    assert!(!t.0.join("x").join("-").exists());
}

#[test]
fn overwrite_sin_json_de_archivo_es_un_error_de_uso() {
    let t = TempTree::new();
    let (code, _, err) = run(&[&t.0, Path::new("--overwrite")]);
    assert_eq!(code, 2);
    assert!(err.contains("--overwrite"), "{err}");
    let (code, _, _) = run(&[
        &t.0,
        Path::new("--json"),
        Path::new("-"),
        Path::new("--overwrite"),
    ]);
    assert_eq!(code, 2);
}

#[test]
fn version_se_imprime_por_la_salida_estandar_y_no_escanea() {
    for flag in ["--version", "-V"] {
        let (code, out, err) = run(&[Path::new(flag)]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            out.trim(),
            format!("tesela-scan {}", env!("CARGO_PKG_VERSION"))
        );
        assert!(err.is_empty(), "{err}");
    }
}
