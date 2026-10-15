//! Llamadas a la API de Windows para listar directorios y consultar volúmenes. SOLO LECTURA.
//!
//! Todo el `unsafe` del enumerador está en este módulo, en funciones pequeñas con su `// SAFETY:`. Los tests de abajo
//! (y los de integración del escáner) pasan por cada una de ellas. Nada abre archivos con acceso de escritura.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_DATA, ERROR_NO_MORE_FILES, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_EXTD_DIR_INFO, FILE_ID_INFO, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FIND_FIRST_EX_LARGE_FETCH,
    FileBasicInfo, FileIdExtdDirectoryInfo, FileIdInfo, FileStandardInfo, FindClose,
    FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, GetDiskFreeSpaceExW,
    GetDiskFreeSpaceW, GetFileInformationByHandleEx, GetVolumeInformationByHandleW, OPEN_EXISTING,
    WIN32_FIND_DATAW,
};

pub use windows_sys::Win32::Storage::FileSystem::{FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES};

/// Tamaño (en bytes) del búfer de enumeración por hilo.
pub const BUF_BYTES: usize = 64 * 1024;

/// Convierte una ruta en UTF-16 sin NUL final.
pub fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().collect()
}

fn with_nul(path: &[u16]) -> Vec<u16> {
    let mut v = Vec::with_capacity(path.len() + 1);
    v.extend_from_slice(path);
    v.push(0);
    v
}

/// Handle de directorio que se cierra al soltarlo.
pub struct DirHandle(HANDLE);

// SAFETY: un HANDLE de archivo del kernel se puede usar y cerrar desde cualquier hilo; aquí además cada `DirHandle`
// pertenece a un solo hilo a la vez.
unsafe impl Send for DirHandle {}

impl Drop for DirHandle {
    fn drop(&mut self) {
        // SAFETY: `self.0` es un handle válido que abrió `open_dir` y que solo se cierra aquí, una vez.
        unsafe { CloseHandle(self.0) };
    }
}

/// Abre un directorio (o un volumen raíz) en SOLO LECTURA: `access` es `FILE_LIST_DIRECTORY` o
/// `FILE_READ_ATTRIBUTES`; se comparte con todos (lectura, escritura y borrado) para no estorbar a nadie, y se abre el
/// propio reparse point en vez de seguirlo. Devuelve el código de error de Windows si falla.
pub fn open_dir(path: &[u16], access: u32) -> Result<DirHandle, u32> {
    let p = with_nul(path);
    // SAFETY: `p` es una cadena UTF-16 terminada en NUL que vive durante la llamada; los demás argumentos son
    // constantes o punteros nulos permitidos por la API (sin atributos de seguridad ni plantilla).
    let h = unsafe {
        CreateFileW(
            p.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        // SAFETY: `GetLastError` no tiene precondiciones.
        return Err(unsafe { GetLastError() });
    }
    Ok(DirHandle(h))
}

/// Metadatos de un directorio abierto con `FILE_READ_ATTRIBUTES`.
#[derive(Clone, Copy, Debug)]
pub struct DirMeta {
    /// Solo lo usan los tests: comprueba que `FileIdInfo` coincide con el `FileId` del listado del padre.
    #[allow(dead_code)]
    pub file_id: u128,
    pub attrs: u32,
    pub mtime_filetime: i64,
    pub alloc: u64,
    pub logical: u64,
}

/// Lee `FILE_BASIC_INFO`, `FILE_STANDARD_INFO` y `FILE_ID_INFO` del handle.
pub fn dir_meta(h: &DirHandle) -> Result<DirMeta, u32> {
    fn query<T>(h: &DirHandle, class: i32) -> Result<T, u32> {
        // SAFETY: `T` es una de las estructuras POD de la API (enteros y arrays de bytes), para las que todo-ceros es
        // un valor válido.
        let mut v: T = unsafe { std::mem::zeroed() };
        // SAFETY: `h.0` es un handle válido; el puntero apunta a `v` y se pasa su tamaño exacto.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                h.0,
                class,
                &mut v as *mut T as *mut c_void,
                size_of::<T>() as u32,
            )
        };
        if ok == 0 {
            // SAFETY: `GetLastError` no tiene precondiciones.
            return Err(unsafe { GetLastError() });
        }
        Ok(v)
    }
    let basic: FILE_BASIC_INFO = query(h, FileBasicInfo)?;
    let standard: FILE_STANDARD_INFO = query(h, FileStandardInfo)?;
    let id: FILE_ID_INFO = query(h, FileIdInfo)?;
    Ok(DirMeta {
        file_id: u128::from_le_bytes(id.FileId.Identifier),
        attrs: basic.FileAttributes,
        mtime_filetime: basic.LastWriteTime,
        alloc: standard.AllocationSize.max(0) as u64,
        logical: standard.EndOfFile.max(0) as u64,
    })
}

/// Datos de una entrada de directorio tal como los da el listado de su padre.
#[derive(Clone, Copy, Debug)]
pub struct EntryInfo {
    pub attrs: u32,
    pub logical: u64,
    pub alloc: u64,
    pub file_id: u128,
    pub reparse_tag: u32,
    pub mtime_filetime: i64,
}

/// Busca `name` (sin distinguir mayúsculas) en el listado de `parent` y devuelve los datos que el listado da de esa
/// entrada. Se usa para precrear los ancestros de las rutas prioritarias con EXACTAMENTE los mismos valores que les
/// daría el recorrido general: el tamaño propio de un directorio que da el listado de su padre puede diferir del que
/// da `FileStandardInfo` sobre el directorio abierto, y sus `FileId` son los que se comparan al adoptar.
pub fn find_entry(parent: &[u16], name: &[u16]) -> Result<Option<EntryInfo>, u32> {
    let h = open_dir(parent, FILE_LIST_DIRECTORY)?;
    let mut buf = vec![0u64; BUF_BYTES / 8];
    let wanted = String::from_utf16_lossy(name).to_lowercase();
    let mut found: Option<EntryInfo> = None;
    enum_dir_extd(&h, &mut buf, &mut |e| {
        let exact = e.name == name;
        if found.is_none() && (exact || String::from_utf16_lossy(e.name).to_lowercase() == wanted) {
            found = Some(EntryInfo {
                attrs: e.attrs,
                logical: e.logical,
                alloc: e.alloc.unwrap_or(0),
                file_id: e.file_id,
                reparse_tag: e.reparse_tag,
                mtime_filetime: e.mtime_filetime,
            });
        }
    })?;
    Ok(found)
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
        // SAFETY: `h.0` es un handle de directorio válido; `buf` es memoria escribible de `buf_len` bytes y está
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
            // SAFETY: `GetLastError` no tiene precondiciones.
            let e = unsafe { GetLastError() };
            return if e == ERROR_NO_MORE_FILES {
                Ok(())
            } else {
                Err(e)
            };
        }
        // SAFETY: `buf` es memoria inicializada de `buf_len` bytes (un `[u64]` entero) y no se modifica mientras
        // viva esta vista (la siguiente llamada a la API solo ocurre después de soltarla).
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
                    reparse_tag: read_u32(rec, TAG),
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

/// Respaldo: `FindFirstFileExW(FindExInfoBasic, LARGE_FETCH)`. No da `AllocationSize` ni `FileId`.
pub fn enum_dir_find(path: &[u16], emit: &mut dyn FnMut(&RawEntry)) -> Result<(), u32> {
    let mut pat = Vec::with_capacity(path.len() + 3);
    pat.extend_from_slice(path);
    if pat.last() != Some(&(b'\\' as u16)) {
        pat.push(b'\\' as u16);
    }
    pat.push(b'*' as u16);
    pat.push(0);
    // SAFETY: WIN32_FIND_DATAW es un POD (enteros y arrays de u16): todo-ceros es válido.
    let mut fd: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    // SAFETY: `pat` termina en NUL; `fd` es escribible y del tipo que espera `FindExInfoBasic`; los demás
    // argumentos son los valores documentados para una búsqueda sin filtros.
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
        // SAFETY: `GetLastError` no tiene precondiciones.
        return Err(unsafe { GetLastError() });
    }
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
        // SAFETY: `h` es el handle de búsqueda válido que devolvió `FindFirstFileExW`; `fd` es escribible.
        if unsafe { FindNextFileW(h, &mut fd) } == 0 {
            // SAFETY: `GetLastError` no tiene precondiciones.
            let e = unsafe { GetLastError() };
            // SAFETY: `h` es válido y se cierra una única vez, justo aquí.
            unsafe { FindClose(h) };
            return if e == ERROR_NO_MORE_FILES {
                Ok(())
            } else {
                Err(e)
            };
        }
    }
}

/// Espacio de un volumen: `(total, libre)` en bytes (`lpTotalNumberOfFreeBytes`, no el libre del usuario con cuota).
pub fn volume_space(path: &[u16]) -> Result<(u64, u64), u32> {
    let p = with_nul(path);
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: `p` termina en NUL y los tres punteros apuntan a variables locales escribibles.
    let ok = unsafe { GetDiskFreeSpaceExW(p.as_ptr(), &mut avail, &mut total, &mut free) };
    if ok == 0 {
        // SAFETY: `GetLastError` no tiene precondiciones.
        return Err(unsafe { GetLastError() });
    }
    Ok((total, free))
}

/// Bytes por cluster del volumen que contiene `path` (para estimar `alloc` cuando solo hay `FindFirstFileExW`).
pub fn cluster_bytes(path: &[u16]) -> Option<u64> {
    let p = with_nul(path);
    let (mut spc, mut bps, mut free, mut total) = (0u32, 0u32, 0u32, 0u32);
    // SAFETY: `p` termina en NUL y los cuatro punteros apuntan a variables locales escribibles.
    let ok = unsafe { GetDiskFreeSpaceW(p.as_ptr(), &mut spc, &mut bps, &mut free, &mut total) };
    (ok != 0 && spc != 0 && bps != 0).then(|| spc as u64 * bps as u64)
}

/// Nombre del sistema de archivos del volumen del handle (p. ej. `NTFS`, `ReFS`, `FAT32`).
pub fn fs_name(h: &DirHandle) -> Result<String, u32> {
    let mut name = [0u16; 64];
    // SAFETY: `h.0` es un handle válido; `name` es escribible y se pasa su longitud en unidades UTF-16; los punteros
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
        // SAFETY: `GetLastError` no tiene precondiciones.
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
        let h = open_dir(path, FILE_LIST_DIRECTORY).expect("abre");
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
        assert!(matches!(open_dir(&p, FILE_LIST_DIRECTORY), Err(2 | 3)));
    }

    #[test]
    fn meta_del_directorio_coincide_con_su_listado() {
        let path = manifest_dir();
        let h = open_dir(&path, FILE_READ_ATTRIBUTES).unwrap();
        let m = dir_meta(&h).unwrap();
        assert_ne!(m.file_id, 0);
        assert!(m.attrs & 0x10 != 0, "es un directorio");
        assert!(m.mtime_filetime > 0);
        // El FileId de `src` visto desde el padre es el mismo que da dir_meta al abrirlo.
        let listed = collect_extd(&path);
        let mut src = path.clone();
        src.extend(r"\src".encode_utf16());
        let hs = open_dir(&src, FILE_READ_ATTRIBUTES).unwrap();
        let ms = dir_meta(&hs).unwrap();
        assert_eq!(listed["src"].2, ms.file_id);
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
        let (total, free) = volume_space(&path).unwrap();
        assert!(total > 0 && free <= total);
        let c = cluster_bytes(&path).unwrap();
        assert!(c.is_power_of_two() && c >= 512);
        let h = open_dir(&path, FILE_READ_ATTRIBUTES).unwrap();
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
}
