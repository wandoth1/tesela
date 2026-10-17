//! Windows API calls for listing directories and querying volumes. READ-ONLY.
//!
//! All enumerator `unsafe` code is in this module, in small functions with their own `// SAFETY:` notes. The tests below
//! (and scanner integration tests) exercise every function. Nothing opens files with write access.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::IntoRawHandle;
use std::path::{Path, PathBuf};
use windows_sys::Wdk::Storage::FileSystem::{
    FileFsFullSizeInformation, FileFsFullSizeInformationEx, NtQueryVolumeInformationFile,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_DATA, ERROR_INVALID_NAME, ERROR_NO_MORE_FILES, ERROR_NOT_SUPPORTED,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_BASIC_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_EXTD_DIR_INFO, FILE_ID_INFO,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO,
    FIND_FIRST_EX_LARGE_FETCH, FileAttributeTagInfo, FileBasicInfo, FileIdExtdDirectoryInfo,
    FileIdInfo, FileStandardInfo, FindClose, FindExInfoBasic, FindExSearchNameMatch,
    FindFirstFileExW, FindNextFileW, GetDiskFreeSpaceExW, GetDiskFreeSpaceW,
    GetFileInformationByHandleEx, GetFinalPathNameByHandleW, GetLongPathNameW,
    GetVolumeInformationByHandleW, OPEN_EXISTING, VOLUME_NAME_DOS, VOLUME_NAME_GUID,
    WIN32_FIND_DATAW,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

pub use windows_sys::Win32::Storage::FileSystem::{FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES};

/// Enumeration buffer size per thread, in bytes.
pub const BUF_BYTES: usize = 64 * 1024;

/// Convert a path to UTF-16 without a trailing NUL.
pub fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().collect()
}

fn with_nul(path: &[u16]) -> Vec<u16> {
    let mut v = Vec::with_capacity(path.len() + 1);
    v.extend_from_slice(path);
    v.push(0);
    v
}

/// Directory handle, closed when dropped.
pub struct DirHandle(HANDLE);

// SAFETY: a kernel file HANDLE can be used and closed from any thread; each `DirHandle` here is also owned by one thread at a time.
unsafe impl Send for DirHandle {}

impl Drop for DirHandle {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a valid handle opened by `open_dir` and is closed only here, exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

/// Open a directory (or volume root) READ-ONLY: `access` is `FILE_LIST_DIRECTORY` or
/// `FILE_READ_ATTRIBUTES`; share read, write, and delete access so no one is blocked, and open the
/// reparse point itself instead of following it (unless `follow_reparse` is set, only for cloud folders). Return the
/// Windows error code on failure.
pub fn open_dir(path: &[u16], access: u32, follow_reparse: bool) -> Result<DirHandle, u32> {
    let p = with_nul(path);
    // A normal reparse point is opened itself (not followed). Only cloud folders (OneDrive...) are opened
    // through their filter, which allows them to be listed.
    let flags = if follow_reparse {
        FILE_FLAG_BACKUP_SEMANTICS
    } else {
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT
    };
    // SAFETY: `p` is a NUL-terminated UTF-16 string that remains alive for the call; the other arguments are
    // constants or null pointers allowed by the API (no security attributes or template).
    let h = unsafe {
        CreateFileW(
            p.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            flags,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        // SAFETY: `GetLastError` has no preconditions.
        return Err(unsafe { GetLastError() });
    }
    Ok(DirHandle(h))
}

/// Metadata for a directory opened with `FILE_READ_ATTRIBUTES`.
///
/// REQUIRED metadata (attributes, mtime, sizes: `FileBasicInfo` and `FileStandardInfo`, supported by
/// any file system) is separated from OPTIONAL metadata (`file_id` and `reparse_tag`): if the system does not support
/// identity queries, scanning continues with `FindFirstFileExW`.
#[derive(Clone, Copy, Debug)]
pub struct DirMeta {
    /// Identificador de archivo de 128 bits, OPCIONAL: `None` si el sistema de archivos no soporta `FileIdInfo`
    /// (FAT, exFAT, algunos recursos de red...). Solo lo usan los tests (comprueba que coincide con el `FileId`
    /// del listado del padre).
    #[allow(dead_code)]
    pub file_id: Option<u128>,
    pub attrs: u32,
    /// Etiqueta de reparse (`FileAttributeTagInfo`); 0 si no es un reparse point o la consulta no está soportada.
    pub reparse_tag: u32,
    pub mtime_filetime: i64,
    pub alloc: u64,
    pub logical: u64,
}

/// Query a handle information class with `GetFileInformationByHandleEx`.
fn query_info<T>(h: &DirHandle, class: i32) -> Result<T, u32> {
    // SAFETY: `T` is one of the API's POD structures (integers and byte arrays), for which an all-zero value is
    // valid.
    let mut v: T = unsafe { std::mem::zeroed() };
    // SAFETY: `h.0` is a valid handle; the pointer refers to `v` and its exact size is passed.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            h.0,
            class,
            &mut v as *mut T as *mut c_void,
            size_of::<T>() as u32,
        )
    };
    if ok == 0 {
        // SAFETY: `GetLastError` has no preconditions.
        return Err(unsafe { GetLastError() });
    }
    Ok(v)
}

/// Read directory metadata. `FILE_BASIC_INFO` and `FILE_STANDARD_INFO` are required (errors are
/// returned); `FILE_ID_INFO` and `FILE_ATTRIBUTE_TAG_INFO` are optional: if they fail (usually 50 or 87, "not
/// supported"), the identifier is set to `None` and the tag to 0.
pub fn dir_meta(h: &DirHandle) -> Result<DirMeta, u32> {
    dir_meta_with(
        h,
        |h| {
            query_info::<FILE_ID_INFO>(h, FileIdInfo)
                .map(|id| u128::from_le_bytes(id.FileId.Identifier))
        },
        |h| query_info::<FILE_ATTRIBUTE_TAG_INFO>(h, FileAttributeTagInfo).map(|t| t.ReparseTag),
    )
}

/// Igual que [`dir_meta`] con las dos consultas opcionales inyectables (los tests simulan que no están soportadas).
fn dir_meta_with(
    h: &DirHandle,
    id_query: impl FnOnce(&DirHandle) -> Result<u128, u32>,
    tag_query: impl FnOnce(&DirHandle) -> Result<u32, u32>,
) -> Result<DirMeta, u32> {
    let basic: FILE_BASIC_INFO = query_info(h, FileBasicInfo)?;
    let standard: FILE_STANDARD_INFO = query_info(h, FileStandardInfo)?;
    let attrs = basic.FileAttributes;
    // A tag is valid only when the reparse-point attribute is set.
    let reparse_tag = if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        tag_query(h).unwrap_or(0)
    } else {
        0
    };
    Ok(DirMeta {
        file_id: id_query(h).ok(),
        attrs,
        reparse_tag,
        mtime_filetime: basic.LastWriteTime,
        alloc: standard.AllocationSize.max(0) as u64,
        logical: standard.EndOfFile.max(0) as u64,
    })
}

/// Open the user-provided path WITHOUT following the final component's reparse point (junction, symlink, or
/// mount point): the handle refers to the link itself, not its target, so the target is not opened. Request only
/// `access` (usually `FILE_READ_ATTRIBUTES`). The standard library (`OpenOptions`) does this and also adds
/// `\\?\` to long paths. The system still resolves INTERMEDIATE link components as usual (they
/// cannot be avoided when opening a path). Return the Windows error code on failure.
pub fn open_no_follow(path: &Path, access: u32) -> Result<DirHandle, u32> {
    let file = std::fs::OpenOptions::new()
        .access_mode(access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|e| e.raw_os_error().map_or(ERROR_INVALID_NAME, |c| c as u32))?;
    Ok(DirHandle(file.into_raw_handle() as HANDLE))
}

/// Canonical final path for the handle (`\\?\C:\...`, with on-disk folder casing and long names),
/// calculated THROUGH THE ALREADY OPEN handle: for a link opened with [`open_no_follow`], this returns the link's path,
/// not its target's.
///
/// `volume_name` is `VOLUME_NAME_DOS` (drive letters and mount folders) or `VOLUME_NAME_GUID` (`Volume{GUID}`).
fn final_path_with(h: &DirHandle, volume_name: u32) -> Result<PathBuf, u32> {
    let mut buf = vec![0u16; 512];
    loop {
        // SAFETY: `h.0` is a valid handle; `buf` is writable and its exact length in UTF-16 units is passed. The
        // flags are `FILE_NAME_NORMALIZED` (0) plus the requested volume format.
        let n = unsafe {
            GetFinalPathNameByHandleW(h.0, buf.as_mut_ptr(), buf.len() as u32, volume_name)
        };
        if n == 0 {
            // SAFETY: `GetLastError` has no preconditions.
            return Err(unsafe { GetLastError() });
        }
        let n = n as usize;
        if n < buf.len() {
            return Ok(PathBuf::from(std::ffi::OsString::from_wide(&buf[..n])));
        }
        // Búfer corto: `n` es el tamaño necesario, con el NUL final.
        buf.resize(n + 1, 0);
    }
}

/// An opened path that did not follow its final reparse point, with its metadata and canonical path.
pub struct Opened {
    pub handle: DirHandle,
    pub meta: DirMeta,
    pub path: PathBuf,
}

/// [`open_no_follow`] with `FILE_READ_ATTRIBUTES` + [`dir_meta`] + the handle's final path (`final_path_with`). The caller checks `meta` (is this a
/// directory? a reparse point?) BEFORE using `path` for traversal.
///
/// The path uses the DOS form (`\\?\C:\...`), except for a volume root opened through `\\?\Volume{GUID}\` that has NO
/// drive letter: its DOS name is the mount folder, and traversing it would reopen the (empty) reparse directory on the
/// parent volume. In that case, keep the `\\?\Volume{GUID}\` path (see `paths::choose_root_path`). If the volume has no
/// DOS name, use the GUID path as well.
pub fn open_resolved(path: &Path) -> Result<Opened, u32> {
    let handle = open_no_follow(path, FILE_READ_ATTRIBUTES)?;
    let meta = dir_meta(&handle)?;
    let path = match final_path_with(&handle, VOLUME_NAME_DOS) {
        Ok(dos) => match final_path_with(&handle, VOLUME_NAME_GUID) {
            Ok(guid) => crate::paths::choose_root_path(dos, &guid),
            Err(_) => dos,
        },
        Err(code) => match final_path_with(&handle, VOLUME_NAME_GUID) {
            Ok(guid) => crate::paths::volume_guid_root(&guid).ok_or(code)?,
            Err(_) => return Err(code),
        },
    };
    Ok(Opened { handle, meta, path })
}

/// Expand `path` to its LONG form (without 8.3 names) using `GetLongPathNameW`, without opening it for traversal: only expand
/// existing short aliases (`PROGRA~1`). This lets a path from an environment variable
/// (`%TEMP%` is often in 8.3 form) be compared with listed names. This does NOT replace
/// `priority`'s component-by-component validation: this does not resolve links. Returns a Windows error code on failure.
pub fn long_path(path: &Path) -> Result<PathBuf, u32> {
    let p = with_nul(&wide(path));
    let mut buf = vec![0u16; 512];
    loop {
        // SAFETY: `p` is NUL-terminated and remains alive for the call; `buf` is writable and its exact length is passed.
        let n = unsafe { GetLongPathNameW(p.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
        if n == 0 {
            // SAFETY: `GetLastError` has no preconditions.
            return Err(unsafe { GetLastError() });
        }
        let n = n as usize;
        if n < buf.len() {
            return Ok(PathBuf::from(std::ffi::OsString::from_wide(&buf[..n])));
        }
        // Búfer corto: `n` es el tamaño necesario, con el NUL final.
        buf.resize(n + 1, 0);
    }
}

/// Directory entry data as reported by the parent's listing.
#[derive(Clone, Copy, Debug)]
pub struct EntryInfo {
    pub attrs: u32,
    pub logical: u64,
    pub alloc: u64,
    pub file_id: u128,
    pub reparse_tag: u32,
    pub mtime_filetime: i64,
}

/// Find `name` (case-insensitive) in `parent`'s listing and return the data reported for that
/// entry. Used to precreate priority-path ancestors with EXACTLY the same values as
/// general traversal: a directory's size from its parent's listing may differ from the size
/// reported by `FileStandardInfo` on the open directory; its `FileId` is used for adoption comparisons.
#[cfg(test)]
pub fn find_entry(parent: &[u16], name: &[u16]) -> Result<Option<EntryInfo>, u32> {
    Ok(find_entry_named(parent, name)?.map(|(_, info)| info))
}

/// [`find_entry`] que además devuelve el nombre EXACTO (mayúsculas y minúsculas del disco) de la entrada encontrada.
pub fn find_entry_named(
    parent: &[u16],
    name: &[u16],
) -> Result<Option<(Vec<u16>, EntryInfo)>, u32> {
    let h = open_dir(parent, FILE_LIST_DIRECTORY, false)?;
    let mut buf = vec![0u64; BUF_BYTES / 8];
    let wanted = String::from_utf16_lossy(name).to_lowercase();
    let info = |e: &RawEntry| {
        (
            e.name.to_vec(),
            EntryInfo {
                attrs: e.attrs,
                logical: e.logical,
                alloc: e.alloc.unwrap_or(0),
                file_id: e.file_id,
                reparse_tag: e.reparse_tag,
                mtime_filetime: e.mtime_filetime,
            },
        )
    };
    // Una carpeta que distingue mayúsculas puede tener hermanos `Temp`, `temp` y `TEMP`: gana siempre la coincidencia
    // EXACTA. La insensible solo vale como respaldo y solo si es única (con varias no se sabe cuál es).
    let mut exact: Option<(Vec<u16>, EntryInfo)> = None;
    let mut insensitive: Option<(Vec<u16>, EntryInfo)> = None;
    let mut insensitive_count = 0u32;
    enum_dir_extd(&h, &mut buf, &mut |e| {
        if e.name == name {
            exact = Some(info(e));
        } else if String::from_utf16_lossy(e.name).to_lowercase() == wanted {
            insensitive_count += 1;
            insensitive = Some(info(e));
        }
    })?;
    Ok(exact.or(if insensitive_count == 1 {
        insensitive
    } else {
        None
    }))
}

/// Una entrada de directorio, independiente de la API que la leyó.
pub struct RawEntry<'a> {
    pub name: &'a [u16],
    pub attrs: u32,
    pub logical: u64,
    /// `None` con `FindFirstFileExW` (no da `AllocationSize`).
    pub alloc: Option<u64>,
    /// 0 si la API no lo da.
    pub file_id: u128,
    /// Etiqueta de reparse (solo vale con el atributo `REPARSE_POINT`).
    pub reparse_tag: u32,
    pub mtime_filetime: i64,
}

fn is_dot(name: &[u16]) -> bool {
    name == [b'.' as u16] || name == [b'.' as u16, b'.' as u16]
}

/// Etiqueta de reparse de una entrada del listado: solo vale con el atributo `REPARSE_POINT`. En
/// `FILE_ID_EXTD_DIR_INFO` el campo `ReparsePointTag` comparte memoria con `EaSize`: en una entrada que NO es un
/// reparse point trae el tamaño de sus atributos extendidos, no una etiqueta (y podría parecer una).
fn entry_reparse_tag(attrs: u32, raw: u32) -> u32 {
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        raw
    } else {
        0
    }
}

fn read_u32(b: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn read_i64(b: &[u8], at: usize) -> i64 {
    i64::from_ne_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

/// Enumera un directorio con `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)`: una sola pasada da tamaño lógico,
/// `AllocationSize`, `FileId` de 128 bits y etiqueta de reparse por lotes (PLAN.md §3.3). `buf` es el búfer de trabajo.
/// Devuelve el código de error de Windows si falla (por ejemplo 87 si el sistema no soporta esta clase).
///
/// Los campos se leen con índices comprobados sobre una vista de bytes del búfer: si el sistema devolviera algo
/// inconsistente, se produce un error y no una lectura fuera de rango.
pub fn enum_dir_extd(
    h: &DirHandle,
    buf: &mut [u64],
    emit: &mut dyn FnMut(&RawEntry),
) -> Result<(), u32> {
    const HDR: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, FileName);
    const NEXT: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, NextEntryOffset);
    const MTIME: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, LastWriteTime);
    const EOF: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, EndOfFile);
    const ALLOC: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, AllocationSize);
    const ATTRS: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, FileAttributes);
    const NAME_LEN: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, FileNameLength);
    const TAG: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, ReparsePointTag);
    const ID: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, FileId);
    let buf_len = std::mem::size_of_val(buf);
    let mut name: Vec<u16> = Vec::with_capacity(260);
    loop {
        // SAFETY: `h.0` is a valid directory handle; `buf` is writable memory of `buf_len` bytes and is
        // alineada a 8 (es un `[u64]`), como exige la API.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                h.0,
                FileIdExtdDirectoryInfo,
                buf.as_mut_ptr() as *mut c_void,
                buf_len as u32,
            )
        };
        if ok == 0 {
            // SAFETY: `GetLastError` has no preconditions.
            let e = unsafe { GetLastError() };
            return if e == ERROR_NO_MORE_FILES {
                Ok(())
            } else {
                Err(e)
            };
        }
        // SAFETY: `buf` is initialized memory of `buf_len` bytes (a whole `[u64]` array) and is not modified while
        // this view is alive (the next API call occurs only after it is dropped).
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, buf_len) };
        let mut off = 0usize;
        loop {
            // El sistema garantiza esto; se comprueba para no panicar nunca por datos inesperados.
            if !off.is_multiple_of(8) || off + HDR > buf_len {
                return Err(ERROR_INVALID_DATA);
            }
            let rec = &bytes[off..];
            let next = read_u32(rec, NEXT) as usize;
            let name_len = read_u32(rec, NAME_LEN) as usize;
            if !name_len.is_multiple_of(2) || HDR + name_len > rec.len() {
                return Err(ERROR_INVALID_DATA);
            }
            name.clear();
            name.extend(
                rec[HDR..HDR + name_len]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_ne_bytes(*c)),
            );
            if !is_dot(&name) {
                let id: [u8; 16] = rec[ID..ID + 16].try_into().expect("16 bytes");
                emit(&RawEntry {
                    name: &name,
                    attrs: read_u32(rec, ATTRS),
                    logical: read_i64(rec, EOF).max(0) as u64,
                    alloc: Some(read_i64(rec, ALLOC).max(0) as u64),
                    file_id: u128::from_le_bytes(id),
                    reparse_tag: entry_reparse_tag(read_u32(rec, ATTRS), read_u32(rec, TAG)),
                    mtime_filetime: read_i64(rec, MTIME),
                });
            }
            if next == 0 {
                break;
            }
            off += next;
        }
    }
}

/// Handle de búsqueda de `FindFirstFileExW`, cerrado al soltarlo (también si `emit` entra en pánico).
struct FindHandle(HANDLE);

impl Drop for FindHandle {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the valid search handle returned by `FindFirstFileExW`; it is closed only here.
        unsafe { FindClose(self.0) };
    }
}

/// Respaldo: `FindFirstFileExW(FindExInfoBasic, LARGE_FETCH)`. No da `AllocationSize` ni `FileId`.
pub fn enum_dir_find(path: &[u16], emit: &mut dyn FnMut(&RawEntry)) -> Result<(), u32> {
    let mut pat = Vec::with_capacity(path.len() + 3);
    pat.extend_from_slice(path);
    if pat.last() != Some(&(b'\\' as u16)) {
        pat.push(b'\\' as u16);
    }
    pat.push(b'*' as u16);
    pat.push(0);
    // SAFETY: `WIN32_FIND_DATAW` is POD (integers and `u16` arrays); an all-zero value is valid.
    let mut fd: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    // SAFETY: `pat` is NUL-terminated; `fd` is writable and has the type expected by `FindExInfoBasic`; the other
    // arguments are the documented values for an unfiltered search.
    let h = unsafe {
        FindFirstFileExW(
            pat.as_ptr(),
            FindExInfoBasic,
            &mut fd as *mut _ as *mut c_void,
            FindExSearchNameMatch,
            std::ptr::null(),
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        // SAFETY: `GetLastError` has no preconditions.
        return Err(unsafe { GetLastError() });
    }
    let h = FindHandle(h);
    loop {
        let len = fd
            .cFileName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(fd.cFileName.len());
        let name = &fd.cFileName[..len];
        if !is_dot(name) {
            let reparse = fd.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
            emit(&RawEntry {
                name,
                attrs: fd.dwFileAttributes,
                logical: ((fd.nFileSizeHigh as u64) << 32) | fd.nFileSizeLow as u64,
                alloc: None,
                file_id: 0,
                reparse_tag: if reparse { fd.dwReserved0 } else { 0 },
                mtime_filetime: ((fd.ftLastWriteTime.dwHighDateTime as i64) << 32)
                    | fd.ftLastWriteTime.dwLowDateTime as i64,
            });
        }
        // SAFETY: `h.0` is the valid search handle returned by `FindFirstFileExW`; `fd` is writable.
        if unsafe { FindNextFileW(h.0, &mut fd) } == 0 {
            // SAFETY: `GetLastError` has no preconditions.
            let e = unsafe { GetLastError() };
            return if e == ERROR_NO_MORE_FILES {
                Ok(())
            } else {
                Err(e)
            };
        }
    }
}

/// Espacio de un volumen. El ÁMBITO de cada cifra importa y no se mezcla:
///
/// * Con `volume_scope == true`: `total` es la CAPACIDAD del volumen y `free` lo libre del VOLUMEN entero (`total - free`
///   es lo usado en el volumen, lo único comparable con lo escaneado para el "espacio no explicado"); `available` es lo
///   disponible para el USUARIO (menor que `free` si hay cuota de disco).
/// * Con `volume_scope == false` (hay una cuota y Windows no da la capacidad real del volumen): `total` y `free` son
///   AMBOS del usuario (su cuota y lo que le queda), coherentes entre sí para enseñarlos, pero NO sirven para el
///   residuo: no hay "espacio no explicado".
///
/// Consultas, por orden (`NtQueryVolumeInformationFile`):
/// 1. `FileFsFullSizeInformationEx` (clase 14, Windows 10 1809+): `ActualTotalAllocationUnits` (volumen),
///    `ActualAvailableAllocationUnits` (volumen) y `CallerAvailableAllocationUnits` (usuario).
/// 2. `FileFsFullSizeInformation` (clase 7): con cuota, `TotalAllocationUnits` es lo disponible PARA EL USUARIO (no la
///    capacidad del volumen), así que solo vale como ámbito de volumen cuando `CallerAvailable == ActualAvailable`.
/// 3. `GetDiskFreeSpaceExW`: igual, `lpTotalNumberOfBytes` respeta la cuota; solo vale si no hay cuota.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeSizes {
    pub total: u64,
    pub free: u64,
    pub available: u64,
    pub volume_scope: bool,
}

/// `FILE_FS_FULL_SIZE_INFORMATION` (clase 7, `ntifs.h`).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FsFullSize {
    total_units: i64,
    caller_available_units: i64,
    actual_available_units: i64,
    sectors_per_unit: u32,
    bytes_per_sector: u32,
}

/// `FILE_FS_FULL_SIZE_INFORMATION_EX` (clase 14, `ntifs.h`; Windows 10 1809+).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FsFullSizeEx {
    actual_total_units: u64,
    actual_available_units: u64,
    actual_pool_unavailable_units: u64,
    caller_total_units: u64,
    caller_available_units: u64,
    caller_pool_unavailable_units: u64,
    used_units: u64,
    total_reserved_units: u64,
    volume_storage_reserve_units: u64,
    available_committed_units: u64,
    pool_available_units: u64,
    sectors_per_unit: u32,
    bytes_per_sector: u32,
}

// El diseño de las estructuras FFI tiene que ser el del SDK: 3 × 8 + 2 × 4 y 11 × 8 + 2 × 4 bytes.
const _: () = assert!(size_of::<FsFullSize>() == 32);
const _: () = assert!(size_of::<FsFullSizeEx>() == 96);
const _: () = assert!(std::mem::offset_of!(FsFullSizeEx, sectors_per_unit) == 88);
const _: () = assert!(std::mem::offset_of!(FsFullSize, sectors_per_unit) == 24);

fn units_to_bytes(units: u64, unit: u128) -> u64 {
    ((units as u128) * unit).min(u64::MAX as u128) as u64
}

impl FsFullSize {
    fn sizes(&self) -> Option<VolumeSizes> {
        let unit = (self.sectors_per_unit as u128) * (self.bytes_per_sector as u128);
        if unit == 0 || self.total_units <= 0 {
            return None;
        }
        let b = |u: i64| units_to_bytes(u.max(0) as u64, unit);
        let (total, caller, actual) = (
            b(self.total_units),
            b(self.caller_available_units),
            b(self.actual_available_units),
        );
        if caller == actual {
            // Sin cuota que lo distinga: `total` es la capacidad del volumen.
            let free = actual.min(total);
            Some(VolumeSizes {
                total,
                free,
                available: free,
                volume_scope: true,
            })
        } else {
            // Con cuota, `total` es lo del usuario: se enseña con lo que le queda a él, y no vale para el residuo.
            let free = caller.min(total);
            Some(VolumeSizes {
                total,
                free,
                available: free,
                volume_scope: false,
            })
        }
    }
}

impl FsFullSizeEx {
    fn sizes(&self) -> Option<VolumeSizes> {
        let unit = (self.sectors_per_unit as u128) * (self.bytes_per_sector as u128);
        if unit == 0 || self.actual_total_units == 0 {
            return None;
        }
        let total = units_to_bytes(self.actual_total_units, unit);
        let free = units_to_bytes(self.actual_available_units, unit).min(total);
        let available = units_to_bytes(self.caller_available_units, unit).min(free);
        Some(VolumeSizes {
            total,
            free,
            available,
            volume_scope: true,
        })
    }
}

/// `NtQueryVolumeInformationFile` de la clase `class` sobre el handle, escribiendo en `info`. Devuelve el NTSTATUS si falla.
fn nt_query<T>(h: &DirHandle, info: &mut T, class: i32) -> Result<(), i32> {
    // SAFETY: `IO_STATUS_BLOCK` is POD (a pointer/integer and a `usize`); an all-zero value is valid.
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: `h.0` is a valid file handle; `iosb` and `info` are writable, and the API receives the EXACT size
    // of `T`, a `repr(C)` structure matching the SDK layout (verified by the `const assert`s above).
    let status = unsafe {
        NtQueryVolumeInformationFile(
            h.0,
            &mut iosb,
            info as *mut T as *mut c_void,
            size_of::<T>() as u32,
            class,
        )
    };
    if status < 0 { Err(status) } else { Ok(()) }
}

/// Clase 14 (`FileFsFullSizeInformationEx`). `Err` si el sistema no la soporta (Windows anterior a 1809) o falla.
fn nt_full_size_ex(h: &DirHandle) -> Result<VolumeSizes, i32> {
    let mut info = FsFullSizeEx::default();
    nt_query(h, &mut info, FileFsFullSizeInformationEx)?;
    info.sizes().ok_or(-1)
}

/// Clase 7 (`FileFsFullSizeInformation`): su ámbito depende de si hay cuota (ver [`FsFullSize::sizes`]).
fn nt_full_size(h: &DirHandle) -> Result<VolumeSizes, i32> {
    let mut info = FsFullSize::default();
    nt_query(h, &mut info, FileFsFullSizeInformation)?;
    info.sizes().ok_or(-1)
}

/// `GetDiskFreeSpaceExW`: (disponible para el usuario, total según la cuota, libre del volumen).
fn ex_space(path: &[u16]) -> Result<(u64, u64, u64), u32> {
    let p = with_nul(path);
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: `p` is NUL-terminated and the three pointers refer to writable local variables.
    let ok = unsafe { GetDiskFreeSpaceExW(p.as_ptr(), &mut avail, &mut total, &mut free) };
    if ok == 0 {
        // SAFETY: `GetLastError` has no preconditions.
        return Err(unsafe { GetLastError() });
    }
    Ok((avail, total, free))
}

/// Espacio del volumen que contiene `path` (ver [`VolumeSizes`]).
pub fn volume_space(path: &[u16]) -> Result<VolumeSizes, u32> {
    // Las dos consultas NT comparten el handle (un solo `CreateFileW`).
    let handle = open_dir(path, FILE_READ_ATTRIBUTES, false);
    volume_space_with(
        path,
        |_| match &handle {
            Ok(h) => nt_full_size_ex(h).map_err(|_| ERROR_NOT_SUPPORTED),
            Err(c) => Err(*c),
        },
        |_| match &handle {
            Ok(h) => nt_full_size(h).map_err(|_| ERROR_NOT_SUPPORTED),
            Err(c) => Err(*c),
        },
        ex_space,
    )
}

/// Igual que [`volume_space`] con las tres consultas inyectables (los tests simulan cuotas y fallos).
fn volume_space_with(
    path: &[u16],
    class14: impl FnOnce(&[u16]) -> Result<VolumeSizes, u32>,
    class7: impl FnOnce(&[u16]) -> Result<VolumeSizes, u32>,
    win32: impl FnOnce(&[u16]) -> Result<(u64, u64, u64), u32>,
) -> Result<VolumeSizes, u32> {
    if let Ok(v) = class14(path) {
        return Ok(v);
    }
    if let Ok(v) = class7(path) {
        return Ok(v);
    }
    let (avail, total, free) = win32(path)?;
    if total == 0 {
        return Err(ERROR_NOT_SUPPORTED);
    }
    if avail == free {
        Ok(VolumeSizes {
            total,
            free: free.min(total),
            available: avail.min(total),
            volume_scope: true,
        })
    } else {
        // Con cuota, `total` es el de la cuota: se enseña junto a lo que le queda al usuario y no sirve para el residuo.
        let left = avail.min(total);
        Ok(VolumeSizes {
            total,
            free: left,
            available: left,
            volume_scope: false,
        })
    }
}

/// Bytes por cluster del volumen que contiene `path` (para estimar `alloc` cuando solo hay `FindFirstFileExW`).
pub fn cluster_bytes(path: &[u16]) -> Option<u64> {
    let p = with_nul(path);
    let (mut spc, mut bps, mut free, mut total) = (0u32, 0u32, 0u32, 0u32);
    // SAFETY: `p` is NUL-terminated and the four pointers refer to writable local variables.
    let ok = unsafe { GetDiskFreeSpaceW(p.as_ptr(), &mut spc, &mut bps, &mut free, &mut total) };
    (ok != 0 && spc != 0 && bps != 0).then(|| spc as u64 * bps as u64)
}

/// Nombre del sistema de archivos del volumen del handle (p. ej. `NTFS`, `ReFS`, `FAT32`).
pub fn fs_name(h: &DirHandle) -> Result<String, u32> {
    let mut name = [0u16; 64];
    // SAFETY: `h.0` is a valid handle; `name` is writable and its length in UTF-16 units is passed; the pointers
    // que no se piden son nulos, como permite la API.
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            h.0,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            name.as_mut_ptr(),
            name.len() as u32,
        )
    };
    if ok == 0 {
        // SAFETY: `GetLastError` has no preconditions.
        return Err(unsafe { GetLastError() });
    }
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    Ok(String::from_utf16_lossy(&name[..len]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Directorio del propio proyecto (existe siempre durante `cargo test`) para lecturas de prueba.
    fn manifest_dir() -> Vec<u16> {
        wide(Path::new(env!("CARGO_MANIFEST_DIR")))
    }

    fn collect_extd(path: &[u16]) -> BTreeMap<String, (u64, u64, u128, u32)> {
        let h = open_dir(path, FILE_LIST_DIRECTORY, false).expect("abre");
        let mut buf = vec![0u64; BUF_BYTES / 8];
        let mut out = BTreeMap::new();
        enum_dir_extd(&h, &mut buf, &mut |e| {
            out.insert(
                String::from_utf16_lossy(e.name),
                (e.logical, e.alloc.unwrap(), e.file_id, e.attrs),
            );
        })
        .expect("enumera");
        out
    }

    #[test]
    fn enumera_el_directorio_del_crate_con_las_dos_apis() {
        let path = manifest_dir();
        let a = collect_extd(&path);
        assert!(a.contains_key("Cargo.toml") && a.contains_key("src"));
        let mut b = BTreeMap::new();
        enum_dir_find(&path, &mut |e| {
            assert!(e.alloc.is_none() && e.file_id == 0);
            b.insert(String::from_utf16_lossy(e.name), e.logical);
        })
        .expect("find");
        let a_logical: BTreeMap<_, _> = a.iter().map(|(k, v)| (k.clone(), v.0)).collect();
        // Las dos APIs ven los mismos nombres y los mismos tamaños lógicos de los archivos.
        assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
        for (name, l) in &b {
            if a[name].3 & 0x10 == 0 {
                assert_eq!(a_logical[name], *l, "{name}");
            }
        }
        // El FileId de Cargo.toml no es 0 y el AllocationSize cubre el tamaño.
        let (logical, alloc, id, _) = a["Cargo.toml"];
        assert_ne!(id, 0);
        assert!(alloc >= logical || alloc == 0);
    }

    #[test]
    fn open_dir_falla_con_un_codigo_si_no_existe() {
        let mut p = manifest_dir();
        p.extend(r"\no-existe-tesela".encode_utf16());
        assert!(matches!(
            open_dir(&p, FILE_LIST_DIRECTORY, false),
            Err(2 | 3)
        ));
    }

    #[test]
    fn meta_del_directorio_coincide_con_su_listado() {
        let path = manifest_dir();
        let h = open_dir(&path, FILE_READ_ATTRIBUTES, false).unwrap();
        let m = dir_meta(&h).unwrap();
        assert!(m.file_id.is_some_and(|id| id != 0));
        assert!(m.attrs & 0x10 != 0, "es un directorio");
        assert!(m.mtime_filetime > 0);
        // El FileId de `src` visto desde el padre es el mismo que da dir_meta al abrirlo.
        let listed = collect_extd(&path);
        let mut src = path.clone();
        src.extend(r"\src".encode_utf16());
        let hs = open_dir(&src, FILE_READ_ATTRIBUTES, false).unwrap();
        let ms = dir_meta(&hs).unwrap();
        assert_eq!(Some(listed["src"].2), ms.file_id);
    }

    #[test]
    fn find_entry_da_lo_mismo_que_el_listado_sin_distinguir_mayusculas() {
        let path = manifest_dir();
        let listed = collect_extd(&path);
        let e = find_entry(&path, &"SRC".encode_utf16().collect::<Vec<_>>())
            .unwrap()
            .expect("existe");
        assert_eq!((e.logical, e.alloc, e.file_id, e.attrs), listed["src"]);
        assert!(
            find_entry(
                &path,
                &"no-existe-tesela".encode_utf16().collect::<Vec<_>>()
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn espacio_de_volumen_cluster_y_sistema_de_archivos() {
        let path = manifest_dir();
        let v = volume_space(&path).unwrap();
        assert!(v.total > 0 && v.free <= v.total && v.available <= v.free);
        let c = cluster_bytes(&path).unwrap();
        assert!(c.is_power_of_two() && c >= 512);
        let h = open_dir(&path, FILE_READ_ATTRIBUTES, false).unwrap();
        assert!(!fs_name(&h).unwrap().is_empty());
    }

    #[test]
    fn un_nombre_largo_no_rompe_la_enumeracion_extd() {
        // Cubre la lectura de nombres de longitud variable: el directorio de ejecutables de test tiene nombres largos.
        let exe = std::env::current_exe().unwrap();
        let dir = exe.parent().unwrap();
        let listed = collect_extd(&wide(dir));
        assert!(listed.len() > 1);
        assert!(listed.keys().any(|k| k.len() > 20));
    }

    #[test]
    fn sin_soporte_de_identidad_los_metadatos_imprescindibles_siguen_llegando() {
        // Inyección de errores: la consulta de FileIdInfo (y la de la etiqueta de reparse) "no están soportadas"
        // (50 = ERROR_NOT_SUPPORTED, 87 = ERROR_INVALID_PARAMETER). No es un error: el identificador queda en `None` y
        // el resto de metadatos (atributos, mtime, tamaños) son los mismos que con la consulta real.
        let path = manifest_dir();
        let h = open_dir(&path, FILE_READ_ATTRIBUTES, false).unwrap();
        let real = dir_meta(&h).unwrap();
        assert!(real.file_id.is_some());
        for code in [50u32, 87, 1] {
            let m = dir_meta_with(&h, |_| Err(code), |_| Err(code)).expect("no es un error");
            assert_eq!(m.file_id, None, "código {code}");
            assert_eq!(m.reparse_tag, 0);
            assert_eq!(
                (m.attrs, m.mtime_filetime, m.alloc, m.logical),
                (real.attrs, real.mtime_filetime, real.alloc, real.logical)
            );
        }
        // Con identificador inyectado, se respeta; la etiqueta de un directorio normal es 0 y su consulta ni se hace.
        let m = dir_meta_with(&h, |_| Ok(7), |_| panic!("no es un reparse point")).unwrap();
        assert_eq!(m.file_id, Some(7));
        assert_eq!(m.attrs & FILE_ATTRIBUTE_REPARSE_POINT, 0);
    }

    #[test]
    fn la_etiqueta_de_reparse_de_la_consulta_real_es_cero_en_un_directorio_normal() {
        let path = manifest_dir();
        let h = open_dir(&path, FILE_READ_ATTRIBUTES, false).unwrap();
        // Recorre la consulta real de la etiqueta (la API de verdad) aunque no sea un reparse point.
        let t: FILE_ATTRIBUTE_TAG_INFO = query_info(&h, FileAttributeTagInfo).unwrap();
        assert_eq!(t.ReparseTag, 0);
        assert_eq!(dir_meta(&h).unwrap().reparse_tag, 0);
    }

    /// Estructura de la clase 7 con cuota: `TotalAllocationUnits` es lo disponible PARA EL USUARIO (su cuota), no la
    /// capacidad del volumen; 8 sectores de 512 B = unidades de 4096 B.
    fn class7(total: i64, caller: i64, actual: i64) -> FsFullSize {
        FsFullSize {
            total_units: total,
            caller_available_units: caller,
            actual_available_units: actual,
            sectors_per_unit: 8,
            bytes_per_sector: 512,
        }
    }

    #[test]
    fn el_diseno_de_las_estructuras_ffi_es_el_del_sdk() {
        assert_eq!(size_of::<FsFullSize>(), 32);
        assert_eq!(size_of::<FsFullSizeEx>(), 96);
        assert_eq!(std::mem::offset_of!(FsFullSize, caller_available_units), 8);
        assert_eq!(std::mem::offset_of!(FsFullSize, actual_available_units), 16);
        assert_eq!(
            std::mem::offset_of!(FsFullSizeEx, actual_available_units),
            8
        );
        assert_eq!(
            std::mem::offset_of!(FsFullSizeEx, caller_available_units),
            32
        );
        assert_eq!(std::mem::offset_of!(FsFullSizeEx, bytes_per_sector), 92);
    }

    #[test]
    fn la_clase_14_con_cuota_da_capacidad_y_libre_del_volumen_y_lo_disponible_del_usuario() {
        // Volumen de 1000 unidades (4096 B), 400 libres de verdad; cuota del usuario de 100 unidades con 60 libres.
        let info = FsFullSizeEx {
            actual_total_units: 1000,
            actual_available_units: 400,
            caller_total_units: 100,
            caller_available_units: 60,
            sectors_per_unit: 8,
            bytes_per_sector: 512,
            ..FsFullSizeEx::default()
        };
        let v = info.sizes().unwrap();
        assert_eq!(
            v,
            VolumeSizes {
                total: 4_096_000,
                free: 1_638_400,
                available: 245_760,
                volume_scope: true
            }
        );
        // Usado del volumen = total - libre del volumen (ámbito único).
        assert_eq!(v.total - v.free, 2_457_600);
        // Valores absurdos se recortan: available <= free <= total.
        let odd = FsFullSizeEx {
            caller_available_units: 900,
            actual_available_units: 2000,
            ..info
        };
        let o = odd.sizes().unwrap();
        assert!(o.available <= o.free && o.free <= o.total);
        assert!(FsFullSizeEx::default().sizes().is_none());
        assert!(
            FsFullSizeEx {
                bytes_per_sector: 0,
                ..info
            }
            .sizes()
            .is_none()
        );
    }

    #[test]
    fn la_clase_7_con_cuota_no_da_residuo_y_sin_cuota_si() {
        // Con cuota: `TotalAllocationUnits` (100) es de la cuota; lo libre real del volumen (400) es mayor que lo que le
        // queda al usuario (60). `total - free` mezclaría ámbitos: se marca fuera del ámbito de volumen (sin residuo) y se
        // enseñan las cifras DEL USUARIO, coherentes entre sí.
        let v = class7(100, 60, 400).sizes().unwrap();
        assert!(!v.volume_scope);
        assert_eq!((v.total, v.free, v.available), (409_600, 245_760, 245_760));
        // Sin cuota: lo disponible para el usuario es lo libre del volumen y `total` es la capacidad.
        let v = class7(1000, 400, 400).sizes().unwrap();
        assert!(v.volume_scope);
        assert_eq!(
            (v.total, v.free, v.available),
            (4_096_000, 1_638_400, 1_638_400)
        );
        // Sin datos.
        assert!(class7(0, 0, 0).sizes().is_none());
        assert!(
            FsFullSize {
                bytes_per_sector: 0,
                ..class7(10, 5, 5)
            }
            .sizes()
            .is_none()
        );
    }

    #[test]
    fn las_consultas_reales_de_nt_son_coherentes() {
        let path = manifest_dir();
        let h = open_dir(&path, FILE_READ_ATTRIBUTES, false).unwrap();
        // La clase 7 existe en todo Windows soportado; la 14 (1809+) también en los equipos de prueba, pero si el sistema
        // no la soporta la consulta falla con un NTSTATUS y la cadena sigue con la 7.
        let v7 = nt_full_size(&h).expect("FileFsFullSizeInformation");
        assert!(
            v7.total > 0 && v7.available <= v7.free && v7.free <= v7.total,
            "{v7:?}"
        );
        if let Ok(v14) = nt_full_size_ex(&h) {
            assert!(v14.volume_scope);
            assert!(v14.total > 0 && v14.available <= v14.free && v14.free <= v14.total);
            // Sin cuota en el equipo, las dos clases coinciden en la capacidad.
            if v7.volume_scope {
                assert_eq!(v7.total, v14.total);
            }
        }
        let w = volume_space(&path).unwrap();
        assert!(w.total > 0 && w.available <= w.free && w.free <= w.total);
    }

    #[test]
    fn la_cadena_de_consultas_elige_la_primera_que_da_ambito_de_volumen() {
        let path = manifest_dir();
        let fail = |_: &[u16]| Err(ERROR_NOT_SUPPORTED);
        let ok14 = VolumeSizes {
            total: 100,
            free: 40,
            available: 10,
            volume_scope: true,
        };
        // La clase 14 manda: ni se consultan las demás.
        assert_eq!(
            volume_space_with(&path, |_| Ok(ok14), |_| panic!("no"), |_| panic!("no")).unwrap(),
            ok14
        );
        // Sin clase 14 (Windows < 1809): la clase 7 con cuota da cifras del usuario y NO ámbito de volumen.
        let v = volume_space_with(
            &path,
            fail,
            |_| Ok(class7(100, 60, 400).sizes().unwrap()),
            |_| panic!("no"),
        )
        .unwrap();
        assert!(!v.volume_scope);
        // Sin clase 14 ni 7: GetDiskFreeSpaceExW solo vale sin cuota.
        let v = volume_space_with(&path, fail, fail, |_| Ok((400, 1000, 400))).unwrap();
        assert_eq!(
            v,
            VolumeSizes {
                total: 1000,
                free: 400,
                available: 400,
                volume_scope: true
            }
        );
        let v = volume_space_with(&path, fail, fail, |_| Ok((100, 1000, 400))).unwrap();
        assert!(!v.volume_scope);
        assert_eq!((v.total, v.free, v.available), (1000, 100, 100));
        assert!(volume_space_with(&path, fail, fail, |_| Ok((0, 0, 0))).is_err());
        assert_eq!(volume_space_with(&path, fail, fail, |_| Err(5)), Err(5));
    }

    #[test]
    fn ex_space_real_da_valores_coherentes() {
        let (avail, total, free) = ex_space(&manifest_dir()).unwrap();
        assert!(total > 0 && avail <= free.max(total));
    }

    /// Carpeta temporal propia (`%TEMP%\tesela-test-<uuid>`) que se borra al soltarla; los junctions se quitan antes como
    /// enlaces (`remove_dir` sobre un junction borra solo el enlace).
    struct Tmp(PathBuf, Vec<PathBuf>);

    impl Tmp {
        fn new() -> Tmp {
            let base = dunce::canonicalize(std::env::temp_dir()).unwrap();
            let p = base.join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&p).unwrap();
            Tmp(p, Vec::new())
        }

        fn junction(&mut self, link: &str, target: &Path) -> PathBuf {
            let l = self.0.join(link);
            let out = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&l)
                .arg(target)
                .output()
                .unwrap();
            assert!(out.status.success(), "mklink /J falló");
            self.1.push(l.clone());
            l
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            for j in &self.1 {
                let _ = std::fs::remove_dir(j);
            }
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn open_resolved_no_sigue_un_junction_ni_siquiera_roto() {
        let mut t = Tmp::new();
        let target = t.0.join("destino");
        std::fs::create_dir(&target).unwrap();
        let link = t.junction("enlace", &target);
        // Con el destino vivo: se abre el ENLACE (atributo REPARSE) y la ruta final es la del enlace, no la del destino.
        let o = open_resolved(&link).unwrap();
        assert_ne!(o.meta.attrs & FILE_ATTRIBUTE_REPARSE_POINT, 0);
        assert_ne!(o.meta.reparse_tag, 0);
        assert_eq!(dunce::simplified(&o.path), link);
        // Con el destino borrado seguir el enlace daría ERROR_FILE_NOT_FOUND / ERROR_PATH_NOT_FOUND; un enlace que no se
        // sigue sigue abriéndose. Es la prueba directa de que el destino no se abre.
        drop(o);
        std::fs::remove_dir(&target).unwrap();
        assert!(std::fs::canonicalize(&link).is_err(), "seguirlo fallaría");
        let o = open_resolved(&link).unwrap();
        assert_ne!(o.meta.attrs & FILE_ATTRIBUTE_REPARSE_POINT, 0);
        assert_eq!(dunce::simplified(&o.path), link);
    }

    #[test]
    fn open_resolved_de_la_raiz_de_una_unidad() {
        // El caso real más común: la raíz de la unidad del propio proyecto.
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let drive = manifest.components().next().unwrap();
        let root = PathBuf::from(format!("{}\\", drive.as_os_str().to_string_lossy()));
        let o = open_resolved(&root).unwrap();
        assert_ne!(o.meta.attrs & 0x10, 0, "es un directorio");
        assert_eq!(o.meta.attrs & FILE_ATTRIBUTE_REPARSE_POINT, 0);
        assert_eq!(dunce::simplified(&o.path), dunce::simplified(&root));
    }

    #[test]
    fn open_resolved_de_una_carpeta_normal_da_su_ruta_canonica() {
        let t = Tmp::new();
        let sub = t.0.join("Sub Carpeta");
        std::fs::create_dir(&sub).unwrap();
        // Con otra capitalización y barras mixtas se obtiene la ruta tal como está en el disco.
        let sloppy =
            PathBuf::from(t.0.to_string_lossy().to_uppercase().replace('\\', "/") + "/sub carpeta");
        let o = open_resolved(&sloppy).unwrap();
        assert_eq!(o.meta.attrs & FILE_ATTRIBUTE_REPARSE_POINT, 0);
        assert_eq!(dunce::simplified(&o.path), sub);
        assert!(open_resolved(&t.0.join("no existe")).is_err());
    }

    /// GM-S1: en el listado extendido el campo de la etiqueta de reparse comparte memoria con `EaSize`; solo vale con el
    /// atributo `REPARSE_POINT`.
    #[test]
    fn la_etiqueta_de_reparse_solo_vale_con_el_atributo() {
        // Un directorio o archivo normal con atributos extendidos (`EaSize` = 0x9000_001A): no es una etiqueta de nube.
        assert_eq!(entry_reparse_tag(0x10, 0x9000_001A), 0);
        assert_eq!(entry_reparse_tag(0x20, 0xA000_0003), 0);
        assert_eq!(entry_reparse_tag(0, 7), 0);
        // Con el atributo, la etiqueta tal cual.
        assert_eq!(
            entry_reparse_tag(0x10 | FILE_ATTRIBUTE_REPARSE_POINT, 0xA000_0003),
            0xA000_0003
        );
        assert_eq!(entry_reparse_tag(FILE_ATTRIBUTE_REPARSE_POINT, 0), 0);
    }

    #[test]
    fn el_listado_real_no_da_etiqueta_a_lo_que_no_es_reparse_point() {
        let mut t = Tmp::new();
        std::fs::create_dir(t.0.join("real")).unwrap();
        std::fs::write(t.0.join("a.txt"), b"x").unwrap();
        let target = t.0.join("real");
        t.junction("enlace", &target);
        let h = open_dir(&wide(&t.0), FILE_LIST_DIRECTORY, false).unwrap();
        let mut buf = vec![0u64; BUF_BYTES / 8];
        let mut seen = 0;
        enum_dir_extd(&h, &mut buf, &mut |e| {
            seen += 1;
            if e.attrs & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
                assert_eq!(e.reparse_tag, 0, "{}", String::from_utf16_lossy(e.name));
            } else {
                assert_eq!(
                    e.reparse_tag, 0xA000_0003,
                    "un junction es un punto de montaje"
                );
            }
        })
        .unwrap();
        assert_eq!(seen, 3);
    }

    #[test]
    fn find_entry_named_da_el_nombre_exacto_del_disco() {
        let t = Tmp::new();
        std::fs::create_dir(t.0.join("Carpeta Mixta")).unwrap();
        let (name, info) = find_entry_named(&wide(&t.0), &wide(Path::new("CARPETA mixta")))
            .unwrap()
            .expect("la encuentra sin distinguir mayúsculas");
        assert_eq!(String::from_utf16_lossy(&name), "Carpeta Mixta");
        assert_ne!(info.attrs & 0x10, 0);
        assert!(
            find_entry_named(&wide(&t.0), &wide(Path::new("otra")))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn long_path_conserva_una_ruta_larga_y_falla_si_no_existe() {
        let t = Tmp::new();
        let sub = t.0.join("Carpeta con nombre largo de verdad");
        std::fs::create_dir(&sub).unwrap();
        let long = long_path(&sub).unwrap();
        assert_eq!(dunce::simplified(&long), sub);
        // La carpeta temporal tal como la da el entorno (a veces en 8.3) también se expande.
        assert!(long_path(&std::env::temp_dir()).is_ok());
        assert_eq!(long_path(&t.0.join("no existe")), Err(2));
    }
}
