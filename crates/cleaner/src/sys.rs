//! Windows layer for `cleaner`: open objects by handle, identify, delete, and rename THROUGH THE HANDLE.
//!
//! Es el único módulo del crate con `unsafe` junto a `recycle` (COM) y `process` (lista de procesos). Cada llamada
//! es una función pequeña con su `// SAFETY:` y el módulo de pruebas las recorre todas sobre un árbol temporal.
//!
//! # Principle (PLAN.md §3.7)
//! Verify and act on the SAME handle: never "check by path and delete by path".
//! * [`open_path`] abre por ruta SOLO para capturar una identidad o abrir la raíz de una categoría: con
//!   `FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS`, así que un enlace se abre a sí mismo, no su destino.
//! * [`open_rel`] es la apertura relativa a un handle (`NtCreateFile` con `RootDirectory`): ninguna ruta se vuelve a
//!   resolver desde la raíz, de modo que cambiar un ancestro por un junction no redirige la operación.
//! * [`open_file_exclusive`]: acceso `DELETE` con `dwShareMode = 0`. La apertura exclusiva es a la vez la comprobación de
//!   "en uso" (`ERROR_SHARING_VIOLATION`), cierra el caso `FILE_SHARE_DELETE` y evita la sustitución entre comprobar y
//!   actuar.
//! * [`delete_on_handle`]: `FileDispositionInfoEx` (DELETE | POSIX_SEMANTICS | IGNORE_READONLY_ATTRIBUTE) sobre el
//!   handle abierto; en sistemas de archivos sin POSIX cae a `FileDispositionInfo` (borra al cerrar el handle).
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
    FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_FILE_TOO_LARGE, ERROR_INVALID_DATA, ERROR_INVALID_FUNCTION,
    ERROR_INVALID_PARAMETER, ERROR_IO_INCOMPLETE, ERROR_NO_MORE_FILES, ERROR_NOT_SUPPORTED,
    ERROR_OPERATION_ABORTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE, RtlNtStatusToDosError,
    UNICODE_STRING, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ATTRIBUTE_TAG_INFO, FILE_BASIC_INFO, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_OVERLAPPED, FILE_ID_EXTD_DIR_INFO, FILE_ID_INFO,
    FILE_LIST_DIRECTORY, FILE_NAME_NORMALIZED, FILE_NOTIFY_CHANGE_DIR_NAME,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
    FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_STANDARD_INFO, FileAttributeTagInfo, FileBasicInfo, FileDispositionInfo,
    FileDispositionInfoEx, FileIdExtdDirectoryInfo, FileIdInfo, FileStandardInfo,
    GetFileInformationByHandleEx, GetFinalPathNameByHandleW, OPEN_EXISTING, ReadDirectoryChangesW,
    ReadFile, SetFileInformationByHandle, VOLUME_NAME_DOS,
};
use windows_sys::Win32::System::IO::{
    CancelIoEx, GetOverlappedResult, IO_STATUS_BLOCK, OVERLAPPED,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcessToken, WaitForSingleObject,
};

/// ¿Se ejecuta ESTE proceso con privilegios elevados (el token tiene `TokenElevation`)? `None` si no se pudo consultar. El
/// manifiesto es `asInvoker`: un proceso elevado que lance Tesela se lo pasa al hijo.
pub fn process_elevation() -> Option<bool> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `token` es escribible; `GetCurrentProcess` devuelve un pseudo-handle que no hay que cerrar.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    // Se cierra al soltarlo, pase lo que pase.
    let token = Handle(token);
    // SAFETY: `TOKEN_ELEVATION` es un POD (un entero): todo-ceros es válido.
    let mut elevation: TOKEN_ELEVATION = unsafe { std::mem::zeroed() };
    let mut returned = 0u32;
    // SAFETY: `token.0` es un token válido abierto con `TOKEN_QUERY`; el puntero apunta a `elevation` y se pasa su tamaño
    // exacto; `returned` es escribible.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            &mut elevation as *mut TOKEN_ELEVATION as *mut c_void,
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    };
    (ok != 0).then_some(elevation.TokenIsElevated != 0)
}

/// Código de error de Windows (`GetLastError` / `RtlNtStatusToDosError`).
pub type Win32 = u32;

/// `ERROR_SHARING_VIOLATION`: alguien tiene el objeto abierto sin compartir lo que pedimos.
pub const SHARING_VIOLATION: Win32 = 32;
/// `ERROR_ACCESS_DENIED`.
pub const ACCESS_DENIED: Win32 = 5;
/// `ERROR_DIR_NOT_EMPTY`.
pub const DIR_NOT_EMPTY: Win32 = 145;

const SYNCHRONIZE: u32 = 0x0010_0000;
const FILE_SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
/// Tamaño del búfer de enumeración (64 KiB, como el escáner).
const BUF_BYTES: usize = 64 * 1024;

/// Identidad de un objeto del sistema de archivos: número de serie del volumen + `FileId` de 128 bits (`FileIdInfo`).
/// Es lo que aprueba el usuario al confirmar; las rutas solo sirven para volver a encontrar el objeto.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    pub volume_serial: u64,
    pub file_id: u128,
}

/// Handle del kernel que se cierra al soltarlo.
pub struct Handle(HANDLE);

// SAFETY: un HANDLE de archivo se puede usar y cerrar desde cualquier hilo. Varios hilos pueden abrir hijos relativos
// al mismo handle de directorio a la vez (`NtCreateFile` con `RootDirectory`); la enumeración y el borrado se hacen
// desde un solo hilo por handle (lo garantiza el motor).
unsafe impl Send for Handle {}
// SAFETY: ver arriba; las llamadas del kernel sobre un mismo handle están serializadas por el propio sistema.
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: `self.0` es un handle válido que abrió este módulo y solo se cierra aquí, una vez.
        unsafe { CloseHandle(self.0) };
    }
}

/// Metadatos de un objeto abierto: identidad, enlaces, atributos, tamaños y la fecha más reciente de las tres que
/// pueden delatar actividad (creación, escritura, cambio de metadatos).
#[derive(Clone, Copy, Debug)]
pub struct Meta {
    pub identity: FileIdentity,
    /// `NumberOfLinks` (`FileStandardInfo`).
    pub links: u32,
    pub attrs: u32,
    /// Etiqueta de reparse; 0 si no es un reparse point.
    pub reparse_tag: u32,
    pub is_dir: bool,
    /// `FileStandardInfo.DeletePending`: alguien ya marcó el objeto para borrar y desaparecerá al cerrarse el último handle
    /// (hasta entonces el NOMBRE sigue visible y abrirlo da acceso denegado). Es la forma de saber, con un handle abierto,
    /// que un objeto ya se eliminó.
    pub delete_pending: bool,
    pub alloc: u64,
    pub logical: u64,
    /// El mayor de `CreationTime`, `LastWriteTime` y `ChangeTime` (FILETIME).
    pub newest_filetime: i64,
}

impl Meta {
    pub fn is_reparse(&self) -> bool {
        self.attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
}

/// Entrada de un directorio tal como la da su listado (`FileIdExtdDirectoryInfo`).
#[derive(Clone, Debug)]
pub struct DirEntry {
    /// Nombre en UTF-16, tal cual lo guarda el sistema.
    pub name: Box<[u16]>,
    pub attrs: u32,
    pub reparse_tag: u32,
    pub alloc: u64,
    pub logical: u64,
    /// `FileId` de 128 bits que da el listado (se compara con el del handle al abrirlo).
    pub file_id: u128,
    pub newest_filetime: i64,
}

impl DirEntry {
    pub fn is_dir(&self) -> bool {
        self.attrs & FILE_ATTRIBUTE_DIRECTORY != 0
    }
    pub fn is_reparse(&self) -> bool {
        self.attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
}

/// `true` si la etiqueta de reparse es la de un archivo de la nube (OneDrive y similares: `IO_REPARSE_TAG_CLOUD*`).
pub fn is_cloud_tag(tag: u32) -> bool {
    tag & !0xF000 == 0x9000_001A
}

/// Ruta en UTF-16 con NUL final y prefijo `\\?\` (rutas largas, sin normalización de Win32: por eso las `/` se
/// convierten antes en `\`).
fn wide_long(path: &Path) -> Vec<u16> {
    let mut v: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .map(|c| if c == b'/' as u16 { b'\\' as u16 } else { c })
        .collect();
    let verbatim = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    if !v.starts_with(&verbatim) {
        if v.starts_with(&[b'\\' as u16, b'\\' as u16]) {
            // Forma UNC (dos barras, servidor y recurso) -> forma `verbatim` con `UNC`.
            let mut out: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
            out.extend_from_slice(&v[2..]);
            v = out;
        } else {
            let mut out = verbatim.to_vec();
            out.extend_from_slice(&v);
            v = out;
        }
    }
    v.push(0);
    v
}

fn last_error() -> Win32 {
    // SAFETY: `GetLastError` no tiene precondiciones.
    unsafe { GetLastError() }
}

fn nt_to_win32(status: i32) -> Win32 {
    // SAFETY: `RtlNtStatusToDosError` es una función pura de traducción, sin precondiciones.
    unsafe { RtlNtStatusToDosError(status) }
}

/// Abre `path` (por ruta) sin seguir un reparse point final. `access` y `share` son los de `CreateFileW`.
/// Para capturar identidades y abrir raíces de categoría; las operaciones destructivas usan [`open_rel`] o
/// [`open_file_exclusive`] y verifican la identidad del handle.
pub fn open_path(path: &Path, access: u32, share: u32) -> Result<Handle, Win32> {
    let p = wide_long(path);
    // SAFETY: `p` es una cadena UTF-16 terminada en NUL que vive durante la llamada; el resto de argumentos son
    // constantes o punteros nulos permitidos (sin atributos de seguridad ni plantilla).
    let h = unsafe {
        CreateFileW(
            p.as_ptr(),
            access,
            share,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    Ok(Handle(h))
}

/// Abre `path` solo para consultar atributos e identidad (lo comparte todo: no estorba a nadie).
pub fn open_for_query(path: &Path) -> Result<Handle, Win32> {
    open_path(path, FILE_READ_ATTRIBUTES, FILE_SHARE_ALL)
}

/// Abre un directorio para listarlo y consultarlo (compartiendo todo). Sin `DELETE`: no se puede borrar con él.
pub fn open_dir_for_listing(path: &Path) -> Result<Handle, Win32> {
    open_path(
        path,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
        FILE_SHARE_ALL,
    )
}

/// Qué se espera encontrar al abrir una entrada relativa.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    File,
    Dir,
}

/// Apertura RELATIVA a `parent` (`NtCreateFile` con `RootDirectory`): `name` es un nombre de entrada, sin separadores.
/// Siempre `FILE_OPEN_REPARSE_POINT` (un enlace se abre a sí mismo) y con el tipo esperado impuesto por el sistema
/// (`FILE_DIRECTORY_FILE` / `FILE_NON_DIRECTORY_FILE`): si el tipo cambió desde el listado, falla.
fn open_rel(
    parent: &Handle,
    name: &[u16],
    access: u32,
    share: u32,
    expect: Expect,
) -> Result<Handle, Win32> {
    // Un nombre con separadores o vacío no es un nombre de entrada: se rechaza antes de llegar al kernel. Tampoco con `:`:
    // en NTFS abre un flujo de datos alternativo (`archivo:flujo`) o un atributo (`carpeta::$INDEX_ALLOCATION`), no la
    // entrada (H6.10).
    if name.is_empty()
        || name
            .iter()
            .any(|&c| c == b'\\' as u16 || c == b'/' as u16 || c == b':' as u16 || c == 0)
        || name.len() > (u16::MAX as usize) / 2
    {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let bytes = (name.len() * 2) as u16;
    let us = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: name.as_ptr() as *mut u16,
    };
    let oa = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.0,
        ObjectName: &us,
        Attributes: 0,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let type_opt = match expect {
        Expect::File => FILE_NON_DIRECTORY_FILE,
        Expect::Dir => FILE_DIRECTORY_FILE,
    };
    let mut h: HANDLE = std::ptr::null_mut();
    // SAFETY: IO_STATUS_BLOCK es un POD (entero y puntero): todo-ceros es un valor válido.
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: `oa` apunta a `us` y `name`, que viven durante la llamada; `parent.0` es un handle de directorio válido
    // (lo posee `parent`); `h` e `iosb` son escribibles; no hay EA ni tamaño de asignación.
    let status = unsafe {
        NtCreateFile(
            &mut h,
            access | SYNCHRONIZE,
            &oa,
            &mut iosb,
            std::ptr::null(),
            0,
            share,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT | type_opt,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        return Err(nt_to_win32(status));
    }
    Ok(Handle(h))
}

/// Abre un ARCHIVO hijo de `parent` en exclusiva para borrarlo: `DELETE`, `dwShareMode = 0`. Falla con
/// [`SHARING_VIOLATION`] si alguien lo tiene abierto (también con `FILE_SHARE_DELETE`).
pub fn open_file_exclusive(parent: &Handle, name: &[u16]) -> Result<Handle, Win32> {
    open_rel(parent, name, DELETE | FILE_READ_ATTRIBUTES, 0, Expect::File)
}

/// Abre un DIRECTORIO hijo de `parent` para listarlo y, al final, borrarlo (vacío) por este mismo handle. Se comparte
/// todo para poder enumerar aunque el Explorador o un antivirus lo estén observando; la identidad se verifica igual.
pub fn open_dir_for_delete(parent: &Handle, name: &[u16]) -> Result<Handle, Win32> {
    open_rel(
        parent,
        name,
        DELETE | FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
        FILE_SHARE_ALL,
        Expect::Dir,
    )
}

/// Abre un hijo (archivo o directorio) solo para consultar sus metadatos, compartiéndolo todo.
pub fn open_child_for_query(
    parent: &Handle,
    name: &[u16],
    expect: Expect,
) -> Result<Handle, Win32> {
    open_rel(parent, name, FILE_READ_ATTRIBUTES, FILE_SHARE_ALL, expect)
}

/// Abre un directorio hijo solo para listarlo (sin `DELETE`), compartiéndolo todo.
pub fn open_child_dir_for_listing(parent: &Handle, name: &[u16]) -> Result<Handle, Win32> {
    open_rel(
        parent,
        name,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
        FILE_SHARE_ALL,
        Expect::Dir,
    )
}

/// Por qué falla [`open_reparse_free_dir`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChainError {
    /// La ruta no es absoluta con letra de unidad y componentes normales (UNC, relativa, `..`, `\\.\`...).
    BadPath,
    /// El componente `component` (nombre tal como se escribió en la ruta) es un reparse point (junction, symlink, punto de
    /// montaje, nube): la cadena no es de confianza.
    Reparse { component: String },
    /// Error del sistema al abrir un componente (o al leer su identidad).
    Os(Win32),
}

/// Abre el directorio `path` recorriendo la cadena COMPONENTE A COMPONENTE desde la raíz del volumen, con aperturas
/// RELATIVAS (`NtCreateFile` + `RootDirectory` + `FILE_OPEN_REPARSE_POINT`), y exige que NINGÚN componente (ni la raíz de
/// la unidad ni el último) sea un reparse point. Devuelve el handle del último.
///
/// Es la comprobación de las raíces de categoría (PLAN.md §3.7.3): `GetFinalPathNameByHandleW` resuelve en silencio los
/// enlaces de los ANCESTROS, así que comparar la ruta canónica con la léxica no basta para ver un junction por encima de
/// `User Data` o de `%TEMP%`. Hay que mirar cada componente de la ruta TAL COMO SE ESCRIBIÓ (los nombres 8.3 de `%TEMP%`
/// valen: los resuelve el sistema de archivos al abrir).
pub fn open_reparse_free_dir(path: &Path) -> Result<Handle, ChainError> {
    open_chain_dir(path, |_| false)
}

/// Como [`open_reparse_free_dir`], pero un componente que es un reparse point SE ACEPTA si `tolerate` lo permite (se le pasan
/// sus metadatos). La identidad de cada salto se sigue verificando al abrir relativo al anterior. Lo usa la aprobación de la
/// cesta para dejar pasar las carpetas de sincronización de la nube (M1): un placeholder de nube no redirige la resolución de
/// nombres; junction, symlink y punto de montaje sí y se siguen rechazando.
pub fn open_chain_dir(path: &Path, tolerate: impl Fn(&Meta) -> bool) -> Result<Handle, ChainError> {
    use std::path::{Component, Prefix};
    let plain = strip_verbatim(path.to_path_buf());
    let mut comps = plain.components();
    let drive = match comps.next() {
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::Disk(l) | Prefix::VerbatimDisk(l) => l,
            _ => return Err(ChainError::BadPath),
        },
        _ => return Err(ChainError::BadPath),
    };
    if !matches!(comps.next(), Some(Component::RootDir)) {
        return Err(ChainError::BadPath);
    }
    let root = PathBuf::from(format!("{}:\\", drive as char));
    // Solo se piden atributos (no `FILE_LIST_DIRECTORY`): hay carpetas que un usuario sin elevar no puede listar
    // (`Windows\Temp`) y la cadena no necesita listar nada.
    let mut cur = open_for_query(&root).map_err(ChainError::Os)?;
    let check = |h: &Handle, name: &str| -> Result<(), ChainError> {
        let m = meta(h).map_err(ChainError::Os)?;
        if m.is_reparse() && !tolerate(&m) {
            return Err(ChainError::Reparse {
                component: name.to_string(),
            });
        }
        Ok(())
    };
    check(&cur, &root.display().to_string())?;
    for c in comps {
        let Component::Normal(name) = c else {
            return Err(ChainError::BadPath);
        };
        let wide: Vec<u16> = name.encode_wide().collect();
        let next = open_child_for_query(&cur, &wide, Expect::Dir).map_err(ChainError::Os)?;
        check(&next, &name.to_string_lossy())?;
        cur = next;
    }
    Ok(cur)
}

/// Abre por ruta con acceso `DELETE` y compartiendo todo: comprobación previa de un elemento que va a la Papelera (se
/// captura la identidad y se SUELTA el handle antes de entregar la ruta a la shell).
pub fn open_path_for_rename(path: &Path) -> Result<Handle, Win32> {
    open_path(path, DELETE | FILE_READ_ATTRIBUTES, FILE_SHARE_ALL)
}

/// Abre por ruta un ARCHIVO en exclusiva con acceso `DELETE` y `dwShareMode = 0` (borrado definitivo de un elemento de
/// la cesta que es un archivo).
pub fn open_path_exclusive(path: &Path) -> Result<Handle, Win32> {
    open_path(path, DELETE | FILE_READ_ATTRIBUTES, 0)
}

/// Abre por ruta un archivo en exclusiva (`dwShareMode = 0`) para LEERLO y borrarlo por el mismo handle.
pub fn open_path_exclusive_read(path: &Path) -> Result<Handle, Win32> {
    open_path(path, DELETE | FILE_READ_ATTRIBUTES | FILE_READ_DATA, 0)
}

/// Lee el contenido de un archivo pequeño (hasta `max` bytes) por un handle abierto con `FILE_READ_DATA`. Falla con
/// `ERROR_FILE_TOO_LARGE` si hay más de `max` bytes.
pub fn read_small(h: &Handle, max: usize) -> Result<Vec<u8>, Win32> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let mut got = 0u32;
        // SAFETY: `h.0` es un handle válido abierto con `FILE_READ_DATA`; `buf` es escribible con `buf.len()` bytes y
        // `got` también; no hay estructura OVERLAPPED (lectura síncrona).
        let ok = unsafe {
            ReadFile(
                h.0,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                &mut got,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_error());
        }
        if got == 0 {
            return Ok(out);
        }
        if out.len() + got as usize > max {
            return Err(ERROR_FILE_TOO_LARGE);
        }
        out.extend_from_slice(&buf[..got as usize]);
    }
}

/// Abre por ruta un DIRECTORIO para listarlo y borrarlo (vacío) por este mismo handle.
pub fn open_path_dir_for_delete(path: &Path) -> Result<Handle, Win32> {
    open_path(
        path,
        DELETE | FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
        FILE_SHARE_ALL,
    )
}

fn query_info<T>(h: &Handle, class: i32) -> Result<T, Win32> {
    // SAFETY: `T` es una de las estructuras POD de la API (enteros y arrays de bytes): todo-ceros es válido.
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
        return Err(last_error());
    }
    Ok(v)
}

/// Metadatos del objeto abierto. `FileIdInfo` es OBLIGATORIO (sin identidad no se aprueba nada): si el sistema de
/// archivos no lo soporta (FAT, exFAT) devuelve su error y el elemento no es elegible.
pub fn meta(h: &Handle) -> Result<Meta, Win32> {
    let basic: FILE_BASIC_INFO = query_info(h, FileBasicInfo)?;
    let std_info: FILE_STANDARD_INFO = query_info(h, FileStandardInfo)?;
    let id: FILE_ID_INFO = query_info(h, FileIdInfo)?;
    let reparse_tag = if basic.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        query_info::<FILE_ATTRIBUTE_TAG_INFO>(h, FileAttributeTagInfo)
            .map(|t| t.ReparseTag)
            .unwrap_or(0)
    } else {
        0
    };
    Ok(Meta {
        identity: FileIdentity {
            volume_serial: id.VolumeSerialNumber,
            file_id: u128::from_le_bytes(id.FileId.Identifier),
        },
        links: std_info.NumberOfLinks,
        attrs: basic.FileAttributes,
        reparse_tag,
        is_dir: std_info.Directory,
        delete_pending: std_info.DeletePending,
        alloc: std_info.AllocationSize.max(0) as u64,
        logical: std_info.EndOfFile.max(0) as u64,
        newest_filetime: basic
            .CreationTime
            .max(basic.LastWriteTime)
            .max(basic.ChangeTime),
    })
}

/// Lo que ocupa AHORA el objeto abierto, leído por su handle (solo `FileStandardInfo`: una consulta, sin reabrir la ruta).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sizes {
    /// `AllocationSize`: bytes en disco.
    pub alloc: u64,
    /// `EndOfFile`: tamaño lógico.
    pub logical: u64,
}

impl Sizes {
    /// El mayor de los dos tamaños: lo que se cuenta contra el presupuesto de la Papelera.
    pub fn charge(&self) -> u64 {
        self.alloc.max(self.logical)
    }
}

/// Tamaños actuales del objeto abierto (A2-B1: el receptor de la shell vuelve a medir cada archivo por su handle de guarda justo
/// antes de que la shell lo toque).
pub fn sizes(h: &Handle) -> Result<Sizes, Win32> {
    let std_info: FILE_STANDARD_INFO = query_info(h, FileStandardInfo)?;
    Ok(Sizes {
        alloc: std_info.AllocationSize.max(0) as u64,
        logical: std_info.EndOfFile.max(0) as u64,
    })
}

/// Ruta canónica LARGA del objeto abierto (`GetFinalPathNameByHandleW`, normalizada y sin `\\?\`). Resuelve los
/// ancestros y los nombres 8.3, y para un enlace abierto con `OPEN_REPARSE_POINT` es la ruta del propio enlace.
pub fn final_path(h: &Handle) -> Result<PathBuf, Win32> {
    let mut buf = vec![0u16; 512];
    loop {
        // SAFETY: `h.0` es un handle válido; `buf` es escribible con `buf.len()` unidades UTF-16.
        let n = unsafe {
            GetFinalPathNameByHandleW(
                h.0,
                buf.as_mut_ptr(),
                buf.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        if n == 0 {
            return Err(last_error());
        }
        if (n as usize) < buf.len() {
            buf.truncate(n as usize);
            break;
        }
        buf = vec![0u16; n as usize + 1];
    }
    Ok(strip_verbatim(std::ffi::OsString::from_wide(&buf).into()))
}

/// Quita el prefijo `verbatim` (`\\?\C:\x` -> `C:\x`; la forma con `UNC` vuelve a la de dos barras).
///
/// Trabaja sobre UTF-16: un nombre con unidades sueltas (válido en NTFS) pasa tal cual, sin `U+FFFD`.
pub fn strip_verbatim(p: PathBuf) -> PathBuf {
    let w: Vec<u16> = p.as_os_str().encode_wide().collect();
    let unc: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
    let verbatim: Vec<u16> = r"\\?\".encode_utf16().collect();
    if w.starts_with(&unc) {
        let mut out: Vec<u16> = r"\\".encode_utf16().collect();
        out.extend_from_slice(&w[unc.len()..]);
        return std::ffi::OsString::from_wide(&out).into();
    }
    if w.starts_with(&verbatim) {
        let rest = &w[verbatim.len()..];
        if rest.len() >= 2 && rest[1] == u16::from(b':') {
            return std::ffi::OsString::from_wide(rest).into();
        }
    }
    p
}

fn read_u32(b: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn read_i64(b: &[u8], at: usize) -> i64 {
    i64::from_ne_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

/// Lista TODAS las entradas de un directorio abierto con `FILE_LIST_DIRECTORY` (sin `.` ni `..`) con
/// `FileIdExtdDirectoryInfo`. Devuelve el código de error de Windows si falla. Los campos se leen con índices
/// comprobados: un búfer inconsistente da un error, nunca una lectura fuera de rango.
pub fn list_dir(h: &Handle) -> Result<Vec<DirEntry>, Win32> {
    const HDR: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, FileName);
    const NEXT: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, NextEntryOffset);
    const CTIME: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, CreationTime);
    const MTIME: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, LastWriteTime);
    const CHTIME: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, ChangeTime);
    const EOF: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, EndOfFile);
    const ALLOC: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, AllocationSize);
    const ATTRS: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, FileAttributes);
    const NAME_LEN: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, FileNameLength);
    const TAG: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, ReparsePointTag);
    const ID: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, FileId);
    let mut buf = vec![0u64; BUF_BYTES / 8];
    let buf_len = std::mem::size_of_val(buf.as_slice());
    let mut out = Vec::new();
    loop {
        // SAFETY: `h.0` es un handle de directorio válido; `buf` es memoria escribible de `buf_len` bytes alineada a 8
        // (es un `[u64]`), como exige la API.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                h.0,
                FileIdExtdDirectoryInfo,
                buf.as_mut_ptr() as *mut c_void,
                buf_len as u32,
            )
        };
        if ok == 0 {
            let e = last_error();
            return if e == ERROR_NO_MORE_FILES {
                Ok(out)
            } else {
                Err(e)
            };
        }
        // SAFETY: `buf` es memoria inicializada de `buf_len` bytes y no se modifica mientras viva esta vista (la
        // siguiente llamada a la API solo ocurre después de soltarla).
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, buf_len) };
        let mut off = 0usize;
        loop {
            if !off.is_multiple_of(8) || off + HDR > buf_len {
                return Err(ERROR_INVALID_DATA);
            }
            let rec = &bytes[off..];
            let next = read_u32(rec, NEXT) as usize;
            let name_len = read_u32(rec, NAME_LEN) as usize;
            if !name_len.is_multiple_of(2) || HDR + name_len > rec.len() {
                return Err(ERROR_INVALID_DATA);
            }
            let name: Box<[u16]> = rec[HDR..HDR + name_len]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_ne_bytes(*c))
                .collect();
            let dot = name.as_ref() == [b'.' as u16] || name.as_ref() == [b'.' as u16, b'.' as u16];
            if !dot {
                let id: [u8; 16] = rec[ID..ID + 16].try_into().expect("16 bytes");
                out.push(DirEntry {
                    name,
                    attrs: read_u32(rec, ATTRS),
                    reparse_tag: read_u32(rec, TAG),
                    alloc: read_i64(rec, ALLOC).max(0) as u64,
                    logical: read_i64(rec, EOF).max(0) as u64,
                    file_id: u128::from_le_bytes(id),
                    newest_filetime: read_i64(rec, CTIME)
                        .max(read_i64(rec, MTIME))
                        .max(read_i64(rec, CHTIME)),
                });
            }
            if next == 0 {
                break;
            }
            off += next;
        }
    }
}

/// Marca para borrar el objeto abierto: `FileDispositionInfoEx` (DELETE | POSIX_SEMANTICS | IGNORE_READONLY) sobre
/// ESTE handle. Un directorio solo se borra si está vacío (`DIR_NOT_EMPTY` si no). Si el sistema de archivos no
/// admite la clase extendida (o la semántica POSIX), cae a `FileDispositionInfo`, que borra al cerrar el handle.
pub(crate) fn delete_on_handle(h: &Handle) -> Result<(), Win32> {
    let info = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE
            | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
            | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    // SAFETY: `h.0` es un handle válido abierto con acceso DELETE; el puntero apunta a `info` y se pasa su tamaño.
    let ok = unsafe {
        SetFileInformationByHandle(
            h.0,
            FileDispositionInfoEx,
            &info as *const _ as *const c_void,
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };
    if ok != 0 {
        return Ok(());
    }
    let e = last_error();
    if !matches!(
        e,
        ERROR_INVALID_PARAMETER | ERROR_NOT_SUPPORTED | ERROR_INVALID_FUNCTION
    ) {
        return Err(e);
    }
    let legacy = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: igual que arriba, con la estructura de la clase clásica.
    let ok = unsafe {
        SetFileInformationByHandle(
            h.0,
            FileDispositionInfo,
            &legacy as *const _ as *const c_void,
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    if ok == 0 { Err(last_error()) } else { Ok(()) }
}

// ---- Vigilancia de cambios de un directorio (A3-M1) -------------------------------------------------------------------

/// Por qué no se pudo vigilar un directorio. FALLA CERRADO: sin vigilancia no se envía.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchError {
    /// El objeto abierto no es el aprobado (identidad, tipo o reparse point).
    Changed,
    /// Error del sistema al abrirlo, leer su identidad o arrancar la vigilancia.
    Os(Win32),
}

/// Palabras de 32 bits del búfer de `ReadDirectoryChangesW`. No se lee nunca (cualquier finalización invalida la medida):
/// solo tiene que existir y estar alineado a DWORD mientras la lectura esté pendiente.
const WATCH_BUF_WORDS: usize = 256;

/// Lo que el kernel toca mientras hay una lectura pendiente: su dirección NO puede cambiar hasta que termine (por eso vive en
/// una `Box` que no se mueve nunca).
struct WatchInner {
    /// El directorio, abierto con `FILE_FLAG_OVERLAPPED`.
    handle: Handle,
    /// Estructura de la lectura pendiente (su `hEvent` es `event`).
    ov: OVERLAPPED,
    /// Evento de reinicio manual de `ov`.
    event: Handle,
    buf: [u32; WATCH_BUF_WORDS],
    /// ¿Hay una lectura pendiente en el kernel? Solo entonces el kernel escribe en `ov` y `buf`.
    pending: bool,
}

// SAFETY: `OVERLAPPED` lleva un `HANDLE` (puntero crudo) y por eso no es `Send` por sí solo. El kernel solo escribe en `ov` y
// `buf` mientras `pending` es cierto, la caja no se mueve y todo acceso desde Rust pasa por el `Mutex` de `DirWatch`.
unsafe impl Send for WatchInner {}

/// Tests only: roots under which `DirWatch::start` fails with "access denied" (to verify that an item is not sent without
/// monitoring). Acts only on paths under the test folder that installed it.
#[cfg(test)]
static FAIL_WATCH: std::sync::Mutex<Vec<(u64, crate::paths::PathKey)>> =
    std::sync::Mutex::new(Vec::new());
#[cfg(test)]
static FAIL_WATCH_NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Tests only: guard that removes the forced failure when dropped.
#[cfg(test)]
pub struct FailWatchGuard(u64);

#[cfg(test)]
impl Drop for FailWatchGuard {
    fn drop(&mut self) {
        FAIL_WATCH
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(id, _)| *id != self.0);
    }
}

/// Tests only: makes `DirWatch::start` fail for paths under `root`.
#[cfg(test)]
pub fn fail_watch_under(root: &Path) -> FailWatchGuard {
    let id = FAIL_WATCH_NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    FAIL_WATCH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((id, crate::paths::PathKey::new(root)));
    FailWatchGuard(id)
}

/// Vigilancia de TODO lo que cuelga de un directorio (subárbol) mientras se mide y hasta justo antes de que la shell actúe
/// (A3-M1). Es un segundo handle del directorio con `ReadDirectoryChangesW` (tamaño, última escritura, altas, bajas y
/// renombrados de archivos y de directorios). CUALQUIER finalización de la lectura (una notificación, el desbordamiento del
/// búfer o un error) significa que la medida ya no es cierta: [`DirWatch::changed`] pasa a `true` y no vuelve a `false`. Los
/// accesos de solo lectura del propio recorrido (atributos, listados) no generan notificaciones con estos filtros.
///
/// Límite del sistema (documentado, no oculto): NTFS retrasa las notificaciones de TAMAÑO y de ÚLTIMA ESCRITURA de un archivo
/// que alguien sigue teniendo abierto hasta que lo cierra o se vuelca la caché; las altas, bajas y renombrados se notifican
/// al momento. Un escritor que mantenga abierto un archivo y lo agrande puede no notificar dentro de la ventana.
pub struct DirWatch {
    inner: std::sync::Mutex<Option<Box<WatchInner>>>,
    /// Hubo un cambio (o la vigilancia falló): no vuelve a `false`.
    changed: std::sync::atomic::AtomicBool,
}

impl DirWatch {
    /// Abre `path` por segunda vez (sin seguir enlaces), exige que sea el directorio de identidad `expect` y arranca la
    /// vigilancia. Hay que llamarla ANTES de empezar a medir: lo que cambie antes no se vería.
    pub fn start(path: &Path, expect: FileIdentity) -> Result<DirWatch, WatchError> {
        #[cfg(test)]
        {
            let key = crate::paths::PathKey::new(path);
            let forced = FAIL_WATCH
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|(_, root)| key.is_same_or_under(root));
            if forced {
                return Err(WatchError::Os(ACCESS_DENIED));
            }
        }
        let h = open_dir_for_watch(path).map_err(WatchError::Os)?;
        let m = meta(&h).map_err(WatchError::Os)?;
        if m.identity != expect || !m.is_dir || m.is_reparse() {
            return Err(WatchError::Changed);
        }
        // SAFETY: sin atributos de seguridad ni nombre; evento de reinicio manual, no señalado.
        let ev = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if ev.is_null() {
            return Err(WatchError::Os(last_error()));
        }
        // SAFETY: `OVERLAPPED` es un POD (enteros y punteros): todo-ceros es válido.
        let ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        let mut inner = Box::new(WatchInner {
            handle: h,
            ov,
            event: Handle(ev),
            buf: [0; WATCH_BUF_WORDS],
            pending: false,
        });
        inner.ov.hEvent = inner.event.0;
        let filter = FILE_NOTIFY_CHANGE_SIZE
            | FILE_NOTIFY_CHANGE_LAST_WRITE
            | FILE_NOTIFY_CHANGE_FILE_NAME
            | FILE_NOTIFY_CHANGE_DIR_NAME;
        // SAFETY: el handle se abrió con `FILE_LIST_DIRECTORY` y `FILE_FLAG_OVERLAPPED`; `buf` y `ov` viven en la caja, que no
        // se mueve ni se libera mientras la lectura esté pendiente (`finish_watch` espera a que termine antes de soltarla); el
        // búfer es de `u32` (alineado a DWORD) y se pasa su tamaño exacto; `lpBytesReturned` es nulo, como exige la llamada
        // asíncrona.
        let ok = unsafe {
            ReadDirectoryChangesW(
                inner.handle.0,
                inner.buf.as_mut_ptr().cast(),
                (WATCH_BUF_WORDS * 4) as u32,
                1,
                filter,
                std::ptr::null_mut(),
                &mut inner.ov,
                None,
            )
        };
        if ok == 0 {
            return Err(WatchError::Os(last_error()));
        }
        inner.pending = true;
        Ok(DirWatch {
            inner: std::sync::Mutex::new(Some(inner)),
            changed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// ¿Cambió algo desde que arrancó la vigilancia (o falló)? Consulta sin esperar. Tras [`DirWatch::stop`] devuelve lo que
    /// se supo hasta entonces.
    pub fn changed(&self) -> bool {
        use std::sync::atomic::Ordering;
        if self.changed.load(Ordering::SeqCst) {
            return true;
        }
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(inner) = g.as_mut() else {
            return self.changed.load(Ordering::SeqCst);
        };
        if inner.pending {
            let mut n = 0u32;
            // SAFETY: handle y `ov` válidos de una lectura lanzada con esta misma estructura; `n` es escribible; sin esperar.
            let ok = unsafe { GetOverlappedResult(inner.handle.0, &inner.ov, &mut n, 0) };
            if ok == 0 && last_error() == ERROR_IO_INCOMPLETE {
                return false;
            }
            // Terminó: con notificaciones (`n > 0`), desbordada (`n == 0`, `ERROR_NOTIFY_ENUM_DIR`) o con un error. En los tres
            // casos lo que se midió ya no es de fiar.
            inner.pending = false;
            self.changed.store(true, Ordering::SeqCst);
            return true;
        }
        self.changed.load(Ordering::SeqCst)
    }

    /// Para la vigilancia (cancela la lectura pendiente y suelta el handle). Se llama tras comprobar por última vez, justo
    /// antes de que la shell mueva el directorio: así no queda un handle con E/S pendiente sobre él.
    pub fn stop(&self) {
        let _ = self.changed();
        let taken = self.inner.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(inner) = taken
            && finish_watch(inner)
        {
            self.changed
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

impl std::fmt::Debug for DirWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirWatch")
            .field(
                "changed",
                &self.changed.load(std::sync::atomic::Ordering::SeqCst),
            )
            .finish_non_exhaustive()
    }
}

impl Drop for DirWatch {
    fn drop(&mut self) {
        let taken = self
            .inner
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(inner) = taken {
            finish_watch(inner);
        }
    }
}

/// Termina una vigilancia: si la lectura sigue pendiente la cancela y ESPERA a que el kernel acabe con el búfer antes de
/// liberarlo. Devuelve `true` si la lectura había terminado con un cambio real (no por la cancelación). Si el kernel no
/// termina (no debería), se PIERDE la memoria a propósito: liberar un búfer que el kernel aún puede escribir es peor.
fn finish_watch(mut inner: Box<WatchInner>) -> bool {
    if !inner.pending {
        return false;
    }
    // SAFETY: handle y `ov` válidos de la lectura pendiente; cancelar solo esa lectura. Un error (`ERROR_NOT_FOUND`: ya había
    // terminado) no importa: se espera igual.
    unsafe { CancelIoEx(inner.handle.0, &inner.ov) };
    // SAFETY: `event` es el evento de `ov`, vivo.
    let waited = unsafe { WaitForSingleObject(inner.event.0, 5_000) };
    if waited != WAIT_OBJECT_0 {
        std::mem::forget(inner);
        return false;
    }
    let mut n = 0u32;
    // SAFETY: handle y `ov` válidos; la lectura ya terminó (el evento está señalado), así que no espera.
    let ok = unsafe { GetOverlappedResult(inner.handle.0, &inner.ov, &mut n, 0) };
    inner.pending = false;
    // Cancelada (`ERROR_OPERATION_ABORTED`): nada cambió. Terminada con éxito o con cualquier otro error: algo pasó.
    !(ok == 0 && last_error() == ERROR_OPERATION_ABORTED)
}

/// Abre un directorio solo para vigilarlo: listar, leer atributos, compartiéndolo TODO (también `FILE_SHARE_DELETE`, para no
/// impedir que la shell lo mueva), sin seguir enlaces y para E/S asíncrona.
fn open_dir_for_watch(path: &Path) -> Result<Handle, Win32> {
    let p = wide_long(path);
    // SAFETY: `p` es una cadena UTF-16 terminada en NUL que vive durante la llamada; el resto son constantes o nulos
    // permitidos.
    let h = unsafe {
        CreateFileW(
            p.as_ptr(),
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
            FILE_SHARE_ALL,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OVERLAPPED,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    Ok(Handle(h))
}

/// Tests only: preallocates `bytes` on disk (`FileAllocationInfo`) WITHOUT changing the logical size (`EndOfFile`).
#[cfg(test)]
pub fn preallocate_for_test(path: &Path, bytes: i64) -> Result<(), Win32> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ALLOCATION_INFO, FILE_WRITE_DATA, FileAllocationInfo,
    };
    let h = open_path(path, FILE_WRITE_DATA | FILE_READ_ATTRIBUTES, FILE_SHARE_ALL)?;
    let info = FILE_ALLOCATION_INFO {
        AllocationSize: bytes,
    };
    // SAFETY: `h.0` is a valid handle opened with FILE_WRITE_DATA; the pointer refers to `info`, whose size is passed.
    let ok = unsafe {
        SetFileInformationByHandle(
            h.0,
            FileAllocationInfo,
            &info as *const _ as *const c_void,
            size_of::<FILE_ALLOCATION_INFO>() as u32,
        )
    };
    if ok == 0 { Err(last_error()) } else { Ok(()) }
}

/// Tests only: deletes `path` with classic Windows semantics (`FileDispositionInfo`, without POSIX), as the shell does:
/// if another handle remains open, the name stays visible as "delete pending" until that handle is closed.
#[cfg(test)]
pub fn delete_legacy_for_test(path: &Path) -> Result<(), Win32> {
    let h = open_path(path, DELETE | FILE_READ_ATTRIBUTES, FILE_SHARE_ALL)?;
    let legacy = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: `h.0` is a valid handle opened with DELETE; the pointer refers to `legacy`, whose size is passed.
    let ok = unsafe {
        SetFileInformationByHandle(
            h.0,
            FileDispositionInfo,
            &legacy as *const _ as *const c_void,
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    if ok == 0 { Err(last_error()) } else { Ok(()) }
}

/// Tests only: sets the object's creation, last-access, last-write, and change times to `filetime` (FILETIME).
#[cfg(test)]
pub fn set_all_times_for_test(path: &Path, filetime: i64) -> Result<(), Win32> {
    use windows_sys::Win32::Storage::FileSystem::FILE_WRITE_ATTRIBUTES;
    let h = open_path(
        path,
        FILE_WRITE_ATTRIBUTES | FILE_READ_ATTRIBUTES,
        FILE_SHARE_ALL,
    )?;
    let info = FILE_BASIC_INFO {
        CreationTime: filetime,
        LastAccessTime: filetime,
        LastWriteTime: filetime,
        ChangeTime: filetime,
        FileAttributes: 0,
    };
    // SAFETY: `h.0` is a valid handle with FILE_WRITE_ATTRIBUTES; the pointer refers to `info`, whose size is passed.
    let ok = unsafe {
        SetFileInformationByHandle(
            h.0,
            FileBasicInfo,
            &info as *const _ as *const c_void,
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if ok == 0 { Err(last_error()) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Sandbox;
    use std::fs;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn rutas_largas_con_prefijo_y_unc() {
        // Network paths are assembled in parts: the repository does not allow literal UNC paths.
        let net_in = ["\\\\", "srv", "\\", "share", "\\", "x"].concat();
        let net_out = ["\\\\?\\", "UNC", "\\", "srv", "\\", "share", "\\", "x"].concat();
        let v = wide_long(Path::new(r"C:\a\b"));
        assert_eq!(String::from_utf16_lossy(&v[..v.len() - 1]), r"\\?\C:\a\b");
        let v = wide_long(Path::new(r"\\?\C:\a"));
        assert_eq!(String::from_utf16_lossy(&v[..v.len() - 1]), r"\\?\C:\a");
        let v = wide_long(Path::new("C:/a/b"));
        assert_eq!(String::from_utf16_lossy(&v[..v.len() - 1]), r"\\?\C:\a\b");
        let v = wide_long(Path::new(&net_in));
        assert_eq!(String::from_utf16_lossy(&v[..v.len() - 1]), net_out);
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\C:\x")),
            PathBuf::from(r"C:\x")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from(&net_out)),
            PathBuf::from(&net_in)
        );
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\GLOBALROOT\x")),
            PathBuf::from(r"\\?\GLOBALROOT\x")
        );
    }

    #[test]
    fn etiquetas_de_la_nube() {
        assert!(is_cloud_tag(0x9000_001A));
        assert!(is_cloud_tag(0x9000_601A));
        assert!(is_cloud_tag(0x9000_F01A));
        assert!(!is_cloud_tag(0xA000_000C)); // symlink
        assert!(!is_cloud_tag(0xA000_0003)); // junction
        assert!(!is_cloud_tag(0));
    }

    #[test]
    fn meta_identidad_enlaces_y_ruta_final() {
        let sb = Sandbox::new();
        let f = sb.write("a/uno.bin", 5000);
        let h = open_for_query(&f).unwrap();
        let m = meta(&h).unwrap();
        assert!(!m.is_dir && !m.is_reparse());
        assert_eq!(m.links, 1);
        assert_eq!(m.logical, 5000);
        assert!(m.alloc >= 5000);
        assert_ne!(m.identity.file_id, 0);
        assert_eq!(final_path(&h).unwrap(), sb.long("a/uno.bin"));
        // The same identity through another handle.
        let m2 = meta(&open_for_query(&f).unwrap()).unwrap();
        assert_eq!(m.identity, m2.identity);
        // A directory.
        let d = open_for_query(&sb.path("a")).unwrap();
        let md = meta(&d).unwrap();
        assert!(md.is_dir);
        assert_ne!(md.identity, m.identity);
        // A hard link has two links and retains the same identity.
        sb.hard_link("a/uno.bin", "a/dos.bin");
        let m3 = meta(&open_for_query(&f).unwrap()).unwrap();
        assert_eq!(m3.links, 2);
        assert_eq!(m3.identity, m.identity);
        // Opening a nonexistent file fails with "not found".
        assert_eq!(open_for_query(&sb.path("a/no.bin")).err(), Some(2));
    }

    #[test]
    fn el_listado_da_nombre_id_y_tamanos_y_el_hijo_relativo_coincide() {
        let sb = Sandbox::new();
        sb.write("d/x.bin", 100);
        sb.write("d/y.bin", 9000);
        sb.mkdir("d/sub");
        let dh = open_dir_for_listing(&sb.path("d")).unwrap();
        let mut es = list_dir(&dh).unwrap();
        es.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<String> = es
            .iter()
            .map(|e| String::from_utf16_lossy(&e.name))
            .collect();
        assert_eq!(names, ["sub", "x.bin", "y.bin"]);
        assert!(es[0].is_dir() && !es[1].is_dir());
        assert_eq!(es[2].logical, 9000);
        // Open each child RELATIVE to its parent and compare its identity with the listing.
        for e in &es {
            let ex = if e.is_dir() {
                Expect::Dir
            } else {
                Expect::File
            };
            let h = open_child_for_query(&dh, &e.name, ex).unwrap();
            assert_eq!(meta(&h).unwrap().identity.file_id, e.file_id);
        }
        // The system-enforced type: a file cannot be opened as a directory, or vice versa.
        assert!(open_child_for_query(&dh, &w("x.bin"), Expect::Dir).is_err());
        assert!(open_child_for_query(&dh, &w("sub"), Expect::File).is_err());
        // Names containing separators or empty names are rejected before reaching the kernel.
        assert_eq!(
            open_child_for_query(&dh, &w(r"sub\x"), Expect::File).err(),
            Some(ERROR_INVALID_PARAMETER)
        );
        assert_eq!(
            open_child_for_query(&dh, &w(""), Expect::File).err(),
            Some(ERROR_INVALID_PARAMETER)
        );
        assert!(open_child_for_query(&dh, &w(".."), Expect::Dir).is_err());
        // H6.10: `:` (an alternate data stream, `::$INDEX_ALLOCATION`) is not an entry name: reject it before the kernel,
        // even when the stream exists.
        std::fs::write(sb.path("d/x.bin:flujo"), b"oculto").unwrap();
        for bad in [
            "x.bin:flujo",
            "sub::$INDEX_ALLOCATION",
            "x.bin::$DATA",
            ":",
            "a:b",
        ] {
            assert_eq!(
                open_child_for_query(&dh, &w(bad), Expect::File).err(),
                Some(ERROR_INVALID_PARAMETER),
                "{bad}"
            );
            assert_eq!(
                open_child_for_query(&dh, &w(bad), Expect::Dir).err(),
                Some(ERROR_INVALID_PARAMETER),
                "{bad}"
            );
        }
    }

    #[test]
    fn la_apertura_exclusiva_falla_si_otro_la_tiene_abierta_incluso_con_share_delete() {
        let sb = Sandbox::new();
        let f = sb.write("e/uso.bin", 2000);
        let dh = open_dir_for_listing(&sb.path("e")).unwrap();
        // `std::fs::File::open` shares read, write, AND delete access (the Chromium case).
        let other = fs::File::open(&f).unwrap();
        assert_eq!(
            open_file_exclusive(&dh, &w("uso.bin")).err(),
            Some(SHARING_VIOLATION)
        );
        drop(other);
        let h = open_file_exclusive(&dh, &w("uso.bin")).unwrap();
        assert!(!meta(&h).unwrap().is_dir);
    }

    #[test]
    fn borrar_por_handle_quita_el_nombre_y_un_directorio_solo_vacio() {
        let sb = Sandbox::new();
        let f = sb.write("b/f.bin", 3000);
        sb.write("b/dir/g.bin", 10);
        let bh = open_dir_for_listing(&sb.path("b")).unwrap();
        // A read-only file is still deleted (IGNORE_READONLY).
        let mut perm = fs::metadata(&f).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&f, perm).unwrap();
        let fh = open_file_exclusive(&bh, &w("f.bin")).unwrap();
        delete_on_handle(&fh).unwrap();
        drop(fh);
        assert!(!f.exists());
        // A nonempty directory is not deleted.
        let dh = open_dir_for_delete(&bh, &w("dir")).unwrap();
        assert_eq!(delete_on_handle(&dh).err(), Some(DIR_NOT_EMPTY));
        assert!(sb.path("b/dir/g.bin").exists());
        // An empty one is.
        let gh = open_file_exclusive(&dh, &w("g.bin")).unwrap();
        delete_on_handle(&gh).unwrap();
        drop(gh);
        delete_on_handle(&dh).unwrap();
        drop(dh);
        assert!(!sb.path("b/dir").exists());
    }

    #[test]
    fn leer_un_archivo_pequeno_por_el_handle_exclusivo_y_borrarlo_por_el_mismo() {
        let sb = Sandbox::new();
        let f = sb.write("r/pequeno.bin", 9_000);
        let h = open_path_exclusive_read(&f).unwrap();
        // While the handle is open exclusively, nobody else can open it (even for reading).
        assert_eq!(
            fs::File::open(&f).err().and_then(|e| e.raw_os_error()),
            Some(SHARING_VIOLATION as i32)
        );
        let bytes = read_small(&h, 70_000).unwrap();
        assert_eq!(bytes.len(), 9_000);
        assert_eq!(bytes[0], 1);
        // Exceeding the limit fails.
        let g = sb.write("r/otro.bin", 5_000);
        let h2 = open_path_exclusive_read(&g).unwrap();
        assert_eq!(read_small(&h2, 100).err(), Some(ERROR_FILE_TOO_LARGE));
        delete_on_handle(&h).unwrap();
        drop(h);
        assert!(!f.exists());
    }

    #[test]
    fn fijar_fechas_cambia_la_mas_reciente_que_ve_meta() {
        let sb = Sandbox::new();
        let f = sb.write("t/f.bin", 10);
        let now = meta(&open_for_query(&f).unwrap()).unwrap().newest_filetime;
        let old = now - 48 * 3600 * 10_000_000;
        set_all_times_for_test(&f, old).unwrap();
        let after = meta(&open_for_query(&f).unwrap()).unwrap().newest_filetime;
        assert_eq!(after, old);
    }

    #[test]
    fn un_junction_se_abre_a_si_mismo_y_no_se_sigue() {
        let mut sb = Sandbox::new();
        sb.mkdir("t/dentro");
        sb.write("t/dentro/secreto.bin", 100);
        sb.mkdir("l");
        sb.junction("l/enlace", "t");
        let lh = open_dir_for_listing(&sb.path("l")).unwrap();
        let es = list_dir(&lh).unwrap();
        assert_eq!(es.len(), 1);
        assert!(es[0].is_reparse() && es[0].is_dir());
        let h = open_child_for_query(&lh, &es[0].name, Expect::Dir).unwrap();
        let m = meta(&h).unwrap();
        assert!(m.is_reparse());
        assert_eq!(m.reparse_tag, 0xA000_0003);
        // The link's final path is the link itself, not its target.
        assert_eq!(final_path(&h).unwrap(), sb.long("l/enlace"));
        // Its identity also differs from the target's.
        let target = meta(&open_for_query(&sb.path("t")).unwrap()).unwrap();
        assert_ne!(m.identity, target.identity);
    }

    #[test]
    fn la_cadena_se_recorre_por_componentes_y_rechaza_cualquier_reparse_point() {
        let mut sb = Sandbox::new();
        sb.mkdir("real/a/b");
        sb.mkdir("destino/dentro");
        sb.junction("real/enlace", "destino");
        // A clean path (including the test's own `%TEMP%`) opens and returns the canonical path.
        let h = open_reparse_free_dir(&sb.path("real/a/b")).unwrap();
        assert_eq!(final_path(&h).unwrap(), sb.long("real/a/b"));
        // The same holds with a verbatim prefix and different casing.
        let upper = PathBuf::from(sb.long("real/a").to_string_lossy().to_uppercase());
        assert!(open_reparse_free_dir(&upper).is_ok());
        // A junction as the LAST component is rejected (it opens itself, not its target).
        assert_eq!(
            open_reparse_free_dir(&sb.path("real/enlace")).err(),
            Some(ChainError::Reparse {
                component: "enlace".into()
            })
        );
        // An ANCESTOR junction is also rejected, even if its target exists and is a normal directory.
        assert_eq!(
            open_reparse_free_dir(&sb.path("real/enlace/dentro")).err(),
            Some(ChainError::Reparse {
                component: "enlace".into()
            })
        );
        // Paths that are not absolute drive-letter paths, or do not exist.
        assert_eq!(
            open_reparse_free_dir(Path::new(r"real\a")).err(),
            Some(ChainError::BadPath)
        );
        assert_eq!(
            open_reparse_free_dir(Path::new(r"\servidor\recurso\x")).err(),
            Some(ChainError::BadPath)
        );
        assert!(matches!(
            open_reparse_free_dir(&sb.path("real/no-existe")).err(),
            Some(ChainError::Os(2 | 3))
        ));
        // A file is not a directory in the path chain.
        sb.write("real/f.bin", 10);
        assert!(open_reparse_free_dir(&sb.path("real/f.bin")).is_err());
    }

    // ---- DirWatch (A3-M1) -------------------------------------------------------------------------------------------------

    fn watch_tree(sb: &Sandbox) -> (PathBuf, FileIdentity) {
        sb.write("W/a.bin", 1_000);
        sb.write("W/sub/b.bin", 2_000);
        sb.write("W/sub/hondo/c.bin", 3_000);
        let dir = sb.long("W");
        let id = meta(&open_for_query(&dir).unwrap()).unwrap().identity;
        (dir, id)
    }

    /// Wait briefly for the watcher to observe a change: additions and removals are reported immediately, but the system
    /// delivers the completion asynchronously.
    fn sees_change(w: &DirWatch) -> bool {
        for _ in 0..40 {
            if w.changed() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        w.changed()
    }

    /// What APPROVAL has already done before monitoring starts: open and close every directory and file. NTFS delays updating
    /// an entry's cached write time and size in its parent's index until someone opens and closes the object. In this test,
    /// that pending update arrives as a notification on the first close. Approval's measurement has already triggered it,
    /// so no update remains pending when execution monitoring starts.
    fn settle(sb: &Sandbox) {
        for rel in ["W", "W/sub", "W/sub/hondo"] {
            drop(open_dir_for_listing(&sb.long(rel)).unwrap());
        }
        for rel in ["W/a.bin", "W/sub/b.bin", "W/sub/hondo/c.bin"] {
            drop(open_for_query(&sb.long(rel)).unwrap());
        }
    }

    /// A3-M1: what is NOT a change. The measurement walk (read-only opens of each child, listings, and reads of attributes
    /// and contents) generates no notifications with these filters; otherwise, no directory would ever be sent.
    #[test]
    fn la_vigilancia_no_ve_los_accesos_de_solo_lectura() {
        let sb = Sandbox::new();
        let (dir, id) = watch_tree(&sb);
        settle(&sb);
        let w = DirWatch::start(&dir, id).expect("arranca");
        // What the measurement walk and guard do: open, read metadata, and list.
        let root = open_dir_for_listing(&dir).unwrap();
        let _ = list_dir(&root).unwrap();
        for rel in ["W/a.bin", "W/sub/b.bin", "W/sub/hondo/c.bin"] {
            let h = open_for_query(&sb.long(rel)).unwrap();
            let _ = meta(&h).unwrap();
            let _ = sizes(&h).unwrap();
            let _ = final_path(&h).unwrap();
        }
        let guard = open_path_for_rename(&dir).unwrap();
        let _ = meta(&guard).unwrap();
        // Also read contents and metadata through the standard library.
        assert_eq!(fs::read(sb.path("W/a.bin")).unwrap().len(), 1_000);
        let _ = fs::metadata(sb.path("W/sub")).unwrap();
        let _ = fs::read_dir(sb.path("W/sub/hondo")).unwrap().count();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!w.changed(), "los accesos de solo lectura no son cambios");
    }

    /// A3-M1: what IS a change at any depth in the subtree: an addition, removal, rename, new directory, or an existing file
    /// that grows and is closed.
    #[test]
    fn la_vigilancia_ve_altas_bajas_renombrados_y_escrituras() {
        type Change = (&'static str, fn(&Sandbox));
        let changes: [Change; 6] = [
            ("alta", |sb| {
                sb.write("W/sub/hondo/nuevo.bin", 10);
            }),
            ("baja", |sb| {
                fs::remove_file(sb.path("W/sub/b.bin")).unwrap()
            }),
            ("renombrado", |sb| {
                fs::rename(sb.path("W/a.bin"), sb.path("W/z.bin")).unwrap()
            }),
            ("directorio", |sb| {
                sb.mkdir("W/sub/hondo/otro");
            }),
            ("crece", |sb| {
                use std::io::Write;
                let mut f = fs::OpenOptions::new()
                    .append(true)
                    .open(sb.path("W/sub/hondo/c.bin"))
                    .unwrap();
                f.write_all(&vec![7u8; 1 << 20]).unwrap();
            }),
            ("encoge", |sb| {
                let f = fs::OpenOptions::new()
                    .write(true)
                    .open(sb.path("W/sub/b.bin"))
                    .unwrap();
                f.set_len(10).unwrap();
            }),
        ];
        for (name, change) in changes {
            let sb = Sandbox::new();
            let (dir, id) = watch_tree(&sb);
            let w = DirWatch::start(&dir, id).expect("arranca");
            assert!(!w.changed(), "{name}: antes del cambio no hay nada");
            change(&sb);
            assert!(sees_change(&w), "{name}: debe verse");
            // It does not return to `false`.
            assert!(w.changed(), "{name}");
        }
    }

    /// A3-M1: without an approved identity there is no monitoring (fail closed): a different identity, a nonexistent
    /// directory, or a junction (which is not followed).
    #[test]
    fn la_vigilancia_exige_la_identidad_y_no_sigue_enlaces() {
        let mut sb = Sandbox::new();
        let (dir, id) = watch_tree(&sb);
        let other = FileIdentity {
            file_id: id.file_id ^ 1,
            ..id
        };
        assert!(matches!(
            DirWatch::start(&dir, other),
            Err(WatchError::Changed)
        ));
        assert!(matches!(
            DirWatch::start(&sb.long("no-existe"), id),
            Err(WatchError::Os(2 | 3))
        ));
        sb.junction("enlace", "W");
        let link = sb.long("enlace");
        assert!(matches!(
            DirWatch::start(&link, id),
            Err(WatchError::Changed)
        ));
        // A file is not a directory.
        let f = sb.long("W/a.bin");
        let fid = meta(&open_for_query(&f).unwrap()).unwrap().identity;
        assert!(DirWatch::start(&f, fid).is_err());
        // Control: with the correct identity, it starts.
        assert!(DirWatch::start(&dir, id).is_ok());
    }

    /// A3-M1: stopping the watcher cancels the pending read and releases the handle (so the directory can be moved); dropping
    /// it without stopping also completes without hanging. Changes made BEFORE stopping are remembered; later ones are ignored.
    #[test]
    fn parar_la_vigilancia_suelta_el_handle_y_recuerda_lo_anterior() {
        let sb = Sandbox::new();
        let (dir, id) = watch_tree(&sb);
        let w = DirWatch::start(&dir, id).expect("arranca");
        w.stop();
        assert!(!w.changed(), "sin cambios antes de parar");
        sb.write("W/despues.bin", 10);
        assert!(!w.changed(), "tras parar ya no se mira");
        // With no handle open, the directory can be moved.
        fs::rename(sb.path("W"), sb.path("W2")).expect("el directorio se mueve");
        fs::rename(sb.path("W2"), sb.path("W")).unwrap();
        // Changes made before stopping are still reported afterward.
        let w2 = DirWatch::start(&dir, id).expect("arranca");
        sb.write("W/antes.bin", 10);
        assert!(sees_change(&w2));
        w2.stop();
        assert!(w2.changed());
        // Dropping without stopping does not hang and releases the handle.
        let t = std::time::Instant::now();
        let w3 = DirWatch::start(&dir, id).expect("arranca");
        drop(w3);
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
        fs::rename(sb.path("W"), sb.path("W3")).expect("sin handles abiertos");
    }

    /// A3-M1 (measurement, not an assertion): pending monitoring does NOT prevent moving the directory (it shares
    /// `FILE_SHARE_DELETE`, like the guard); it is still stopped after the last `PreDeleteItem`. Also observe NTFS's delay:
    /// a writer keeps a file open and grows it. Print what happens.
    #[test]
    fn medicion_de_la_vigilancia_pendiente_y_de_un_escritor_abierto() {
        use std::io::Write;
        let sb = Sandbox::new();
        let (dir, id) = watch_tree(&sb);
        // 1. Can the directory be renamed while the read is pending?
        let w = DirWatch::start(&dir, id).expect("arranca");
        let moved = fs::rename(sb.path("W"), sb.path("W2"));
        eprintln!("renombrar con la vigilancia pendiente: {moved:?}");
        if moved.is_ok() {
            fs::rename(sb.path("W2"), sb.path("W")).unwrap();
        }
        drop(w);
        // 2. With a writer keeping the file open, when does the growth become visible?
        let w = DirWatch::start(&dir, id).expect("arranca");
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(sb.path("W/a.bin"))
            .unwrap();
        f.write_all(&vec![1u8; 1 << 20]).unwrap();
        let t = std::time::Instant::now();
        let mut seen = None;
        while t.elapsed() < std::time::Duration::from_secs(3) {
            if w.changed() {
                seen = Some(t.elapsed());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        eprintln!("escritor abierto, sin cerrar ni volcar: visto en {seen:?}");
        f.flush().unwrap();
        f.sync_all().ok();
        let t = std::time::Instant::now();
        let mut seen2 = None;
        while t.elapsed() < std::time::Duration::from_secs(3) {
            if w.changed() {
                seen2 = Some(t.elapsed());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        eprintln!("tras volcar (`sync_all`): visto en {seen2:?}");
        drop(f);
        let t = std::time::Instant::now();
        while !w.changed() && t.elapsed() < std::time::Duration::from_secs(3) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        eprintln!("tras cerrar: changed = {}", w.changed());
        assert!(w.changed(), "al cerrar el archivo el crecimiento SÍ se ve");
    }
}
