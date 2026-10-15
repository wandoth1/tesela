//! Experimento 2 (PLAN.md §6): enumeración paralela de un volumen, SOLO LECTURA.
//!
//! Compara tres formas de enumerar:
//!   * `extd`       : `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)`. Da EndOfFile,
//!                    AllocationSize, FileId de 128 bits y la etiqueta de reparse POR LOTES.
//!   * `find`       : `FindFirstFileExW(FindExInfoBasic, FIND_FIRST_EX_LARGE_FETCH)`. Solo da
//!                    EndOfFile y atributos: NO da AllocationSize ni FileId.
//!   * `find-alloc` : `find` + por cada archivo `CreateFileW(FILE_READ_ATTRIBUTES)` y
//!                    `GetFileInformationByHandleEx(FileStandardInfo + FileIdInfo)`. Es lo mínimo
//!                    para obtener los mismos datos que `extd` partiendo de `find` (comparación justa).
//!   * `std`        : referencia mono-hilo con `std::fs` (solo valida recuentos de archivos,
//!                    directorios y EndOfFile en árboles pequeños; no es parte de la comparación de tiempo).
//!
//! Paralelismo: work-stealing con `crossbeam-deque` (un `Injector` + un `Worker` por hilo), una tarea
//! por directorio, igual que PLAN.md §3.3. Los reparse points de directorio NO se siguen.
//! Los reparse points de ARCHIVO (OneDrive, WOF/Compact OS, dedup…) se cuentan con su tamaño normal
//! (no se sigue nada) y se tabulan por etiqueta.
//!
//! Uso:
//!   enum-bench --api extd|find|find-alloc|std [--root C:\] [--threads N] [--runs R] [--json out.json]
//!   enum-bench --probe [--root C:\]      (sondeo de $MFT y FSCTL_GET_NTFS_VOLUME_DATA)
//!
//! NUNCA escribe nada en disco salvo el JSON de salida que se le pida con `--json`.

use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER};

const FILE_ID_INFO_CLASS: FILE_INFO_BY_HANDLE_CLASS = FileIdInfo;
const BUF_BYTES: usize = 64 * 1024;

// ---------------------------------------------------------------------------------------------
// Tipos
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Api {
    Extd,
    Find,
    FindAlloc,
    Std,
}

impl Api {
    fn name(self) -> &'static str {
        match self {
            Api::Extd => "extd",
            Api::Find => "find",
            Api::FindAlloc => "find-alloc",
            Api::Std => "std",
        }
    }
}

/// Un directorio por procesar. `tag` = índice de la carpeta de primer nivel a la que pertenece
/// (0 = archivos sueltos en la raíz; i+1 = i-ésima carpeta de primer nivel).
struct Task {
    path: Vec<u16>, // con prefijo \\?\ y SIN NUL final
    tag: u16,
}

/// Entrada ya normalizada, independiente de la API usada.
struct Entry<'a> {
    name: &'a [u16],
    attrs: u32,
    eof: u64,
    alloc: Option<u64>, // None si la API no lo da
    file_id: Option<u128>,
    reparse_tag: u32,
}

/// Registro por archivo para deduplicar hard links después (solo si hay FileId).
#[derive(Clone, Copy)]
struct Rec {
    id: u128,
    alloc: u64,
    eof: u64,
    tag: u16,
}

#[derive(Default, Clone, Copy)]
struct TagTotals {
    files: u64,
    alloc: u64,
    eof: u64,
}

/// Acumuladores por hilo (sin compartir nada → sin contención).
#[derive(Default)]
struct Acc {
    files: u64,
    dirs: u64,
    alloc_sum: u64, // suma de AllocationSize de archivos (solo APIs que lo dan)
    eof_sum: u64,   // suma de EndOfFile de archivos
    est_alloc_4k: u64, // estimación sin AllocationSize: EndOfFile redondeado a clusters de 4096 (para el respaldo FindFirstFileExW)
    resident_files: u64, // archivos con EndOfFile > 0 y AllocationSize = 0 (datos residentes en la MFT)
    dir_alloc_sum: u64,
    reparse_files: u64,
    reparse_dirs: u64,
    reparse_file_bytes_alloc: u64,
    reparse_tags: BTreeMap<u32, (u64, u64)>, // tag -> (n, alloc o eof si no hay alloc)
    errors: BTreeMap<u32, u64>,              // código Win32 -> n (apertura/enumeración de directorios)
    file_query_errors: BTreeMap<u32, u64>,   // find-alloc: errores al consultar un archivo concreto
    error_samples: Vec<(u32, String)>,
    per_tag: Vec<TagTotals>,
    recs: Vec<Rec>,
    root_entries: Vec<(String, u32, u64, Option<u64>, Option<u128>)>,
    top_names: Vec<String>,
    dirs_visited: u64,
    handles_opened: u64,
}

impl Acc {
    fn tag_mut(&mut self, tag: u16) -> &mut TagTotals {
        let i = tag as usize;
        if self.per_tag.len() <= i {
            self.per_tag.resize(i + 1, TagTotals::default());
        }
        &mut self.per_tag[i]
    }
    fn err(&mut self, code: u32, path: &[u16]) {
        *self.errors.entry(code).or_default() += 1;
        if self.error_samples.len() < 12 {
            self.error_samples.push((code, from_wide(path)));
        }
    }
}

fn from_wide(w: &[u16]) -> String {
    OsString::from_wide(w).to_string_lossy().into_owned()
}

fn wide_nul(w: &[u16]) -> Vec<u16> {
    let mut v = Vec::with_capacity(w.len() + 1);
    v.extend_from_slice(w);
    v.push(0);
    v
}

fn join(parent: &[u16], name: &[u16]) -> Vec<u16> {
    let mut v = Vec::with_capacity(parent.len() + name.len() + 1);
    v.extend_from_slice(parent);
    if v.last() != Some(&(b'\\' as u16)) {
        v.push(b'\\' as u16);
    }
    v.extend_from_slice(name);
    v
}

fn is_dot(name: &[u16]) -> bool {
    name == [b'.' as u16] || name == [b'.' as u16, b'.' as u16]
}

// ---------------------------------------------------------------------------------------------
// Enumeración de UN directorio con cada API. Llaman a `emit` por cada entrada (sin . ni ..).
// Devuelven Err(código Win32) si no se pudo abrir/enumerar.
// ---------------------------------------------------------------------------------------------

fn enum_dir_extd(path: &[u16], buf: &mut [u64], emit: &mut dyn FnMut(&Entry)) -> Result<(), u32> {
    let p = wide_nul(path);
    unsafe {
        let h = CreateFileW(
            p.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        );
        if h == INVALID_HANDLE_VALUE {
            return Err(GetLastError());
        }
        let result = loop {
            let ok = GetFileInformationByHandleEx(
                h,
                FileIdExtdDirectoryInfo,
                buf.as_mut_ptr() as *mut _,
                (buf.len() * 8) as u32,
            );
            if ok == 0 {
                let e = GetLastError();
                break if e == ERROR_NO_MORE_FILES { Ok(()) } else { Err(e) };
            }
            let base = buf.as_ptr() as *const u8;
            let mut off = 0usize;
            loop {
                let info = &*(base.add(off) as *const FILE_ID_EXTD_DIR_INFO);
                let name_ptr = (base.add(off) as *const u8)
                    .add(std::mem::offset_of!(FILE_ID_EXTD_DIR_INFO, FileName))
                    as *const u16;
                let name = std::slice::from_raw_parts(name_ptr, info.FileNameLength as usize / 2);
                if !is_dot(name) {
                    let id = u128::from_le_bytes(info.FileId.Identifier);
                    emit(&Entry {
                        name,
                        attrs: info.FileAttributes,
                        eof: info.EndOfFile.max(0) as u64,
                        alloc: Some(info.AllocationSize.max(0) as u64),
                        file_id: Some(id),
                        reparse_tag: info.ReparsePointTag,
                    });
                }
                if info.NextEntryOffset == 0 {
                    break;
                }
                off += info.NextEntryOffset as usize;
            }
        };
        CloseHandle(h);
        result
    }
}

fn enum_dir_find(path: &[u16], emit: &mut dyn FnMut(&Entry)) -> Result<(), u32> {
    let mut pat = Vec::with_capacity(path.len() + 3);
    pat.extend_from_slice(path);
    if pat.last() != Some(&(b'\\' as u16)) {
        pat.push(b'\\' as u16);
    }
    pat.push(b'*' as u16);
    pat.push(0);
    unsafe {
        let mut fd: WIN32_FIND_DATAW = std::mem::zeroed();
        let h = FindFirstFileExW(
            pat.as_ptr(),
            FindExInfoBasic,
            &mut fd as *mut _ as *mut _,
            FindExSearchNameMatch,
            std::ptr::null(),
            FIND_FIRST_EX_LARGE_FETCH,
        );
        if h == INVALID_HANDLE_VALUE {
            let e = GetLastError();
            // Directorio vacío "visto" sin entradas no debería ocurrir (siempre hay . y ..).
            return Err(e);
        }
        loop {
            let len = fd.cFileName.iter().position(|&c| c == 0).unwrap_or(fd.cFileName.len());
            let name = &fd.cFileName[..len];
            if !is_dot(name) {
                let eof = ((fd.nFileSizeHigh as u64) << 32) | fd.nFileSizeLow as u64;
                emit(&Entry {
                    name,
                    attrs: fd.dwFileAttributes,
                    eof,
                    alloc: None,
                    file_id: None,
                    // dwReserved0 contiene la etiqueta de reparse solo si hay atributo REPARSE_POINT.
                    reparse_tag: if fd.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                        fd.dwReserved0
                    } else {
                        0
                    },
                });
            }
            if FindNextFileW(h, &mut fd) == 0 {
                let e = GetLastError();
                FindClose(h);
                return if e == ERROR_NO_MORE_FILES { Ok(()) } else { Err(e) };
            }
        }
    }
}

/// Para `find-alloc`: abre el archivo con FILE_READ_ATTRIBUTES y pide AllocationSize + FileId.
fn query_file(path: &[u16]) -> Result<(u64, u128), u32> {
    let p = wide_nul(path);
    unsafe {
        let h = CreateFileW(
            p.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        );
        if h == INVALID_HANDLE_VALUE {
            return Err(GetLastError());
        }
        let mut std_info: FILE_STANDARD_INFO = std::mem::zeroed();
        let ok1 = GetFileInformationByHandleEx(
            h,
            FileStandardInfo,
            &mut std_info as *mut _ as *mut _,
            std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
        );
        let e1 = GetLastError();
        let mut id_info: FILE_ID_INFO = std::mem::zeroed();
        let ok2 = GetFileInformationByHandleEx(
            h,
            FILE_ID_INFO_CLASS,
            &mut id_info as *mut _ as *mut _,
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        );
        let e2 = GetLastError();
        CloseHandle(h);
        if ok1 == 0 {
            return Err(e1);
        }
        if ok2 == 0 {
            return Err(e2);
        }
        Ok((
            std_info.AllocationSize.max(0) as u64,
            u128::from_le_bytes(id_info.FileId.Identifier),
        ))
    }
}

// ---------------------------------------------------------------------------------------------
// Procesado de un directorio: clasifica entradas, acumula, devuelve subdirectorios.
// ---------------------------------------------------------------------------------------------

fn process_dir(
    api: Api,
    task: &Task,
    is_root: bool,
    buf: &mut [u64],
    acc: &mut Acc,
    children: &mut Vec<Task>,
) {
    acc.dirs_visited += 1;
    // La enumeración puede tardar; recogemos entradas en el callback y tratamos después los archivos
    // que requieren consulta adicional (find-alloc) fuera del callback.
    let mut pending_files: Vec<(Vec<u16>, u32, u64, u32)> = Vec::new(); // (ruta, attrs, eof, reparse_tag) para find-alloc
    let tag = task.tag;
    let mut cb = |e: &Entry| {
        let is_dir = e.attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
        let is_reparse = e.attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0;
        if is_root {
            acc.root_entries.push((from_wide(e.name), e.attrs, e.eof, e.alloc, e.file_id));
        }
        if is_dir {
            if is_reparse {
                // Junction / symlink / punto de montaje de directorio: se cuenta y NO se sigue.
                acc.reparse_dirs += 1;
                let t = acc.reparse_tags.entry(e.reparse_tag).or_default();
                t.0 += 1;
                return;
            }
            acc.dirs += 1;
            if let Some(a) = e.alloc {
                acc.dir_alloc_sum += a;
            }
            let child_tag = if is_root {
                acc.top_names.push(from_wide(e.name));
                acc.top_names.len() as u16
            } else {
                tag
            };
            children.push(Task { path: join(&task.path, e.name), tag: child_tag });
            return;
        }
        // Archivo (incluye reparse points de archivo: OneDrive, WOF, dedup…).
        if api == Api::FindAlloc {
            pending_files.push((join(&task.path, e.name), e.attrs, e.eof, e.reparse_tag));
            return;
        }
        let alloc = e.alloc;
        record_file(acc, tag, e.eof, alloc, e.file_id, is_reparse, e.reparse_tag);
    };

    let res = match api {
        Api::Extd => enum_dir_extd(&task.path, buf, &mut cb),
        Api::Find | Api::FindAlloc => enum_dir_find(&task.path, &mut cb),
        Api::Std => unreachable!(),
    };
    drop(cb);
    if let Err(code) = res {
        acc.err(code, &task.path);
    }
    for (p, attrs, eof, rtag) in pending_files {
        acc.handles_opened += 1;
        match query_file(&p) {
            Ok((alloc, id)) => {
                record_file(acc, tag, eof, Some(alloc), Some(id), attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0, rtag)
            }
            Err(code) => {
                *acc.file_query_errors.entry(code).or_default() += 1;
                // Sin alloc ni id: cuenta como archivo con solo EndOfFile.
                record_file(acc, tag, eof, None, None, attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0, rtag);
            }
        }
    }
}

fn record_file(
    acc: &mut Acc,
    tag: u16,
    eof: u64,
    alloc: Option<u64>,
    id: Option<u128>,
    is_reparse: bool,
    reparse_tag: u32,
) {
    acc.files += 1;
    acc.eof_sum += eof;
    let a = alloc.unwrap_or(0);
    acc.alloc_sum += a;
    if alloc.is_some() {
        acc.est_alloc_4k += eof.div_ceil(4096) * 4096;
        if a == 0 && eof > 0 {
            acc.resident_files += 1;
        }
    }
    let t = acc.tag_mut(tag);
    t.files += 1;
    t.alloc += a;
    t.eof += eof;
    if is_reparse {
        acc.reparse_files += 1;
        acc.reparse_file_bytes_alloc += a;
        let t = acc.reparse_tags.entry(reparse_tag).or_default();
        t.0 += 1;
        t.1 += if alloc.is_some() { a } else { eof };
    }
    if let Some(id) = id {
        acc.recs.push(Rec { id, alloc: a, eof, tag });
    }
}

// ---------------------------------------------------------------------------------------------
// Escaneo paralelo con work-stealing
// ---------------------------------------------------------------------------------------------

fn scan_parallel(api: Api, root_wide: &[u16], threads: usize) -> (Acc, f64) {
    let t0 = Instant::now();
    // 1) La raíz se procesa en el hilo principal (asigna las etiquetas de primer nivel).
    let mut root_acc = Acc::default();
    let mut buf = vec![0u64; BUF_BYTES / 8];
    let mut first_children = Vec::new();
    process_dir(
        api,
        &Task { path: root_wide.to_vec(), tag: 0 },
        true,
        &mut buf,
        &mut root_acc,
        &mut first_children,
    );

    // 2) El resto, con work-stealing: una tarea por directorio.
    let injector: Injector<Task> = Injector::new();
    let pending = AtomicUsize::new(first_children.len());
    for c in first_children {
        injector.push(c);
    }
    let workers: Vec<Worker<Task>> = (0..threads).map(|_| Worker::new_lifo()).collect();
    let stealers: Vec<Stealer<Task>> = workers.iter().map(|w| w.stealer()).collect();

    let mut accs: Vec<Acc> = std::thread::scope(|s| {
        let handles: Vec<_> = workers
            .into_iter()
            .enumerate()
            .map(|(i, local)| {
                let injector = &injector;
                let stealers = &stealers;
                let pending = &pending;
                s.spawn(move || {
                    let mut acc = Acc::default();
                    let mut buf = vec![0u64; BUF_BYTES / 8];
                    let mut children: Vec<Task> = Vec::new();
                    let mut idle_spins = 0u32;
                    loop {
                        let task = local.pop().or_else(|| {
                            std::iter::repeat_with(|| {
                                injector.steal_batch_and_pop(&local).or_else(|| {
                                    stealers
                                        .iter()
                                        .enumerate()
                                        .filter(|(j, _)| *j != i)
                                        .map(|(_, st)| st.steal())
                                        .collect::<Steal<Task>>()
                                })
                            })
                            .find(|s| !s.is_retry())
                            .and_then(|s| s.success())
                        });
                        match task {
                            Some(t) => {
                                idle_spins = 0;
                                children.clear();
                                process_dir(api, &t, false, &mut buf, &mut acc, &mut children);
                                // Incrementar ANTES de publicar hijos y decrementar después: así
                                // `pending == 0` solo ocurre cuando de verdad no queda trabajo.
                                pending.fetch_add(children.len(), Ordering::SeqCst);
                                for c in children.drain(..) {
                                    local.push(c);
                                }
                                pending.fetch_sub(1, Ordering::SeqCst);
                            }
                            None => {
                                if pending.load(Ordering::SeqCst) == 0 {
                                    break;
                                }
                                idle_spins += 1;
                                if idle_spins < 64 {
                                    std::hint::spin_loop();
                                } else {
                                    std::thread::yield_now();
                                }
                            }
                        }
                    }
                    acc
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    accs.push(root_acc);
    let merged = merge(accs);
    (merged, t0.elapsed().as_secs_f64())
}

fn merge(accs: Vec<Acc>) -> Acc {
    let mut m = Acc::default();
    for a in accs {
        m.files += a.files;
        m.dirs += a.dirs;
        m.alloc_sum += a.alloc_sum;
        m.eof_sum += a.eof_sum;
        m.est_alloc_4k += a.est_alloc_4k;
        m.resident_files += a.resident_files;
        m.dir_alloc_sum += a.dir_alloc_sum;
        m.reparse_files += a.reparse_files;
        m.reparse_dirs += a.reparse_dirs;
        m.reparse_file_bytes_alloc += a.reparse_file_bytes_alloc;
        m.dirs_visited += a.dirs_visited;
        m.handles_opened += a.handles_opened;
        for (k, v) in a.reparse_tags {
            let e = m.reparse_tags.entry(k).or_default();
            e.0 += v.0;
            e.1 += v.1;
        }
        for (k, v) in a.errors {
            *m.errors.entry(k).or_default() += v;
        }
        for (k, v) in a.file_query_errors {
            *m.file_query_errors.entry(k).or_default() += v;
        }
        if m.error_samples.len() < 12 {
            let room = 12 - m.error_samples.len();
            m.error_samples.extend(a.error_samples.into_iter().take(room));
        }
        if m.per_tag.len() < a.per_tag.len() {
            m.per_tag.resize(a.per_tag.len(), TagTotals::default());
        }
        for (i, t) in a.per_tag.iter().enumerate() {
            m.per_tag[i].files += t.files;
            m.per_tag[i].alloc += t.alloc;
            m.per_tag[i].eof += t.eof;
        }
        m.recs.extend(a.recs);
        if !a.root_entries.is_empty() {
            m.root_entries = a.root_entries;
        }
        if !a.top_names.is_empty() {
            m.top_names = a.top_names;
        }
    }
    m
}

// ---------------------------------------------------------------------------------------------
// Referencia mono-hilo con std::fs (para validar recuentos)
// ---------------------------------------------------------------------------------------------

fn scan_std(root: &str) -> (u64, u64, u64, u64, f64) {
    let t0 = Instant::now();
    let (mut files, mut dirs, mut eof, mut errs) = (0u64, 0u64, 0u64, 0u64);
    let mut stack = vec![std::path::PathBuf::from(root)];
    while let Some(d) = stack.pop() {
        let rd = match std::fs::read_dir(&d) {
            Ok(r) => r,
            Err(_) => {
                errs += 1;
                continue;
            }
        };
        for e in rd.flatten() {
            let md = match e.metadata() {
                Ok(m) => m,
                Err(_) => {
                    errs += 1;
                    continue;
                }
            };
            use std::os::windows::fs::MetadataExt;
            let is_reparse = md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
            if md.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0 {
                if !is_reparse {
                    dirs += 1;
                    stack.push(e.path());
                }
            } else {
                files += 1;
                eof += md.len();
            }
        }
    }
    (files, dirs, eof, errs, t0.elapsed().as_secs_f64())
}

// ---------------------------------------------------------------------------------------------
// Sondeo de metadatos NTFS y FSCTL_GET_NTFS_VOLUME_DATA
// ---------------------------------------------------------------------------------------------

fn open_raw(path: &str, access: u32, flags: u32) -> Result<HANDLE, u32> {
    let p: Vec<u16> = std::ffi::OsStr::new(path).encode_wide().chain(Some(0)).collect();
    unsafe {
        let h = CreateFileW(
            p.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            flags,
            std::ptr::null_mut(),
        );
        if h == INVALID_HANDLE_VALUE { Err(GetLastError()) } else { Ok(h) }
    }
}

fn ntfs_volume_data(h: HANDLE) -> Result<NTFS_VOLUME_DATA_BUFFER, u32> {
    unsafe {
        let mut out: NTFS_VOLUME_DATA_BUFFER = std::mem::zeroed();
        let mut ret = 0u32;
        let ok = DeviceIoControl(
            h,
            FSCTL_GET_NTFS_VOLUME_DATA,
            std::ptr::null(),
            0,
            &mut out as *mut _ as *mut _,
            std::mem::size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
            &mut ret,
            std::ptr::null_mut(),
        );
        if ok == 0 { Err(GetLastError()) } else { Ok(out) }
    }
}

fn probe(root: &str) -> serde_json::Value {
    use serde_json::json;
    let drive = root.trim_end_matches('\\').trim_start_matches(r"\\?\"); // "C:"
    let vol_path = format!(r"\\.\{drive}");
    let mut attempts = Vec::new();
    // Intentos: (descripción, ruta, acceso, flags)
    let tries: Vec<(&str, String, u32, u32)> = vec![
        ("volumen \\\\.\\C: acceso 0", vol_path.clone(), 0, 0),
        ("volumen \\\\.\\C: FILE_READ_ATTRIBUTES", vol_path.clone(), FILE_READ_ATTRIBUTES, 0),
        ("volumen \\\\.\\C: GENERIC_READ", vol_path.clone(), GENERIC_READ, 0),
        (
            "directorio raíz \\\\?\\C:\\ FILE_READ_ATTRIBUTES (backup semantics)",
            format!(r"\\?\{drive}\"),
            FILE_READ_ATTRIBUTES,
            FILE_FLAG_BACKUP_SEMANTICS,
        ),
        (
            "directorio raíz \\\\?\\C:\\ FILE_LIST_DIRECTORY (backup semantics)",
            format!(r"\\?\{drive}\"),
            FILE_LIST_DIRECTORY,
            FILE_FLAG_BACKUP_SEMANTICS,
        ),
    ];
    let mut volume_data = serde_json::Value::Null;
    for (desc, path, access, flags) in tries {
        match open_raw(&path, access, flags) {
            Err(code) => attempts.push(json!({"intento": desc, "open": format!("ERROR Win32 {code}")})),
            Ok(h) => {
                let r = ntfs_volume_data(h);
                let ioctl = match &r {
                    Ok(d) => {
                        if volume_data.is_null() {
                            volume_data = json!({
                                "BytesPerCluster": d.BytesPerCluster,
                                "BytesPerFileRecordSegment": d.BytesPerFileRecordSegment,
                                "TotalClusters": d.TotalClusters,
                                "FreeClusters": d.FreeClusters,
                                "TotalReserved": d.TotalReserved,
                                "MftValidDataLength": d.MftValidDataLength,
                                "MftStartLcn": d.MftStartLcn,
                                "MftZoneStart": d.MftZoneStart,
                                "MftZoneEnd": d.MftZoneEnd,
                            });
                        }
                        "OK".to_string()
                    }
                    Err(code) => format!("ERROR Win32 {code}"),
                };
                attempts.push(json!({"intento": desc, "open": "OK", "FSCTL_GET_NTFS_VOLUME_DATA": ioctl}));
                unsafe { CloseHandle(h) };
            }
        }
    }
    // Metadatos NTFS por nombre directo (solo FILE_READ_ATTRIBUTES; no se lee contenido).
    let mut meta = Vec::new();
    for name in ["$MFT", "$MFTMirr", "$LogFile", "$Volume", "$Bitmap", "$Boot", "$Secure", "$UpCase", "$Extend", "$BadClus", "$AttrDef"] {
        let p = format!(r"\\?\{drive}\{name}");
        let r = match open_raw(&p, FILE_READ_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS) {
            Ok(h) => {
                unsafe { CloseHandle(h) };
                "se puede abrir (FILE_READ_ATTRIBUTES)".to_string()
            }
            Err(c) => format!("open falla: Win32 {c}"),
        };
        meta.push(json!({"nombre": name, "resultado": r}));
    }
    json!({
        "drive": drive,
        "intentos_FSCTL": attempts,
        "volume_data": volume_data,
        "metadatos_por_nombre": meta,
        "nota": "los metadatos solo se prueban abriendo con FILE_READ_ATTRIBUTES; no se lee ni escribe contenido"
    })
}

// ---------------------------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------------------------

fn root_to_wide(root: &str) -> Vec<u16> {
    let mut r = root.to_string();
    if !r.starts_with(r"\\?\") {
        r = format!(r"\\?\{}", r);
    }
    if !r.ends_with('\\') {
        r.push('\\');
    }
    std::ffi::OsStr::new(&r).encode_wide().collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let root = get("--root").unwrap_or_else(|| r"C:\".to_string());
    let json_out = get("--json");

    if args.iter().any(|a| a == "--probe") {
        let v = probe(&root);
        let s = serde_json::to_string_pretty(&v).unwrap();
        println!("{s}");
        if let Some(p) = json_out {
            std::fs::write(p, s).unwrap();
        }
        return;
    }

    let api = match get("--api").as_deref() {
        Some("extd") | None => Api::Extd,
        Some("find") => Api::Find,
        Some("find-alloc") => Api::FindAlloc,
        Some("std") => Api::Std,
        Some(x) => panic!("--api desconocido: {x}"),
    };
    let threads: usize = get("--threads").and_then(|s| s.parse().ok()).unwrap_or(32);
    let runs: usize = get("--runs").and_then(|s| s.parse().ok()).unwrap_or(3);
    let drive_root = format!("{}\\", root.trim_end_matches('\\').chars().take(2).collect::<String>());

    let mut all_runs = Vec::new();
    for run in 1..=runs {
        if api == Api::Std {
            let (files, dirs, eof, errs, secs) = scan_std(&root);
            eprintln!("[std run {run}] {files} archivos, {dirs} dirs, eof={eof}, errores={errs}, {secs:.3}s");
            all_runs.push(serde_json::json!({
                "api": "std", "run": run, "segundos": secs, "archivos": files, "directorios": dirs,
                "eof_sum": eof, "errores": errs
            }));
            continue;
        }
        let root_w = root_to_wide(&root);
        let (mut acc, secs) = scan_parallel(api, &root_w, threads);

        // --- Dedup de hard links por FileId (solo si la API da FileId) ---
        let windows_tag = acc.top_names.iter().position(|n| n.eq_ignore_ascii_case("Windows")).map(|i| (i + 1) as u16);
        let t_dd = Instant::now();
        let (mut groups, mut extra_entries, mut avoided_alloc, mut avoided_eof) = (0u64, 0u64, 0u64, 0u64);
        let (mut win_groups_extra, mut win_avoided_alloc) = (0u64, 0u64);
        acc.recs.sort_unstable_by_key(|r| r.id);
        let mut i = 0;
        while i < acc.recs.len() {
            let mut j = i + 1;
            while j < acc.recs.len() && acc.recs[j].id == acc.recs[i].id {
                j += 1;
            }
            if j - i > 1 {
                groups += 1;
                for r in &acc.recs[i + 1..j] {
                    extra_entries += 1;
                    avoided_alloc += r.alloc;
                    avoided_eof += r.eof;
                    if Some(r.tag) == windows_tag {
                        win_groups_extra += 1;
                        win_avoided_alloc += r.alloc;
                    }
                }
            }
            i = j;
        }
        let dedup_secs = t_dd.elapsed().as_secs_f64();
        let have_ids = !acc.recs.is_empty();

        // --- Comparación con total − libre ---
        let (total, _avail, free) = spike_common::disk_free_space(&drive_root).unwrap_or((0, 0, 0));
        let used = total.saturating_sub(free);
        let alloc_dedup = acc.alloc_sum - avoided_alloc;
        let (mut tag_rows, mut win_alloc_total) = (Vec::new(), 0u64);
        for (t, tt) in acc.per_tag.iter().enumerate() {
            let name = if t == 0 { "<raíz>".to_string() } else { acc.top_names.get(t - 1).cloned().unwrap_or_default() };
            if Some(t as u16) == windows_tag {
                win_alloc_total = tt.alloc;
            }
            tag_rows.push((name, tt.files, tt.alloc, tt.eof));
        }
        tag_rows.sort_by_key(|r| std::cmp::Reverse(r.2));
        let mut toplevel = serde_json::Map::new();
        for (n, f, a, e) in tag_rows.iter().take(40) {
            toplevel.insert(n.clone(), serde_json::json!({"archivos": f, "alloc": a, "eof": e}));
        }

        let root_entries_dollar: Vec<_> = acc
            .root_entries
            .iter()
            .filter(|(n, ..)| n.starts_with('$'))
            .map(|(n, attrs, eof, alloc, id)| serde_json::json!({"nombre": n, "attrs": format!("{attrs:#x}"), "eof": eof, "alloc": alloc, "file_id_low64": id.map(|v| format!("{:#x}", v as u64))}))
            .collect();
        // FileId bajo (número de registro MFT < 24 = metadatos NTFS del sistema)
        let sys_meta_ids: Vec<_> = acc
            .root_entries
            .iter()
            .filter(|(_, _, _, _, id)| id.map(|v| ((v as u64) & 0x0000_FFFF_FFFF_FFFF) < 24).unwrap_or(false))
            .map(|(n, ..)| n.clone())
            .collect();

        eprintln!(
            "[{} run {run}] {secs:.3}s · {} archivos · {} dirs · alloc={:.2} GiB · eof={:.2} GiB · hardlinks evitados={:.2} GiB · errores dir={:?}",
            api.name(),
            acc.files,
            acc.dirs,
            acc.alloc_sum as f64 / 1073741824.0,
            acc.eof_sum as f64 / 1073741824.0,
            avoided_alloc as f64 / 1073741824.0,
            acc.errors
        );
        all_runs.push(serde_json::json!({
            "api": api.name(), "run": run, "threads": threads, "root": root,
            "segundos": secs,
            "archivos": acc.files, "directorios": acc.dirs, "directorios_visitados": acc.dirs_visited,
            "alloc_sum_archivos": acc.alloc_sum, "eof_sum_archivos": acc.eof_sum,
            "dir_alloc_sum": acc.dir_alloc_sum,
            "alloc_menos_eof": acc.alloc_sum as i64 - acc.eof_sum as i64,
            "estimacion_alloc_eof_redondeado_a_4096": acc.est_alloc_4k,
            "error_estimacion_vs_alloc_real_bytes": acc.est_alloc_4k as i64 - acc.alloc_sum as i64,
            "archivos_residentes_alloc0_eof_mayor0": acc.resident_files,
            "tiene_alloc_y_fileid": have_ids,
            "handles_abiertos_por_archivo": acc.handles_opened,
            "reparse_dirs_no_seguidos": acc.reparse_dirs,
            "reparse_files": acc.reparse_files,
            "reparse_files_alloc_bytes": acc.reparse_file_bytes_alloc,
            "reparse_tags": acc.reparse_tags.iter().map(|(k, v)| (format!("{k:#010x}"), serde_json::json!({"n": v.0, "bytes": v.1}))).collect::<serde_json::Map<_,_>>(),
            "errores_dir_por_codigo_win32": acc.errors.iter().map(|(k, v)| (k.to_string(), *v)).collect::<BTreeMap<_,_>>(),
            "errores_consulta_archivo_por_codigo": acc.file_query_errors.iter().map(|(k, v)| (k.to_string(), *v)).collect::<BTreeMap<_,_>>(),
            "muestras_error": acc.error_samples,
            "hardlinks": {
                "grupos_con_mas_de_una_entrada": groups,
                "entradas_extra": extra_entries,
                "bytes_alloc_evitados": avoided_alloc,
                "bytes_eof_evitados": avoided_eof,
                "windows_entradas_extra": win_groups_extra,
                "windows_bytes_alloc_evitados": win_avoided_alloc,
                "windows_alloc_total_con_duplicados": win_alloc_total,
                "segundos_dedup_sort": dedup_secs,
            },
            "disco": {"total": total, "libre": free, "usado_total_menos_libre": used},
            "escaneado_alloc_dedup": alloc_dedup,
            "hueco_usado_menos_escaneado_dedup": used as i64 - alloc_dedup as i64,
            "hueco_pct_del_usado": if used > 0 { (used as f64 - alloc_dedup as f64) / used as f64 * 100.0 } else { 0.0 },
            "hueco_sin_dedup": used as i64 - acc.alloc_sum as i64,
            "toplevel_por_alloc": toplevel,
            "entradas_raiz_con_dolar": root_entries_dollar,
            "entradas_raiz_con_file_record_number_menor_24": sys_meta_ids,
            "n_entradas_raiz": acc.root_entries.len(),
        }));
    }
    let out = serde_json::json!({ "runs": all_runs });
    if let Some(p) = json_out {
        std::fs::write(p, serde_json::to_string_pretty(&out).unwrap()).unwrap();
    }
}
