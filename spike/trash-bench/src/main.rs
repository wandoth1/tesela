//! Experimento 3 (PLAN.md §6): Papelera de reciclaje con el crate `trash` 5.2.9.
//!
//! Mide:
//!   * tiempo de mandar N archivos pequeños a la Papelera como CARPETA ENTERA y como ARCHIVOS SUELTOS;
//!   * tiempo de `os_limited::list()` (con la Papelera real del usuario + los elementos de prueba);
//!   * si `os_limited::restore_all` restaura bien SOLO los elementos de prueba (filtrados por ruta);
//!   * tiempo de `purge_all` filtrado a los elementos de prueba.
//!
//! AVISO: `run` mete hasta ~100 000 elementos TRANSITORIOS en la Papelera REAL del usuario durante un tiempo largo
//! (decenas de minutos con `run --n 100000`). Todos llevan la ruta de origen `%TEMP%\tesela-spike-<uuid>` y se purgan al final.
//!
//! SEGURIDAD (no negociable):
//!   * Todo el material vive en `%TEMP%\tesela-spike-<uuid>` (uuid generado por esta ejecución).
//!   * Solo se restaura o purga lo que `is_mine()` identifica: el `original_parent` del elemento debe estar BAJO
//!     `std::env::temp_dir()` y su primer componente tras él debe empezar por el prefijo (en `run`: `tesela-spike-<uuid>`
//!     COMPLETO; en `cleanup`: `tesela-spike-`). El nombre solo cuenta si `original_parent` es exactamente `%TEMP%`.
//!     NUNCA se vacía la Papelera ni se toca nada de otras unidades o rutas.
//!   * Todos los argumentos se validan ANTES de crear o mover nada.
//!   * Una guarda RAII (`RunGuard`) purga SOLO los elementos con el uuid completo de la ejecución si `run` entra en
//!     pánico o termina a medias (no cubre un `kill`/corte de luz: para eso está `cleanup`).
//!   * Al final se compara el conjunto de ids de la Papelera con el de antes de empezar (debe ser idéntico).
//!   * `cleanup` lista lo que haría y NO purga ni borra nada sin `--yes`. Úsalo solo con ninguna otra ejecución viva.
//!
//! Uso:
//!   trash-bench run [--n 100000] [--loose-mode one|batch=1000|par=8] [--skip folder|loose] [--restore-sample 1000] [--json out.json]
//!   trash-bench cleanup [--yes]   recuperación tras un fallo (sin --yes solo lista)
//!   trash-bench count             número de elementos de la Papelera y tiempo de list()

use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;
use trash::TrashItem;
use trash::os_limited::{list, purge_all, restore_all};

const PREFIX: &str = "tesela-spike-";

fn temp_root(uuid: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{PREFIX}{uuid}"))
}

/// Normaliza una ruta para comparar: sin prefijo `\\?\`, minúsculas, `/` como `\`, sin `\` final.
fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\");
    let s = s.strip_prefix("\\\\?\\").unwrap_or(&s).to_lowercase();
    s.trim_end_matches('\\').to_string()
}

/// Carpeta temporal del usuario, normalizada (se prueba también su forma canónica).
fn temp_dirs_norm() -> Vec<String> {
    let t = std::env::temp_dir();
    let mut v = vec![norm(&t)];
    if let Ok(c) = std::fs::canonicalize(&t) {
        let n = norm(&c);
        if !v.contains(&n) {
            v.push(n);
        }
    }
    v
}

/// ¿Este elemento de la Papelera es de una carpeta de prueba con este prefijo?
///  * `parent_hit`: `original_parent` está bajo `%TEMP%` y el PRIMER componente tras `%TEMP%` empieza por `prefix`.
///  * `name_hit`: solo si `original_parent` es exactamente `%TEMP%` (se mandó la propia carpeta de prueba) y el nombre
///    empieza por `prefix`.
/// En `run` el `prefix` incluye el uuid completo (`tesela-spike-<uuid>`); en `cleanup` es `tesela-spike-`.
/// Cualquier elemento cuyo origen esté fuera de `%TEMP%` (otra unidad, el escritorio…) NUNCA es "mío".
fn is_mine(item: &TrashItem, prefix: &str) -> bool {
    let parent = norm(&item.original_parent);
    let prefix_l = prefix.to_lowercase();
    for t in temp_dirs_norm() {
        if parent == t {
            if item.name.to_string_lossy().to_lowercase().starts_with(&prefix_l) {
                return true;
            }
        } else if let Some(rest) = parent.strip_prefix(&format!("{t}\\")) {
            let first = rest.split('\\').next().unwrap_or("");
            if first.starts_with(&prefix_l) {
                return true;
            }
        }
    }
    false
}

/// Guarda RAII: al destruirse (también durante un pánico) purga SOLO los elementos de la Papelera con el uuid completo
/// de esta ejecución y borra la carpeta de prueba. Se desarma (`disarm`) cuando `run` termina bien y ya lo ha hecho.
struct RunGuard {
    prefix: String, // "tesela-spike-<uuid>" completo
    root: PathBuf,
    armed: bool,
}
impl RunGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for RunGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        eprintln!("[guarda] ejecución interrumpida: purgando SOLO los elementos de {} …", self.prefix);
        if let Ok(items) = list() {
            let mine: Vec<TrashItem> = items.into_iter().filter(|i| is_mine(i, &self.prefix)).collect();
            eprintln!("[guarda] {} elementos propios en la Papelera", mine.len());
            if !mine.is_empty() {
                if let Err(e) = purge_all(mine) {
                    eprintln!("[guarda] purge_all falló: {e:?}; usa `trash-bench cleanup --yes`");
                }
            }
        }
        // Solo la carpeta de esta ejecución (nombre con el uuid completo y directamente bajo %TEMP%).
        let ok_name = self.root.file_name().map(|n| n.to_string_lossy() == self.prefix.as_str()).unwrap_or(false);
        if ok_name && self.root.parent().map(|p| norm(p)) == Some(norm(&std::env::temp_dir())) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

/// Argumentos de `run`, ya validados.
struct RunArgs {
    n: usize,
    loose_mode: String,
    skip: Option<String>,
    restore_sample: usize,
    json: Option<String>,
}

fn parse_run_args(a: &[String]) -> Result<RunArgs, String> {
    let mut r = RunArgs { n: 100_000, loose_mode: "one".into(), skip: None, restore_sample: 1000, json: None };
    let mut i = 2;
    while i < a.len() {
        let key = a[i].as_str();
        let val = a.get(i + 1).ok_or_else(|| format!("falta el valor de {key}"))?;
        match key {
            "--n" => r.n = val.parse().map_err(|_| format!("--n inválido: {val}"))?,
            "--restore-sample" => r.restore_sample = val.parse().map_err(|_| format!("--restore-sample inválido: {val}"))?,
            "--loose-mode" => {
                let ok = val == "one"
                    || val.strip_prefix("batch=").and_then(|x| x.parse::<usize>().ok()).map(|x| x > 0).unwrap_or(false)
                    || val.strip_prefix("par=").and_then(|x| x.parse::<usize>().ok()).map(|x| x > 0).unwrap_or(false);
                if !ok {
                    return Err(format!("--loose-mode inválido: {val} (one | batch=N | par=N)"));
                }
                r.loose_mode = val.clone();
            }
            "--skip" => {
                if val != "folder" && val != "loose" {
                    return Err(format!("--skip inválido: {val} (folder | loose)"));
                }
                r.skip = Some(val.clone());
            }
            "--json" => {
                if let Some(parent) = Path::new(val).parent() {
                    if !parent.as_os_str().is_empty() && !parent.exists() {
                        return Err(format!("la carpeta de --json no existe: {}", parent.display()));
                    }
                }
                r.json = Some(val.clone());
            }
            other => return Err(format!("argumento desconocido: {other}")),
        }
        i += 2;
    }
    if r.n == 0 {
        return Err("--n debe ser > 0".into());
    }
    Ok(r)
}

fn timed_list() -> (Vec<TrashItem>, f64) {
    let t = Instant::now();
    let v = list().expect("list() falló");
    (v, t.elapsed().as_secs_f64())
}

/// Crea `n` archivos pequeños repartidos en `dirs` subcarpetas (estructura tipo `node_modules`).
fn populate(root: &Path, n: usize, dirs: usize) -> Vec<PathBuf> {
    let per = n.div_ceil(dirs);
    let mut paths = Vec::with_capacity(n);
    'outer: for d in 0..dirs {
        let dir = root.join(format!("d{d:04}"));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..per {
            if paths.len() == n {
                break 'outer;
            }
            let p = dir.join(format!("f{i:06}.dat"));
            std::fs::write(&p, b"tesela-spike 0123456789").unwrap();
            paths.push(p);
        }
    }
    paths
}

fn count_files(root: &Path) -> usize {
    fn go(p: &Path) -> usize {
        std::fs::read_dir(p)
            .map(|rd| rd.flatten().map(|e| if e.file_type().map(|t| t.is_dir()).unwrap_or(false) { go(&e.path()) } else { 1 }).sum())
            .unwrap_or(0)
    }
    go(root)
}

fn delete_loose(paths: &[PathBuf], mode: &str) -> Result<(), String> {
    if mode == "one" {
        trash::delete_all(paths).map_err(|e| e.to_string())
    } else if let Some(sz) = mode.strip_prefix("batch=") {
        let sz: usize = sz.parse().unwrap();
        for chunk in paths.chunks(sz) {
            trash::delete_all(chunk).map_err(|e| e.to_string())?;
        }
        Ok(())
    } else if let Some(t) = mode.strip_prefix("par=") {
        let t: usize = t.parse().unwrap();
        let chunk = paths.len().div_ceil(t);
        let results: Vec<Result<(), String>> = std::thread::scope(|s| {
            let hs: Vec<_> = paths.chunks(chunk).map(|c| s.spawn(move || trash::delete_all(c).map_err(|e| e.to_string()))).collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        results.into_iter().collect()
    } else {
        panic!("--loose-mode desconocido: {mode}")
    }
}

fn run(n: usize, loose_mode: &str, skip: Option<&str>, restore_sample: usize) -> Value {
    let uuid = uuid::Uuid::new_v4().to_string();
    let root = temp_root(&uuid);
    let my_prefix = format!("{PREFIX}{uuid}");
    std::fs::create_dir_all(&root).unwrap();
    // Guarda RAII: si algo falla o hay pánico a partir de aquí, purga SOLO lo de este uuid.
    let mut guard = RunGuard { prefix: my_prefix.clone(), root: root.clone(), armed: true };
    eprintln!("carpeta de prueba: {}", root.display());
    let mut out = serde_json::Map::new();
    out.insert("n".into(), json!(n));
    out.insert("carpeta_prueba".into(), json!(root.display().to_string()));
    out.insert("loose_mode".into(), json!(loose_mode));

    // 0) Línea base de la Papelera real (solo lectura).
    let (before, t_list0) = timed_list();
    let before_ids: HashSet<String> = before.iter().map(|i| i.id.to_string_lossy().to_string()).collect();
    out.insert("papelera_antes".into(), json!({"elementos": before.len(), "list_s": t_list0}));
    eprintln!("Papelera antes: {} elementos (list() {t_list0:.2}s)", before.len());
    assert!(!before.iter().any(|i| is_mine(i, &my_prefix)), "ya hay elementos con mi uuid: imposible");

    // 1) Carpeta entera
    if skip != Some("folder") {
        let folder = root.join("folder_case");
        let t = Instant::now();
        populate(&folder, n, 100);
        let create_s = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let r = trash::delete(&folder);
        let secs = t.elapsed().as_secs_f64();
        eprintln!("carpeta de {n} archivos -> Papelera: {secs:.3}s ({:?})", r.as_ref().err());
        out.insert(
            "carpeta_entera".into(),
            json!({"archivos": n, "crear_s": create_s, "mandar_a_papelera_s": secs, "ok": r.is_ok(),
                   "error": r.err().map(|e| e.to_string()), "carpeta_ya_no_existe": !folder.exists(),
                   "umbral_plan_s": 10, "supera_umbral": secs > 10.0}),
        );
    }

    // 2) Archivos sueltos
    if skip != Some("loose") {
        let loose = root.join("loose_case");
        let t = Instant::now();
        let paths = populate(&loose, n, 100);
        let create_s = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let r = delete_loose(&paths, loose_mode);
        let secs = t.elapsed().as_secs_f64();
        let remaining = paths.iter().filter(|p| p.exists()).count();
        eprintln!("{n} archivos sueltos -> Papelera ({loose_mode}): {secs:.3}s, restantes en disco={remaining}, err={:?}", r.as_ref().err());
        out.insert(
            "archivos_sueltos".into(),
            json!({"archivos": paths.len(), "modo": loose_mode, "crear_s": create_s, "mandar_a_papelera_s": secs,
                   "ms_por_archivo": secs * 1000.0 / paths.len() as f64, "ok": r.is_ok(),
                   "error": r.err().map(|e| e.to_string()), "restantes_en_disco": remaining,
                   "umbral_plan_s": 10, "supera_umbral": secs > 10.0}),
        );
    }

    // Gancho de prueba de la guarda RAII (no se usa en las mediciones): provoca un pánico con elementos propios ya en la Papelera.
    if std::env::var_os("TRASH_BENCH_TEST_PANIC").is_some() {
        panic!("pánico provocado por TRASH_BENCH_TEST_PANIC para probar RunGuard");
    }
    // 3) list() con los elementos de prueba dentro
    let (during, t_list1) = timed_list();
    let mine: Vec<&TrashItem> = during.iter().filter(|i| is_mine(i, &my_prefix)).collect();
    let others_during = during.len() - mine.len();
    eprintln!("Papelera durante: {} elementos ({} míos, {} ajenos); list() {t_list1:.2}s", during.len(), mine.len(), others_during);
    out.insert(
        "list_con_elementos_de_prueba".into(),
        json!({"elementos_total": during.len(), "mios": mine.len(), "ajenos": others_during, "list_s": t_list1,
               "list_s_por_1000_elementos": t_list1 * 1000.0 / during.len().max(1) as f64}),
    );
    let during_ids_foreign: HashSet<String> = during.iter().filter(|i| !is_mine(i, &my_prefix)).map(|i| i.id.to_string_lossy().to_string()).collect();
    out.insert("ajenos_iguales_a_antes".into(), json!(during_ids_foreign == before_ids));

    // 4) restore_all solo de lo mío: la carpeta entera y una muestra de sueltos.
    let mut restore = serde_json::Map::new();
    let folder_items: Vec<TrashItem> = during
        .iter()
        .filter(|i| is_mine(i, &my_prefix) && i.name.to_string_lossy() == "folder_case")
        .map(|i| TrashItem { id: i.id.clone(), name: i.name.clone(), original_parent: i.original_parent.clone(), time_deleted: i.time_deleted })
        .collect();
    if !folder_items.is_empty() {
        let t = Instant::now();
        let r = restore_all(folder_items);
        let secs = t.elapsed().as_secs_f64();
        let nfiles = count_files(&root.join("folder_case"));
        restore.insert(
            "carpeta".into(),
            json!({"elementos_restaurados": 1, "restore_s": secs, "ok": r.is_ok(), "error": r.err().map(|e| format!("{e:?}")), "archivos_tras_restaurar": nfiles, "esperados": n}),
        );
        eprintln!("restore carpeta: {secs:.3}s, archivos={nfiles}/{n}");
    }
    let sample: Vec<TrashItem> = during
        .iter()
        .filter(|i| is_mine(i, &my_prefix) && i.name.to_string_lossy().starts_with('f') && i.original_parent.starts_with(root.join("loose_case")))
        .take(restore_sample)
        .map(|i| TrashItem { id: i.id.clone(), name: i.name.clone(), original_parent: i.original_parent.clone(), time_deleted: i.time_deleted })
        .collect();
    if !sample.is_empty() {
        let expected: Vec<PathBuf> = sample.iter().map(|i| i.original_parent.join(&i.name)).collect();
        let k = sample.len();
        let t = Instant::now();
        let r = restore_all(sample);
        let secs = t.elapsed().as_secs_f64();
        let present = expected.iter().filter(|p| p.exists()).count();
        restore.insert(
            "sueltos_muestra".into(),
            json!({"elementos_pedidos": k, "restore_s": secs, "ms_por_archivo": secs * 1000.0 / k as f64, "ok": r.is_ok(),
                   "error": r.err().map(|e| format!("{e:?}")), "existen_en_su_ruta_original": present}),
        );
        eprintln!("restore {k} sueltos: {secs:.3}s, existen={present}/{k}");
    }
    // Comprobación: la Papelera tras restaurar tiene exactamente (mis elementos − restaurados) y los ajenos intactos.
    let (after_restore, t_list2) = timed_list();
    let foreign_after: HashSet<String> = after_restore.iter().filter(|i| !is_mine(i, &my_prefix)).map(|i| i.id.to_string_lossy().to_string()).collect();
    let mine_after = after_restore.iter().filter(|i| is_mine(i, &my_prefix)).count();
    restore.insert("papelera_tras_restaurar".into(), json!({"mios_restantes": mine_after, "ajenos_iguales_a_antes": foreign_after == before_ids, "list_s": t_list2}));
    out.insert("restore".into(), Value::Object(restore));

    // 5) purga SOLO de lo mío
    let to_purge: Vec<TrashItem> = after_restore.into_iter().filter(|i| is_mine(i, &my_prefix)).collect();
    let k = to_purge.len();
    let t = Instant::now();
    let r = if k > 0 { purge_all(to_purge) } else { Ok(()) };
    let secs = t.elapsed().as_secs_f64();
    eprintln!("purge_all de {k} elementos míos: {secs:.3}s ({:?})", r.as_ref().err());
    out.insert("purge".into(), json!({"elementos": k, "purge_s": secs, "ok": r.is_ok(), "error": r.err().map(|e| format!("{e:?}")), "ms_por_elemento": if k > 0 { secs * 1000.0 / k as f64 } else { 0.0 }}));

    // 6) Verificación final: la Papelera debe quedar con los mismos ids que antes
    let (fin, t_list3) = timed_list();
    let fin_ids: HashSet<String> = fin.iter().map(|i| i.id.to_string_lossy().to_string()).collect();
    let mine_left = fin.iter().filter(|i| is_mine(i, &my_prefix)).count();
    out.insert(
        "verificacion_final".into(),
        json!({"papelera_identica_a_antes": fin_ids == before_ids, "mis_elementos_restantes": mine_left,
               "elementos_ahora": fin.len(), "elementos_antes": before.len(), "list_s": t_list3,
               "ids_que_faltan_respecto_a_antes": before_ids.difference(&fin_ids).count(),
               "ids_nuevos_respecto_a_antes": fin_ids.difference(&before_ids).count()}),
    );
    // Limpieza de la carpeta temporal (solo la nuestra) y desarme de la guarda: todo salió bien.
    guard.disarm();
    assert!(root.file_name().unwrap().to_string_lossy() == my_prefix.as_str());
    let _ = std::fs::remove_dir_all(&root);
    out.insert("carpeta_prueba_borrada".into(), json!(!root.exists()));
    Value::Object(out)
}

/// Recuperación tras una ejecución muerta. Sin `--yes` solo LISTA lo que haría.
fn cleanup(yes: bool) {
    let tmp = std::env::temp_dir();
    let (items, t) = timed_list();
    let mine: Vec<TrashItem> = items.into_iter().filter(|i| is_mine(i, PREFIX)).collect();
    eprintln!("list() {t:.2}s; {} elementos en la Papelera con origen bajo {} / {PREFIX}*:", mine.len(), tmp.display());
    for i in mine.iter().take(50) {
        eprintln!("  papelera: {} (origen {})", i.name.to_string_lossy(), i.original_parent.display());
    }
    if mine.len() > 50 {
        eprintln!("  … y {} más", mine.len() - 50);
    }
    // Solo carpetas HIJAS DIRECTAS de %TEMP% con el prefijo.
    let mut dirs = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&tmp) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with(PREFIX) && e.path().is_dir() {
                dirs.push(e.path());
            }
        }
    }
    for d in &dirs {
        eprintln!("  carpeta: {}", d.display());
    }
    if !yes {
        eprintln!("Simulación: no se ha purgado ni borrado nada. Repite con --yes (y SIN otra ejecución de trash-bench viva).");
        return;
    }
    if !mine.is_empty() {
        purge_all(mine).expect("purge_all falló");
    }
    for d in dirs {
        eprintln!("borrando {}", d.display());
        let _ = std::fs::remove_dir_all(d);
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(|s| s.as_str()) {
        Some("run") => {
            // Validar TODOS los argumentos antes de crear o mover nada.
            let args = match parse_run_args(&a) {
                Ok(x) => x,
                Err(e) => {
                    eprintln!("error de argumentos: {e}");
                    std::process::exit(2);
                }
            };
            let v = run(args.n, &args.loose_mode, args.skip.as_deref(), args.restore_sample);
            let s = serde_json::to_string_pretty(&v).unwrap();
            println!("{s}");
            if let Some(p) = args.json {
                std::fs::write(p, s).unwrap();
            }
        }
        Some("cleanup") => {
            let rest = &a[2..];
            if rest.iter().any(|x| x != "--yes") {
                eprintln!("argumentos desconocidos para cleanup (solo admite --yes)");
                std::process::exit(2);
            }
            cleanup(rest.iter().any(|x| x == "--yes"));
        }
        Some("count") => {
            let (items, t) = timed_list();
            println!("{} elementos, list() {t:.3}s", items.len());
        }
        _ => {
            eprintln!("uso: trash-bench run|cleanup|count (ver cabecera del código)");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn item(name: &str, parent: PathBuf) -> TrashItem {
        TrashItem { id: OsString::from("x"), name: OsString::from(name), original_parent: parent, time_deleted: 0 }
    }

    #[test]
    fn is_mine_solo_bajo_temp_y_con_prefijo() {
        let t = std::env::temp_dir();
        let p = "tesela-spike-abc";
        assert!(is_mine(&item("f", t.join("tesela-spike-abc").join("d0001")), p));
        assert!(is_mine(&item("tesela-spike-abc", t.clone()), p));
        // otro uuid, otro prefijo, fuera de %TEMP%, o un componente intermedio con el prefijo: NO
        assert!(!is_mine(&item("f", t.join("tesela-spike-xyz")), p));
        assert!(!is_mine(&item("tesela-spike-abc", PathBuf::from(r"D:\ajeno")), p));
        assert!(!is_mine(&item("f", PathBuf::from(r"D:\tesela-spike-abc")), p));
        assert!(!is_mine(&item("f", t.join("otra").join("tesela-spike-abc")), p));
        assert!(!is_mine(&item("tesela-spike-abc", t.join("sub")), p));
        // con prefijo genérico (cleanup)
        assert!(is_mine(&item("f", t.join("tesela-spike-lo-que-sea")), PREFIX));
        assert!(!is_mine(&item("tesela-spike-x", PathBuf::from(r"C:\Users\otro\Desktop")), PREFIX));
    }

    #[test]
    fn args_invalidos_se_rechazan() {
        let v = |x: &[&str]| -> Vec<String> { ["tb", "run"].iter().chain(x.iter()).map(|s| s.to_string()).collect() };
        assert!(parse_run_args(&v(&["--loose-mode", "foo"])).is_err());
        assert!(parse_run_args(&v(&["--loose-mode", "par=0"])).is_err());
        assert!(parse_run_args(&v(&["--n", "0"])).is_err());
        assert!(parse_run_args(&v(&["--skip", "x"])).is_err());
        assert!(parse_run_args(&v(&["--loose-mode", "batch=1000", "--n", "5"])).is_ok());
    }
}
