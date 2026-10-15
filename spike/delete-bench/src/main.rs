//! Experimento 4 (PLAN.md §6, §2.6, §3.7): borrado POSIX con
//! `SetFileInformationByHandle(FileDispositionInfoEx)` frente a `std::fs::remove_file` en paralelo.
//!
//! Todo ocurre DENTRO de `%TEMP%\tesela-spike-<uuid>` (carpeta que crea y borra este mismo programa).
//! No toca nada fuera de ella.
//!
//! Subcomandos:
//!   delete-bench functional          ¿funciona FileDispositionInfoEx? (con/sin POSIX, con/sin IGNORE_READONLY)
//!   delete-bench share-delete        §2.6: archivo abierto por otro handle con/sin FILE_SHARE_DELETE
//!   delete-bench reparse             junction y symlink de directorio: ¿borrar el enlace respeta el destino?
//!   delete-bench speed [--files N] [--runs R] [--threads T] [--dirs D]   velocidad de cada variante
//!   delete-bench all                 los tres anteriores con valores por defecto
//! Todos aceptan `--json ruta`.

use rayon::prelude::*;
use serde_json::{Value, json};
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Instant;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;

fn wide(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// Carpeta de trabajo del test, bajo %TEMP%, con un uuid propio.
struct Sandbox {
    root: PathBuf,
}
impl Sandbox {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("tesela-spike-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        assert!(
            root.file_name().unwrap().to_string_lossy().starts_with("tesela-spike-"),
            "la carpeta de trabajo debe ser tesela-spike-*"
        );
        Sandbox { root }
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        // Solo borra la carpeta que creó este mismo proceso (comprobación de prefijo por seguridad).
        if self.root.file_name().map(|n| n.to_string_lossy().starts_with("tesela-spike-")).unwrap_or(false) {
            clear_readonly_tree(&self.root);
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

fn clear_readonly_tree(p: &Path) {
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            if let Ok(md) = e.metadata() {
                if md.is_dir() {
                    clear_readonly_tree(&e.path());
                } else if md.permissions().readonly() {
                    let mut perm = md.permissions();
                    #[allow(clippy::permissions_set_readonly_false)]
                    perm.set_readonly(false);
                    let _ = std::fs::set_permissions(e.path(), perm);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Primitivas Win32
// ---------------------------------------------------------------------------------------------

const DELETE_FLAG: u32 = FILE_DISPOSITION_FLAG_DELETE;
const POSIX_FLAG: u32 = FILE_DISPOSITION_FLAG_POSIX_SEMANTICS;
const IGNORE_RO_FLAG: u32 = FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE;

/// Abre con acceso DELETE (y FILE_READ_ATTRIBUTES + FILE_WRITE_ATTRIBUTES, necesarios para quitar el
/// atributo de solo lectura que IGNORE_READONLY_ATTRIBUTE salta), sin seguir reparse points.
fn open_for_delete(path: &Path) -> Result<HANDLE, u32> {
    let w = wide(path);
    unsafe {
        let h = CreateFileW(
            w.as_ptr(),
            DELETE | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        );
        if h == INVALID_HANDLE_VALUE { Err(GetLastError()) } else { Ok(h) }
    }
}

fn set_disposition_ex(h: HANDLE, flags: u32) -> Result<(), u32> {
    unsafe {
        let info = FILE_DISPOSITION_INFO_EX { Flags: flags };
        let ok = SetFileInformationByHandle(
            h,
            FileDispositionInfoEx,
            &info as *const _ as *const _,
            std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        );
        if ok == 0 { Err(GetLastError()) } else { Ok(()) }
    }
}

/// Borra con FileDispositionInfoEx; el borrado efectivo ocurre al cerrar el handle (última referencia
/// de nuestro lado; con POSIX el nombre desaparece de inmediato aunque otros handles sigan abiertos).
fn delete_dispex(path: &Path, flags: u32) -> Result<(), u32> {
    let h = open_for_delete(path)?;
    let r = set_disposition_ex(h, flags);
    unsafe { CloseHandle(h) };
    r
}

fn delete_legacy_disposition(path: &Path) -> Result<(), u32> {
    let h = open_for_delete(path)?;
    let r = unsafe {
        let info = FILE_DISPOSITION_INFO { DeleteFile: true };
        if SetFileInformationByHandle(
            h,
            FileDispositionInfo,
            &info as *const _ as *const _,
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        ) == 0
        {
            Err(GetLastError())
        } else {
            Ok(())
        }
    };
    unsafe { CloseHandle(h) };
    r
}

fn delete_file_w(path: &Path) -> Result<(), u32> {
    let w = wide(path);
    unsafe { if DeleteFileW(w.as_ptr()) == 0 { Err(GetLastError()) } else { Ok(()) } }
}

/// Estado del nombre: Ok(true) existe y se puede abrir, Err(código) si falla (2 = no existe,
/// 5 = acceso denegado: típico de "delete pending").
fn probe_name(path: &Path) -> String {
    let w = wide(path);
    unsafe {
        let h = CreateFileW(
            w.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        );
        if h == INVALID_HANDLE_VALUE {
            let e = GetLastError();
            match e {
                ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => format!("el nombre ya no existe (Win32 {e})"),
                ERROR_ACCESS_DENIED => format!("nombre visible pero inaccesible = delete-pending (Win32 {e})"),
                _ => format!("error Win32 {e}"),
            }
        } else {
            CloseHandle(h);
            "el nombre existe y se puede abrir".to_string()
        }
    }
}

fn free_bytes(dir: &Path) -> u64 {
    let root = dir.ancestors().last().unwrap().to_string_lossy().to_string();
    spike_common::disk_free_space(&root).map(|(_, avail, _)| avail).unwrap_or(0)
}

fn err_name(code: u32) -> String {
    match code {
        5 => "ERROR_ACCESS_DENIED (5)".into(),
        32 => "ERROR_SHARING_VIOLATION (32)".into(),
        87 => "ERROR_INVALID_PARAMETER (87)".into(),
        1 => "ERROR_INVALID_FUNCTION (1)".into(),
        c => format!("Win32 {c}"),
    }
}

fn res(r: Result<(), u32>) -> String {
    match r {
        Ok(()) => "OK".into(),
        Err(c) => format!("FALLA: {}", err_name(c)),
    }
}

// ---------------------------------------------------------------------------------------------
// 1) ¿Funciona? flags y solo lectura
// ---------------------------------------------------------------------------------------------

fn make_file(p: &Path, readonly: bool) {
    std::fs::write(p, b"tesela spike").unwrap();
    if readonly {
        let mut perm = std::fs::metadata(p).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(p, perm).unwrap();
    }
}

fn functional() -> Value {
    let sb = Sandbox::new();
    let mut rows = Vec::new();
    let cases: Vec<(&str, u32)> = vec![
        ("DELETE", DELETE_FLAG),
        ("DELETE | POSIX_SEMANTICS", DELETE_FLAG | POSIX_FLAG),
        ("DELETE | POSIX_SEMANTICS | IGNORE_READONLY_ATTRIBUTE", DELETE_FLAG | POSIX_FLAG | IGNORE_RO_FLAG),
        ("DELETE | IGNORE_READONLY_ATTRIBUTE", DELETE_FLAG | IGNORE_RO_FLAG),
    ];
    for readonly in [false, true] {
        for (name, flags) in &cases {
            let p = sb.root.join(format!("f_{}_{}.txt", readonly, flags));
            make_file(&p, readonly);
            let r = delete_dispex(&p, *flags);
            let gone = !p.exists();
            rows.push(json!({
                "solo_lectura": readonly, "flags": name, "SetFileInformationByHandle": res(r),
                "archivo_desaparecio": gone
            }));
            if !gone {
                clear_readonly_tree(&sb.root);
            }
        }
        // Referencias: std::fs::remove_file y DeleteFileW
        let p = sb.root.join(format!("std_{readonly}.txt"));
        make_file(&p, readonly);
        let r = std::fs::remove_file(&p);
        rows.push(json!({"solo_lectura": readonly, "metodo": "std::fs::remove_file",
            "resultado": match &r { Ok(()) => "OK".to_string(), Err(e) => format!("FALLA: {e}") }, "archivo_desaparecio": !p.exists()}));
        clear_readonly_tree(&sb.root);
        let p = sb.root.join(format!("dfw_{readonly}.txt"));
        make_file(&p, readonly);
        let r = delete_file_w(&p);
        rows.push(json!({"solo_lectura": readonly, "metodo": "DeleteFileW", "resultado": res(r), "archivo_desaparecio": !p.exists()}));
        clear_readonly_tree(&sb.root);
    }
    // Directorios: borrar un directorio vacío con DispositionEx POSIX
    let d = sb.root.join("dir_vacio");
    std::fs::create_dir(&d).unwrap();
    let r = delete_dispex(&d, DELETE_FLAG | POSIX_FLAG);
    rows.push(json!({"objeto": "directorio vacío", "flags": "DELETE|POSIX", "resultado": res(r), "desaparecio": !d.exists()}));
    json!({"casos": rows})
}

// ---------------------------------------------------------------------------------------------
// 2) §2.6: archivo abierto por otro handle
// ---------------------------------------------------------------------------------------------

fn open_holder(path: &Path, share_delete: bool) -> HANDLE {
    let w = wide(path);
    let mut share = FILE_SHARE_READ | FILE_SHARE_WRITE;
    if share_delete {
        share |= FILE_SHARE_DELETE;
    }
    unsafe {
        let h = CreateFileW(w.as_ptr(), GENERIC_READ | GENERIC_WRITE, share, std::ptr::null(), OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, std::ptr::null_mut());
        assert!(h != INVALID_HANDLE_VALUE, "no se pudo abrir el holder: {}", GetLastError());
        h
    }
}

fn read_from_handle(h: HANDLE) -> String {
    unsafe {
        let mut buf = [0u8; 16];
        let mut n = 0u32;
        SetFilePointer(h, 0, std::ptr::null_mut(), FILE_BEGIN);
        let ok = ReadFile(h, buf.as_mut_ptr(), 16, &mut n, std::ptr::null_mut());
        if ok != 0 { format!("el handle sigue leyendo ({n} bytes)") } else { format!("lectura falla: Win32 {}", GetLastError()) }
    }
}

fn standard_info(h: HANDLE) -> Value {
    unsafe {
        let mut si: FILE_STANDARD_INFO = std::mem::zeroed();
        let ok = GetFileInformationByHandleEx(h, FileStandardInfo, &mut si as *mut _ as *mut _, std::mem::size_of::<FILE_STANDARD_INFO>() as u32);
        if ok == 0 {
            return json!({"error": GetLastError()});
        }
        json!({"alloc_size": si.AllocationSize, "delete_pending": si.DeletePending, "links": si.NumberOfLinks})
    }
}

/// Escribe `mib` MiB reales (no dispersos) y fuerza el volcado.
fn write_big(path: &Path, mib: usize) {
    let mut f = std::fs::File::create(path).unwrap();
    let mut chunk = vec![0u8; 1 << 20];
    for i in 0..mib {
        for (k, b) in chunk.iter_mut().enumerate().step_by(4096) {
            *b = (i + k) as u8 | 1; // contenido no nulo
        }
        f.write_all(&chunk).unwrap();
    }
    f.sync_all().unwrap();
}

fn share_delete_case(sb: &Sandbox, tag: &str, holder_share_delete: bool, method: &str, mib: usize) -> Value {
    let p = sb.root.join(format!("held_{tag}.bin"));
    let f0 = free_bytes(&sb.root);
    write_big(&p, mib);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let f1 = free_bytes(&sb.root);
    let h1 = open_holder(&p, holder_share_delete);
    let info_before = standard_info(h1);
    let r = match method {
        "dispex_posix" => delete_dispex(&p, DELETE_FLAG | POSIX_FLAG | IGNORE_RO_FLAG),
        "dispex_sin_posix" => delete_dispex(&p, DELETE_FLAG),
        "DeleteFileW" => delete_file_w(&p),
        "std_remove_file" => std::fs::remove_file(&p).map_err(|e| e.raw_os_error().unwrap_or(0) as u32),
        _ => unreachable!(),
    };
    std::thread::sleep(std::time::Duration::from_millis(300));
    let name_after = probe_name(&p);
    let f2 = free_bytes(&sb.root);
    let read_after = read_from_handle(h1);
    let info_after = standard_info(h1);
    // ¿Se puede crear YA un archivo nuevo con el mismo nombre? (con POSIX sí; con delete-pending no)
    let recreate = match std::fs::File::create_new(&p) {
        Ok(_) => {
            let _ = std::fs::remove_file(&p);
            "se puede crear un archivo nuevo con ese nombre".to_string()
        }
        Err(e) => format!("no se puede recrear: {e}"),
    };
    unsafe { CloseHandle(h1) };
    std::thread::sleep(std::time::Duration::from_millis(500));
    let f3 = free_bytes(&sb.root);
    let name_final = probe_name(&p);
    let mb = |x: u64| x as f64 / 1048576.0;
    json!({
        "caso": tag,
        "holder_con_FILE_SHARE_DELETE": holder_share_delete,
        "metodo_de_borrado": method,
        "tamano_archivo_mib": mib,
        "resultado_borrado": res(r),
        "nombre_tras_borrar_con_holder_abierto": name_after,
        "recrear_mismo_nombre_con_holder_abierto": recreate,
        "lectura_via_holder_tras_borrar": read_after,
        "standard_info_holder_antes": info_before,
        "standard_info_holder_despues": info_after,
        "libre_mib_delta_tras_crear(esperado -tamano)": mb(f1) - mb(f0),
        "libre_mib_delta_tras_borrar_holder_abierto(vs tras crear)": mb(f2) - mb(f1),
        "libre_mib_delta_tras_cerrar_holder(vs tras crear)": mb(f3) - mb(f1),
        "nombre_tras_cerrar_holder": name_final,
        "nota": "el espacio libre del volumen es global: otros procesos pueden mover unos MiB de ruido",
    })
}

fn share_delete() -> Value {
    let sb = Sandbox::new();
    let mib = 512;
    let mut rows = Vec::new();
    let mut i = 0;
    for holder_sd in [true, false] {
        for method in ["dispex_posix", "dispex_sin_posix", "DeleteFileW", "std_remove_file"] {
            i += 1;
            rows.push(share_delete_case(&sb, &format!("c{i}"), holder_sd, method, mib));
        }
    }
    json!({"casos": rows})
}

// ---------------------------------------------------------------------------------------------
// 4) Reparse points: ¿borrar el ENLACE respeta el destino? (junction y symlink de directorio)
// ---------------------------------------------------------------------------------------------

fn count_tree_files(p: &Path) -> usize {
    std::fs::read_dir(p)
        .map(|rd| rd.flatten().map(|e| if e.file_type().map(|t| t.is_dir()).unwrap_or(false) { count_tree_files(&e.path()) } else { 1 }).sum())
        .unwrap_or(0)
}

/// Crea un destino con archivos (4 en total, uno en un subdirectorio) dentro del sandbox.
fn make_target(dir: &Path) {
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    for f in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(dir.join(f), b"contenido protegido").unwrap();
    }
    std::fs::write(dir.join("sub").join("d.txt"), b"contenido protegido").unwrap();
}

/// kind: "junction" (mklink /J, no necesita privilegios) o "symlink" (CreateSymbolicLink de directorio).
fn make_link(kind: &str, link: &Path, target: &Path) -> Result<(), String> {
    match kind {
        "junction" => {
            let out = std::process::Command::new("cmd").arg("/C").arg("mklink").arg("/J").arg(link).arg(target).output().map_err(|e| e.to_string())?;
            if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stdout).trim().to_string() + &String::from_utf8_lossy(&out.stderr)) }
        }
        _ => std::os::windows::fs::symlink_dir(target, link).map_err(|e| e.to_string()),
    }
}

fn reparse() -> Value {
    let sb = Sandbox::new();
    let mut rows = Vec::new();
    let methods: Vec<(&str, fn(&Path) -> Result<(), String>)> = vec![
        ("std::fs::remove_file", |p| std::fs::remove_file(p).map_err(|e| e.to_string())),
        ("std::fs::remove_dir", |p| std::fs::remove_dir(p).map_err(|e| e.to_string())),
        ("std::fs::remove_dir_all", |p| std::fs::remove_dir_all(p).map_err(|e| e.to_string())),
        ("FileDispositionInfoEx DELETE|POSIX|IGNORE_READONLY (abre con FILE_FLAG_OPEN_REPARSE_POINT)", |p| delete_dispex(p, DELETE_FLAG | POSIX_FLAG | IGNORE_RO_FLAG).map_err(err_name)),
        ("DeleteFileW", |p| delete_file_w(p).map_err(err_name)),
    ];
    for kind in ["junction", "symlink"] {
        for (i, (mname, f)) in methods.iter().enumerate() {
            let target = sb.root.join(format!("destino_{kind}_{i}"));
            let link = sb.root.join(format!("enlace_{kind}_{i}"));
            make_target(&target);
            let before = count_tree_files(&target);
            if let Err(e) = make_link(kind, &link, &target) {
                rows.push(json!({"tipo": kind, "metodo": mname, "no_se_pudo_crear_el_enlace": e}));
                continue;
            }
            let via_link_before = count_tree_files(&link);
            let r = f(&link);
            let link_exists = std::fs::symlink_metadata(&link).is_ok();
            let after = count_tree_files(&target);
            rows.push(json!({
                "tipo": kind, "metodo": mname,
                "archivos_del_destino_antes": before, "archivos_visibles_a_traves_del_enlace": via_link_before,
                "resultado": match &r { Ok(()) => "OK".to_string(), Err(e) => format!("FALLA: {e}") },
                "el_enlace_sigue_existiendo": link_exists,
                "archivos_del_destino_despues": after,
                "destino_intacto": after == before,
            }));
            // limpieza del enlace si sigue (sin seguirlo) para que el sandbox se pueda borrar
            if link_exists {
                let _ = std::fs::remove_dir(&link).or_else(|_| std::fs::remove_file(&link));
            }
        }
    }
    json!({"casos": rows, "nota": "junction = mklink /J (sin privilegios); symlink de directorio requiere Modo desarrollador o privilegio SeCreateSymbolicLink: si no se pudo crear, la fila lo dice"})
}

// ---------------------------------------------------------------------------------------------
// 3) Velocidad
// ---------------------------------------------------------------------------------------------

fn populate(root: &Path, dirs: usize, files: usize) -> Vec<PathBuf> {
    let per = files / dirs;
    for d in 0..dirs {
        std::fs::create_dir_all(root.join(format!("d{d:04}"))).unwrap();
    }
    let paths: Vec<PathBuf> = (0..dirs).flat_map(|d| (0..per).map(move |i| (d, i))).map(|(d, i)| root.join(format!("d{d:04}")).join(format!("f{i:06}.dat"))).collect();
    paths.par_iter().for_each(|p| {
        std::fs::write(p, b"0123456789abcdef").unwrap();
    });
    paths
}

fn speed(files: usize, runs: usize, threads: usize, dirs: usize) -> Value {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
    type Deleter = fn(&Path) -> Result<(), u32>;
    let variants: Vec<(&str, Deleter)> = vec![
        ("std::fs::remove_file", |p| std::fs::remove_file(p).map_err(|e| e.raw_os_error().unwrap_or(0) as u32)),
        ("DeleteFileW", delete_file_w),
        ("FileDispositionInfo (legacy, por handle)", delete_legacy_disposition),
        ("FileDispositionInfoEx DELETE|POSIX|IGNORE_READONLY", |p| delete_dispex(p, DELETE_FLAG | POSIX_FLAG | IGNORE_RO_FLAG)),
        ("FileDispositionInfoEx DELETE (sin POSIX)", |p| delete_dispex(p, DELETE_FLAG)),
    ];
    let mut results: Vec<(String, Vec<f64>, usize)> = variants.iter().map(|(n, _)| (n.to_string(), vec![], 0)).collect();
    for run in 1..=runs {
        for (vi, (name, f)) in variants.iter().enumerate() {
            let sb = Sandbox::new();
            let t_create = Instant::now();
            let paths = pool.install(|| populate(&sb.root, dirs, files));
            let create_s = t_create.elapsed().as_secs_f64();
            let t = Instant::now();
            let fails: usize = pool.install(|| paths.par_iter().map(|p| f(p).is_err() as usize).sum());
            let secs = t.elapsed().as_secs_f64();
            // Verificación: no debe quedar ningún archivo
            let left: usize = std::fs::read_dir(&sb.root).unwrap().flatten().map(|d| std::fs::read_dir(d.path()).map(|r| r.count()).unwrap_or(0)).sum();
            eprintln!("[run {run}] {name}: {} archivos en {secs:.3}s ({:.0} arch/s), fallos={fails}, restantes={left} (creación {create_s:.1}s)", paths.len(), paths.len() as f64 / secs);
            results[vi].1.push(secs);
            results[vi].2 += fails + left;
        }
    }
    let rows: Vec<Value> = results
        .iter()
        .map(|(n, v, bad)| {
            let mean = v.iter().sum::<f64>() / v.len() as f64;
            let min = v.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = v.iter().cloned().fold(0.0, f64::max);
            json!({"variante": n, "segundos_por_ejecucion": v, "media_s": mean, "min_s": min, "max_s": max,
                   "archivos_por_s_media": files as f64 / mean, "fallos_o_restos_totales": bad})
        })
        .collect();
    json!({"archivos": files, "directorios": dirs, "hilos": threads, "ejecuciones": runs, "variantes": rows})
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let num = |k: &str, d: usize| get(k).and_then(|s| s.parse().ok()).unwrap_or(d);
    let cmd = a.get(1).map(|s| s.as_str()).unwrap_or("all");
    let out = match cmd {
        "functional" => json!({"functional": functional()}),
        "share-delete" => json!({"share_delete": share_delete()}),
        "reparse" => json!({"reparse": reparse()}),
        "speed" => json!({"speed": speed(num("--files", 50_000), num("--runs", 3), num("--threads", 16), num("--dirs", 100))}),
        "all" => json!({"functional": functional(), "share_delete": share_delete(), "reparse": reparse(), "speed": speed(num("--files", 50_000), num("--runs", 3), num("--threads", 16), num("--dirs", 100))}),
        _ => {
            eprintln!("subcomando desconocido: {cmd}");
            std::process::exit(2);
        }
    };
    let s = serde_json::to_string_pretty(&out).unwrap();
    println!("{s}");
    if let Some(p) = get("--json") {
        std::fs::write(p, s).unwrap();
    }
}
