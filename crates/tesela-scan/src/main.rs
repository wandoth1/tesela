//! `tesela-scan`: escanea una ruta en SOLO LECTURA con el escáner de Tesela e imprime el top-N por tamaño en disco, los
//! totales, los enlaces duros deduplicados, el espacio no explicado, el tiempo y los errores.
//!
//! ```text
//! tesela-scan <ruta> [--threads N] [--top N] [--json archivo] [--no-priority] [--fallback]
//!                    [--stop-after SEGUNDOS] [--mem] [--verbose]
//! ```
//!
//! * `--top N`: los N hijos más grandes de la ruta (por defecto 20; 0 = ninguno).
//! * `--json archivo`: escribe el resumen y el top-N en JSON. ATENCIÓN: contiene nombres de carpetas reales; no lo
//!   versiones (`*.scan.json` y `/scan-out/` están en `.gitignore`).
//! * `--no-priority`: sin cola prioritaria. `--fallback`: fuerza `FindFirstFileExW`.
//! * `--stop-after S`: "Parar y ver" a los S segundos (el árbol se construye con lo recibido).
//! * `--mem`: imprime el private working set (antes, pico muestreado y en régimen con el árbol vivo).
//! * `--verbose`: lista las rutas prioritarias y las muestras de error.

mod memory;

use serde_json::json;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};
use tesela_model::{NodeId, Tree, flags};
use tesela_scanner::{ApiUsed, ScanOptions, ScanResult, start};

struct Args {
    root: PathBuf,
    threads: usize,
    top: usize,
    json: Option<PathBuf>,
    no_priority: bool,
    fallback: bool,
    stop_after: Option<Duration>,
    mem: bool,
    verbose: bool,
}

const USAGE: &str = "uso: tesela-scan <ruta> [--threads N] [--top N] [--json archivo] [--no-priority] [--fallback] \
                     [--stop-after SEGUNDOS] [--mem] [--verbose]";

fn parse_args(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut a = Args {
        root: PathBuf::new(),
        threads: tesela_scanner::default_threads(),
        top: 20,
        json: None,
        no_priority: false,
        fallback: false,
        stop_after: None,
        mem: false,
        verbose: false,
    };
    let mut have_root = false;
    let num = |flag: &str, v: Option<String>| -> Result<u64, String> {
        v.ok_or_else(|| format!("{flag} necesita un valor"))?
            .parse()
            .map_err(|_| format!("{flag}: no es un número"))
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--threads" => a.threads = num("--threads", it.next())?.max(1) as usize,
            "--top" => a.top = num("--top", it.next())? as usize,
            "--json" => a.json = Some(it.next().ok_or("--json necesita un archivo")?.into()),
            "--no-priority" => a.no_priority = true,
            "--fallback" => a.fallback = true,
            "--stop-after" => {
                a.stop_after = Some(Duration::from_secs(num("--stop-after", it.next())?))
            }
            "--mem" => a.mem = true,
            "--verbose" => a.verbose = true,
            "-h" | "--help" => return Err(String::new()),
            s if s.starts_with("--") => return Err(format!("opción desconocida: {s}")),
            s => {
                if have_root {
                    return Err("solo se admite una ruta".into());
                }
                a.root = s.into();
                have_root = true;
            }
        }
    }
    if !have_root {
        return Err("falta la ruta a escanear".into());
    }
    Ok(a)
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

fn report(args: &Args, r: &ScanResult, mem: Option<(u64, u64, u64)>) {
    let (tree, s) = (&r.tree, &r.summary);
    let st = tree.stats();
    let root = tree.node(tree.root());
    println!("Ruta: {}", s.root.display());
    println!(
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
        println!(
            "ESCANEO DETENIDO (parar y ver): los directorios marcados incompletos son un mínimo."
        );
    }
    println!(
        "Archivos: {} · directorios: {} · reparse points de directorio (no seguidos): {}",
        st.files, st.dirs, st.reparse_dirs
    );
    println!(
        "Archivos con flag de reparse: {} · de la nube: {} · directorios incompletos: {} · con error: {}",
        st.reparse_files, st.cloud_entries, st.incomplete_dirs, st.error_dirs
    );
    println!(
        "Escaneado en disco: {} · tamaño lógico: {}",
        gib(st.scanned_alloc),
        gib(root.logical)
    );
    println!(
        "Hard links deduplicados: {} entradas en {} grupos · {} evitados en disco ({} lógicos)",
        st.hardlink_extra_entries,
        st.hardlink_groups,
        gib(st.hardlink_alloc_saved),
        gib(st.hardlink_logical_saved)
    );
    match s.volume {
        Some(v) => {
            let used = v.total.saturating_sub(v.free);
            println!(
                "Volumen: {} usados de {} ({} libres)",
                gib(used),
                gib(v.total),
                gib(v.free)
            );
            if s.full_volume && !s.cancelled {
                let pct = if used > 0 {
                    st.unexplained_alloc as f64 * 100.0 / used as f64
                } else {
                    0.0
                };
                println!(
                    "Espacio no explicado (total − libre − escaneado, recortado a ≥ 0): {} ({pct:.2} % de lo usado)",
                    gib(st.unexplained_alloc)
                );
            } else {
                println!(
                    "Espacio no explicado: no aplica (solo en un escaneo terminado de un volumen completo)"
                );
            }
        }
        None => println!("Volumen: no se pudo consultar el espacio"),
    }
    if !s.priority.is_empty() {
        let done = s.priority.iter().filter(|p| p.done).count();
        let bytes: u64 = s.priority.iter().map(|p| p.alloc).sum();
        println!(
            "Rutas prioritarias: {} ({} recorridas enteras, {} vistos)",
            s.priority.len(),
            done,
            gib(bytes)
        );
        if args.verbose {
            for p in &s.priority {
                println!(
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
        println!("Errores: 0");
    } else {
        println!("Errores: {} directorios sin poder listar", s.errors());
        for (code, n) in &s.errors_by_code {
            println!(
                "  código {code}: {n} ({})",
                std::io::Error::from_raw_os_error(*code as i32)
            );
        }
        if args.verbose {
            for (code, p) in &s.error_samples {
                println!("  ejemplo (código {code}): {}", p.display());
            }
        }
    }
    if let Some(e) = s.build_error {
        println!("ERROR al construir el árbol: {e}");
    }
    if let Some((before, peak, steady)) = mem {
        println!(
            "Memoria (private working set): antes {} · pico muestreado durante escaneo y construcción {} · en régimen con el árbol {} ({} nodos, {} del árbol)",
            mib(before),
            mib(peak),
            mib(steady),
            tree.len(),
            mib(tree.heap_bytes() as u64)
        );
    }
    if args.top > 0 {
        println!(
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
            println!(
                "  {:>11}{inc} {pct:>5.1} %  {kind}  {} archivos  {name}",
                gib(n.alloc),
                n.files_below
            );
        }
    }
}

fn write_json(path: &PathBuf, args: &Args, r: &ScanResult) -> std::io::Result<()> {
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
        "volume": s.volume.map(|v| json!({"total": v.total, "free": v.free})),
        "errors_by_code": s.errors_by_code,
        "top": top,
    });
    std::fs::write(
        path,
        serde_json::to_string_pretty(&doc).expect("JSON") + "\n",
    )
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("error: {msg}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
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
            eprintln!("error: {e}");
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
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };
    let mem = sampler.map(|s| {
        let peak = s.finish();
        (before, peak, memory::private_working_set().unwrap_or(0))
    });
    report(&args, &result, mem);
    if let Some(path) = &args.json {
        if let Err(e) = write_json(path, &args, &result) {
            eprintln!("error: no se pudo escribir el JSON: {e}");
            return ExitCode::from(1);
        }
        println!(
            "\nJSON escrito en {} (contiene nombres reales: no lo versiones)",
            path.display()
        );
    }
    ExitCode::SUCCESS
}
