//! `tesela-scan`: escanea una ruta en SOLO LECTURA con el escáner de Tesela e imprime el top-N por tamaño en disco, los
//! totales, los enlaces duros deduplicados, el espacio no explicado, el tiempo y los errores.
//!
//! ```text
//! tesela-scan <ruta> [--threads N] [--top N] [--json archivo|-] [--overwrite] [--no-priority] [--fallback]
//!                    [--stop-after SEGUNDOS] [--mem] [--verbose]
//! ```
//!
//! * `--top N`: los N hijos más grandes de la ruta (por defecto 20; 0 = ninguno).
//! * `--json archivo`: escribe el resumen y el top-N en JSON. NUNCA pisa un archivo que ya exista (falla antes de
//!   escanear y, por si aparece mientras tanto, al escribir con `create_new`); `--overwrite` lo reemplaza
//!   explícitamente. `--json -` lo escribe por la salida estándar y el informe legible pasa a la salida de errores.
//!   ATENCIÓN: contiene nombres de carpetas reales; no lo versiones (`*.scan.json` y `/scan-out/` están en
//!   `.gitignore`).
//! * `--no-priority`: sin cola prioritaria. `--fallback`: fuerza `FindFirstFileExW`.
//! * `--stop-after S`: "Parar y ver" a los S segundos (el árbol se construye con lo recibido).
//! * `--mem`: imprime el private working set (antes, pico muestreado y en régimen con el árbol vivo).
//! * `--verbose`: lista las rutas prioritarias y las muestras de error.

mod dll_hardening;
mod memory;

use serde_json::json;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};
use tesela_model::{NodeId, Tree, flags};
use tesela_scanner::{ApiUsed, ScanOptions, ScanResult, start};

/// Destino del JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
enum JsonTarget {
    /// `--json -`: la salida estándar.
    Stdout,
    File(PathBuf),
}

#[derive(Debug)]
struct Args {
    root: PathBuf,
    threads: usize,
    top: usize,
    json: Option<JsonTarget>,
    overwrite: bool,
    no_priority: bool,
    fallback: bool,
    stop_after: Option<Duration>,
    mem: bool,
    verbose: bool,
}

const USAGE: &str = "uso: tesela-scan <ruta> [--threads N] [--top N] [--json archivo|-] [--overwrite] [--no-priority] [--fallback] [--stop-after SEGUNDOS] [--mem] [--verbose] | --version
  --json archivo   escribe el resumen en JSON; NO pisa un archivo que ya exista
  --json -         escribe el JSON por la salida estandar (el informe legible va a la de errores)
  --overwrite      con --json archivo: reemplaza el archivo si ya existe";

/// Interpreta los argumentos. Llegan como `OsString` (`args_os`): una ruta con caracteres que no son Unicode válido (un
/// nombre de carpeta con un sustituto desparejado, por ejemplo) no puede hacer que el programa entre en pánico ni que se
/// pierda; las opciones y sus valores numéricos sí tienen que ser Unicode.
fn parse_args(mut it: impl Iterator<Item = OsString>) -> Result<Args, String> {
    let mut a = Args {
        root: PathBuf::new(),
        threads: tesela_scanner::default_threads(),
        top: 20,
        json: None,
        overwrite: false,
        no_priority: false,
        fallback: false,
        stop_after: None,
        mem: false,
        verbose: false,
    };
    let mut have_root = false;
    let num = |flag: &str, v: Option<OsString>| -> Result<u64, String> {
        v.ok_or_else(|| format!("{flag} necesita un valor"))?
            .to_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("{flag}: no es un número"))
    };
    while let Some(arg) = it.next() {
        // Una ruta (o cualquier argumento) que no es Unicode válido no es una opción: se trata como la ruta a escanear.
        let Some(text) = arg.to_str() else {
            if have_root {
                return Err("solo se admite una ruta".into());
            }
            a.root = arg.into();
            have_root = true;
            continue;
        };
        match text {
            "--threads" => a.threads = num("--threads", it.next())?.max(1) as usize,
            "--top" => a.top = num("--top", it.next())? as usize,
            "--json" => {
                let v = it
                    .next()
                    .ok_or("--json necesita un archivo (o `-` para la salida estándar)")?;
                a.json = Some(if v == "-" {
                    JsonTarget::Stdout
                } else {
                    JsonTarget::File(v.into())
                });
            }
            "--overwrite" => a.overwrite = true,
            "--no-priority" => a.no_priority = true,
            "--fallback" => a.fallback = true,
            "--stop-after" => {
                a.stop_after = Some(Duration::from_secs(num("--stop-after", it.next())?))
            }
            "--mem" => a.mem = true,
            "--verbose" => a.verbose = true,
            "-h" | "--help" => return Err(String::new()),
            s if s.starts_with("--") => return Err(format!("opción desconocida: {s}")),
            _ => {
                if have_root {
                    return Err("solo se admite una ruta".into());
                }
                a.root = arg.into();
                have_root = true;
            }
        }
    }
    if !have_root {
        return Err("falta la ruta a escanear".into());
    }
    if a.overwrite && !matches!(a.json, Some(JsonTarget::File(_))) {
        return Err("--overwrite solo tiene sentido con --json <archivo>".into());
    }
    Ok(a)
}

/// ¿Tiene sentido imprimir el "espacio no explicado"? Solo en un escaneo TERMINADO de un volumen completo, con las cifras
/// del VOLUMEN (no las de una cuota), sin directorios sin recorrer y sin error al construir el árbol: con cualquiera de
/// ellos el residuo (usado − escaneado) mezclaría lo que falta por recorrer con lo realmente inexplicado.
fn unexplained_applies(
    full_volume: bool,
    cancelled: bool,
    volume_scope: bool,
    incomplete_dirs: u64,
    build_error: bool,
) -> bool {
    full_volume && !cancelled && volume_scope && incomplete_dirs == 0 && !build_error
}

/// Tamaño legible con la unidad que corresponda (B, KiB, MiB, GiB o TiB).
fn gib(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{b} B")
    } else {
        format!("{v:.2} {}", UNITS[u])
    }
}

fn mib(b: u64) -> String {
    format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0))
}

/// `println!` hacia un escritor cualquiera (la salida estándar, o la de errores si el JSON ocupa la estándar).
macro_rules! outln {
    ($o:expr, $($a:tt)*) => {
        {
            let _ = writeln!($o, $($a)*);
        }
    };
}

fn flag_names(f: u16) -> Vec<&'static str> {
    [
        (flags::DIR, "dir"),
        (flags::REPARSE, "reparse"),
        (flags::CLOUD, "cloud"),
        (flags::HARDLINK, "hardlink"),
        (flags::INCOMPLETE, "incomplete"),
        (flags::INCOMPLETE_BELOW, "incomplete_below"),
        (flags::SYSTEM_INFO, "system_info"),
        (flags::ERROR, "error"),
    ]
    .into_iter()
    .filter(|(bit, _)| f & bit != 0)
    .map(|(_, n)| n)
    .collect()
}

fn top_children(tree: &Tree, n: usize) -> Vec<NodeId> {
    tree.children(tree.root()).take(n).collect()
}

fn report(out: &mut dyn Write, args: &Args, r: &ScanResult, mem: Option<(u64, u64, u64)>) {
    let (tree, s) = (&r.tree, &r.summary);
    let st = tree.stats();
    let root = tree.node(tree.root());
    outln!(out, "Ruta: {}", s.root.display());
    outln!(
        out,
        "Tiempo: enumeración {:.2} s + construcción del árbol {:.2} s = {:.2} s ({} hilos, API {}, sistema de archivos {})",
        s.elapsed_scan.as_secs_f64(),
        s.elapsed_build.as_secs_f64(),
        (s.elapsed_scan + s.elapsed_build).as_secs_f64(),
        s.threads,
        match s.api {
            ApiUsed::Extended => "extendida",
            ApiUsed::FindFallback =>
                "FindFirstFileExW (respaldo: alloc estimado, sin dedup de enlaces duros)",
            ApiUsed::Mixed => "extendida con respaldo parcial",
        },
        if s.fs_name.is_empty() {
            "desconocido"
        } else {
            &s.fs_name
        },
    );
    if s.cancelled {
        outln!(
            out,
            "ESCANEO DETENIDO (parar y ver): los directorios marcados incompletos son un mínimo."
        );
    }
    outln!(
        out,
        "Archivos: {} · directorios: {} · reparse points de directorio (no seguidos): {}",
        st.files,
        st.dirs,
        st.reparse_dirs
    );
    outln!(
        out,
        "Archivos con flag de reparse: {} · de la nube: {} · directorios incompletos: {} · con error: {}",
        st.reparse_files,
        st.cloud_entries,
        st.incomplete_dirs,
        st.error_dirs
    );
    outln!(
        out,
        "Escaneado en disco: {} · tamaño lógico: {}",
        gib(st.scanned_alloc),
        gib(root.logical)
    );
    outln!(
        out,
        "Hard links deduplicados: {} entradas en {} grupos · {} evitados en disco ({} lógicos)",
        st.hardlink_extra_entries,
        st.hardlink_groups,
        gib(st.hardlink_alloc_saved),
        gib(st.hardlink_logical_saved)
    );
    match s.volume {
        Some(v) => {
            let used = v.total.saturating_sub(v.free);
            outln!(
                out,
                "Volumen: {} usados de {} ({} libres)",
                gib(used),
                gib(v.total),
                gib(v.free)
            );
            if !v.volume_scope {
                outln!(
                    out,
                    "Cuota de disco: las cifras de arriba son las de tu usuario (disponible {}); Windows no da aquí la capacidad real del volumen, así que no se calcula el espacio no explicado",
                    gib(v.available)
                );
            } else if v.quota_limited() {
                outln!(
                    out,
                    "Cuota de disco: disponible para tu usuario {} (menos que lo libre del volumen; el espacio no explicado usa solo magnitudes del volumen)",
                    gib(v.available)
                );
            }
            if unexplained_applies(
                s.full_volume,
                s.cancelled,
                v.volume_scope,
                st.incomplete_dirs,
                s.build_error.is_some(),
            ) {
                let pct = if used > 0 {
                    st.unexplained_alloc as f64 * 100.0 / used as f64
                } else {
                    0.0
                };
                outln!(
                    out,
                    "Espacio no explicado (total − libre − escaneado, recortado a ≥ 0): {} ({pct:.2} % de lo usado)",
                    gib(st.unexplained_alloc)
                );
            } else {
                outln!(
                    out,
                    "Espacio no explicado: no aplica (solo en un escaneo terminado de un volumen completo)"
                );
            }
        }
        None => outln!(out, "Volumen: no se pudo consultar el espacio"),
    }
    if !s.priority.is_empty() {
        let done = s.priority.iter().filter(|p| p.done).count();
        let bytes: u64 = s.priority.iter().map(|p| p.alloc).sum();
        outln!(
            out,
            "Rutas prioritarias: {} ({} recorridas enteras, {} vistos)",
            s.priority.len(),
            done,
            gib(bytes)
        );
        if args.verbose {
            for p in &s.priority {
                outln!(
                    out,
                    "  {} · {} · {} archivos{}",
                    p.path.display(),
                    gib(p.alloc),
                    p.files,
                    if p.done { "" } else { " (sin terminar)" }
                );
            }
        }
    }
    if s.errors() == 0 {
        outln!(out, "Errores: 0");
    } else {
        outln!(out, "Errores: {} directorios sin poder listar", s.errors());
        for (code, n) in &s.errors_by_code {
            outln!(
                out,
                "  código {code}: {n} ({})",
                std::io::Error::from_raw_os_error(*code as i32)
            );
        }
        if args.verbose {
            for (code, p) in &s.error_samples {
                outln!(out, "  ejemplo (código {code}): {}", p.display());
            }
        }
    }
    if s.ids_exhausted {
        outln!(
            out,
            "ERROR: se agotaron los ids provisionales (demasiadas entradas): el árbol está incompleto"
        );
    }
    if let Some(e) = s.build_error {
        outln!(out, "ERROR al construir el árbol: {e}");
    }
    if let Some((before, peak, steady)) = mem {
        outln!(
            out,
            "Memoria (private working set): antes {} · pico muestreado durante escaneo y construcción {} · en régimen con el árbol {} ({} nodos, {} del árbol)",
            mib(before),
            mib(peak),
            mib(steady),
            tree.len(),
            mib(tree.heap_bytes() as u64)
        );
    }
    if args.top > 0 {
        outln!(
            out,
            "\nTop {} por tamaño en disco (hijos directos de la ruta):",
            args.top
        );
        for id in top_children(tree, args.top) {
            let n = tree.node(id);
            let pct = if root.alloc > 0 {
                n.alloc as f64 * 100.0 / root.alloc as f64
            } else {
                0.0
            };
            let kind = if n.flags & flags::SYSTEM_INFO != 0 {
                "SIS"
            } else if n.is_dir() && n.flags & flags::CLOUD != 0 {
                "NUBE"
            } else if n.flags & flags::REPARSE != 0 && n.is_dir() {
                "ENL"
            } else if n.is_dir() {
                "DIR"
            } else {
                "ARC"
            };
            let name = if n.flags & flags::SYSTEM_INFO != 0 {
                "(windows y otros datos del sistema)".into()
            } else {
                tree.name_lossy(id).into_owned()
            };
            // "≥": el directorio o algo de lo que contiene no se llegó a recorrer entero.
            let inc = if n.flags & (flags::INCOMPLETE | flags::INCOMPLETE_BELOW) != 0 {
                " ≥"
            } else {
                ""
            };
            outln!(
                out,
                "  {:>11}{inc} {pct:>5.1} %  {kind}  {} archivos  {name}",
                gib(n.alloc),
                n.files_below
            );
        }
    }
}

/// Documento JSON del escaneo.
fn json_doc(args: &Args, r: &ScanResult) -> String {
    let (tree, s) = (&r.tree, &r.summary);
    let st = tree.stats();
    let root = tree.node(tree.root());
    let top: Vec<_> = top_children(tree, args.top)
        .into_iter()
        .map(|id| {
            let n = tree.node(id);
            json!({
                "name": tree.name_lossy(id),
                "alloc": n.alloc, "logical": n.logical, "files_below": n.files_below,
                "flags": flag_names(n.flags),
            })
        })
        .collect();
    let doc = json!({
        "root": s.root.display().to_string(),
        "cancelled": s.cancelled,
        "elapsed_scan_s": s.elapsed_scan.as_secs_f64(),
        "elapsed_build_s": s.elapsed_build.as_secs_f64(),
        "threads": s.threads,
        "api": format!("{:?}", s.api),
        "fs": s.fs_name,
        "totals": {
            "files": st.files, "dirs": st.dirs, "reparse_dirs": st.reparse_dirs,
            "alloc": root.alloc, "logical": root.logical, "scanned_alloc": st.scanned_alloc,
            "unexplained_alloc": st.unexplained_alloc,
            "incomplete_dirs": st.incomplete_dirs, "error_dirs": st.error_dirs,
        },
        "hardlinks": {
            "groups": st.hardlink_groups, "extra_entries": st.hardlink_extra_entries,
            "alloc_saved": st.hardlink_alloc_saved, "logical_saved": st.hardlink_logical_saved,
        },
        "volume": s.volume.map(|v| json!({"total": v.total, "free": v.free, "available": v.available, "volume_scope": v.volume_scope})),
        "errors_by_code": s.errors_by_code,
        "ids_exhausted": s.ids_exhausted,
        "top": top,
    });
    serde_json::to_string_pretty(&doc).expect("JSON") + "\n"
}

/// Abre el archivo de salida SIN pisar nada (`create_new`), o reemplazándolo si `overwrite`.
fn open_output(path: &Path, overwrite: bool) -> std::io::Result<std::fs::File> {
    let mut o = OpenOptions::new();
    o.write(true);
    if overwrite {
        o.create(true).truncate(true);
    } else {
        o.create_new(true);
    }
    o.open(path)
}

/// Frase en español para un error del escáner (la biblioteca devuelve errores tipados, sin texto para el usuario).
fn explain(e: &tesela_scanner::ScanError) -> String {
    use tesela_scanner::ScanError;
    match e {
        ScanError::Root(io) => format!("no se puede abrir la ruta a escanear: {io}"),
        ScanError::NotADirectory(_) => "la ruta a escanear no es un directorio".into(),
        ScanError::RootIsLink(_) => {
            "la ruta a escanear es un enlace (junction o symlink) y los enlaces no se siguen: elige la carpeta de destino"
                .into()
        }
        ScanError::Panicked => "un hilo del escáner entró en pánico".into(),
    }
}

fn exists_error(path: &Path) -> String {
    format!(
        "{} ya existe: no se pisa. Usa otro nombre o --overwrite para reemplazarlo",
        path.display()
    )
}

/// Escribe el JSON en `target`. Devuelve un mensaje de error legible.
fn write_json(
    target: &JsonTarget,
    overwrite: bool,
    doc: &str,
    stdout: &mut dyn Write,
) -> Result<(), String> {
    match target {
        JsonTarget::Stdout => stdout
            .write_all(doc.as_bytes())
            .and_then(|()| stdout.flush())
            .map_err(|e| format!("no se pudo escribir el JSON en la salida estándar: {e}")),
        JsonTarget::File(path) => {
            let mut f = open_output(path, overwrite).map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    exists_error(path)
                } else {
                    format!("no se pudo escribir el JSON: {e}")
                }
            })?;
            f.write_all(doc.as_bytes())
                .map_err(|e| format!("no se pudo escribir el JSON: {e}"))
        }
    }
}

fn main() -> ExitCode {
    // LO PRIMERO: la búsqueda de DLL se limita a System32 (ver `dll_hardening`).
    let _ = dll_hardening::restrict_dll_search();
    // `--version` (o `-V`) como primer argumento: imprime la version por la salida estandar y sale (sin escanear).
    if matches!(
        std::env::args_os()
            .nth(1)
            .as_deref()
            .and_then(|a| a.to_str()),
        Some("--version" | "-V")
    ) {
        println!("tesela-scan {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let args = match parse_args(std::env::args_os().skip(1)) {
        Ok(a) => a,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("error: {msg}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    // Falla ANTES de escanear (puede tardar mucho) si el archivo ya existe y no se pidió reemplazarlo.
    if let Some(JsonTarget::File(p)) = &args.json
        && !args.overwrite
        && p.exists()
    {
        eprintln!("error: {}", exists_error(p));
        return ExitCode::from(1);
    }
    let mut opts = ScanOptions::new(&args.root);
    opts.threads = args.threads;
    opts.known_paths = !args.no_priority;
    opts.force_find = args.fallback;
    let before = memory::private_working_set().unwrap_or(0);
    let sampler = args.mem.then(memory::PeakSampler::start);
    let t0 = Instant::now();
    let handle = match start(opts) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error: {}", explain(&e));
            return ExitCode::from(1);
        }
    };
    let tty = std::io::stderr().is_terminal();
    let mut last_print = Instant::now();
    let mut stopped = false;
    while !handle.is_finished() {
        std::thread::sleep(Duration::from_millis(50));
        if let Some(limit) = args.stop_after
            && !stopped
            && t0.elapsed() >= limit
        {
            handle.cancel();
            stopped = true;
        }
        if tty && last_print.elapsed() >= Duration::from_secs(1) {
            let p = handle.progress();
            eprint!(
                "\r  {} archivos · {} directorios · {}        ",
                p.files.load(std::sync::atomic::Ordering::Relaxed),
                p.dirs.load(std::sync::atomic::Ordering::Relaxed),
                gib(p.alloc.load(std::sync::atomic::Ordering::Relaxed))
            );
            last_print = Instant::now();
        }
    }
    if tty {
        eprint!("\r{:60}\r", "");
    }
    let result = match handle.wait() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {}", explain(&e));
            return ExitCode::from(1);
        }
    };
    let mem = sampler.map(|s| {
        let peak = s.finish();
        (before, peak, memory::private_working_set().unwrap_or(0))
    });
    // Con `--json -` el JSON ocupa la salida estándar: el informe legible va a la de errores.
    let to_stdout = args.json == Some(JsonTarget::Stdout);
    if to_stdout {
        report(&mut std::io::stderr(), &args, &result, mem);
    } else {
        report(&mut std::io::stdout(), &args, &result, mem);
    }
    if let Some(target) = &args.json {
        let doc = json_doc(&args, &result);
        if let Err(e) = write_json(target, args.overwrite, &doc, &mut std::io::stdout()) {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
        if let JsonTarget::File(path) = target {
            println!(
                "\nJSON escrito en {} (contiene nombres reales: no lo versiones)",
                path.display()
            );
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStringExt;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn el_espacio_no_explicado_solo_sale_con_un_escaneo_completo_del_volumen() {
        assert!(unexplained_applies(true, false, true, 0, false));
        // Cada condición por separado lo apaga (GM-m8).
        assert!(
            !unexplained_applies(false, false, true, 0, false),
            "no es un volumen entero"
        );
        assert!(!unexplained_applies(true, true, true, 0, false), "detenido");
        assert!(
            !unexplained_applies(true, false, false, 0, false),
            "cifras de la cuota, no del volumen"
        );
        assert!(
            !unexplained_applies(true, false, true, 1, false),
            "directorios sin recorrer"
        );
        assert!(
            !unexplained_applies(true, false, true, 0, true),
            "error al construir el árbol"
        );
    }

    #[test]
    fn una_ruta_que_no_es_unicode_valido_se_conserva_y_no_provoca_panico() {
        // Un sustituto desparejado (U+D800) es un nombre válido en NTFS y no es UTF-8/Unicode válido.
        let raw = OsString::from_wide(&[b'C' as u16, b':' as u16, 0x5C, 0xD800, b'x' as u16]);
        assert!(raw.to_str().is_none());
        let a = parse_args([raw.clone(), OsString::from("--top"), OsString::from("3")].into_iter())
            .unwrap();
        assert_eq!(a.root.as_os_str(), raw.as_os_str());
        assert_eq!(a.top, 3);
        // También como valor de `--json`, y una segunda ruta sigue siendo un error de uso.
        let a =
            parse_args([OsString::from("X"), OsString::from("--json"), raw.clone()].into_iter())
                .unwrap();
        assert_eq!(a.json, Some(JsonTarget::File(PathBuf::from(raw.clone()))));
        assert!(parse_args([OsString::from("X"), raw].into_iter()).is_err());
    }

    #[test]
    fn los_argumentos_normales_se_siguen_interpretando() {
        let a = parse(&["C:/x", "--threads", "3", "--top", "0", "--mem", "--verbose"]).unwrap();
        assert_eq!((a.threads, a.top, a.mem, a.verbose), (3, 0, true, true));
        assert!(parse(&[]).is_err());
        assert!(
            parse(&["x", "--top", "no"])
                .unwrap_err()
                .contains("no es un número")
        );
        assert!(parse(&["x", "--cosa"]).unwrap_err().contains("desconocida"));
        assert!(parse(&["x", "--overwrite"]).is_err());
        assert_eq!(
            parse(&["x", "--json", "-"]).unwrap().json,
            Some(JsonTarget::Stdout)
        );
        assert_eq!(parse(&["--help"]).unwrap_err(), "");
    }
}
