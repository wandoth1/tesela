//! Windows layer for `cleaner`: open objects by handle, identify, delete, and rename THROUGH THE HANDLE.
//!
//! This is the only module in the crate with `unsafe`, alongside `recycle` (COM) and `process` (process listing). Each
//! call is a small function with its own `// SAFETY:` explanation, and the test module exercises them all on a temp tree.
//!
//! # Principle (PLAN.md §3.7)
//! Verify and act on the SAME handle: never "check by path and delete by path".
//! * [`open_path`] opens by path ONLY to capture an identity or open a category root. It uses
//!   `FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS`, so a link is opened itself, not its target.
//! * [`open_rel`] opens relative to a handle (`NtCreateFile` with `RootDirectory`): paths are never resolved again from the
//!   root, so replacing an ancestor with a junction cannot redirect the operation.
//! * [`open_file_exclusive`]: `DELETE` access with `dwShareMode = 0`. Exclusive opening also checks whether the item is
//!   "in use" (`ERROR_SHARING_VIOLATION`), closes the `FILE_SHARE_DELETE` case, and prevents replacement between checking
//!   and acting.
//! * [`delete_on_handle`]: applies `FileDispositionInfoEx` (DELETE | POSIX_SEMANTICS | IGNORE_READONLY_ATTRIBUTE) to the
//!   open handle; on file systems without POSIX support, it falls back to `FileDispositionInfo` (deletes when the handle closes).
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

/// Is THIS process running elevated (does its token have `TokenElevation`)? Returns `None` if the query failed. The
/// manifest is `asInvoker`: an elevated process that launches Tesela passes its elevation to the child.
pub fn process_elevation() -> Option<bool> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `token` is writable; `GetCurrentProcess` returns a pseudo-handle that must not be closed.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    // It is closed on drop, regardless of what happens.
    let token = Handle(token);
    // SAFETY: `TOKEN_ELEVATION` is a POD (one integer); all-zero is valid.
    let mut elevation: TOKEN_ELEVATION = unsafe { std::mem::zeroed() };
    let mut returned = 0u32;
    // SAFETY: `token.0` is a valid token opened with `TOKEN_QUERY`; the pointer refers to `elevation` and its exact size is
    // passed; `returned` is writable.
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

/// Windows error code (`GetLastError` / `RtlNtStatusToDosError`).
pub type Win32 = u32;

/// `ERROR_SHARING_VIOLATION`: another handle has the object open without sharing the access we requested.
pub const SHARING_VIOLATION: Win32 = 32;
/// `ERROR_ACCESS_DENIED`.
pub const ACCESS_DENIED: Win32 = 5;
/// `ERROR_DIR_NOT_EMPTY`.
pub const DIR_NOT_EMPTY: Win32 = 145;

const SYNCHRONIZE: u32 = 0x0010_0000;
const FILE_SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
/// Enumeration buffer size (64 KiB, matching the scanner).
const BUF_BYTES: usize = 64 * 1024;

/// Identity of a file-system object: volume serial number + 128-bit `FileId` (`FileIdInfo`). This is what the user approves
/// at confirmation; paths are only used to find the object again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    pub volume_serial: u64,
    pub file_id: u128,
}

/// Kernel handle that is closed on drop.
pub struct Handle(HANDLE);

// SAFETY: a file HANDLE can be used and closed from any thread. Multiple threads can open children relative to the same
// directory handle at once (`NtCreateFile` with `RootDirectory`); enumeration and deletion run on one thread per handle
// (guaranteed by the engine).
unsafe impl Send for Handle {}
// SAFETY: see above; the system itself serializes kernel calls on the same handle.
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a valid handle opened by this module and is closed only here, exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

/// Metadata for an open object: identity, link count, attributes, sizes, and the newest of three timestamps that may
/// indicate activity (creation, write, metadata change).
#[derive(Clone, Copy, Debug)]
pub struct Meta {
    pub identity: FileIdentity,
    /// `NumberOfLinks` (`FileStandardInfo`).
    pub links: u32,
    pub attrs: u32,
    /// Reparse tag; 0 if this is not a reparse point.
    pub reparse_tag: u32,
    pub is_dir: bool,
    /// `FileStandardInfo.DeletePending`: the object has been marked for deletion and will disappear when the last handle
    /// closes (until then, its NAME remains visible and opening it returns access denied). This tells us, while a handle is
    /// open, that an object has already been deleted.
    pub delete_pending: bool,
    pub alloc: u64,
    pub logical: u64,
    /// The latest of `CreationTime`, `LastWriteTime`, and `ChangeTime` (FILETIME).
    pub newest_filetime: i64,
}

impl Meta {
    pub fn is_reparse(&self) -> bool {
        self.attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
}

/// Directory entry as returned by its listing (`FileIdExtdDirectoryInfo`).
#[derive(Clone, Debug)]
pub struct DirEntry {
    /// Name in UTF-16, exactly as stored by the system.
    pub name: Box<[u16]>,
    pub attrs: u32,
    pub reparse_tag: u32,
    pub alloc: u64,
    pub logical: u64,
    /// 128-bit `FileId` returned by the listing (compared with the handle's ID when opened).
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

/// `true` if the reparse tag belongs to a cloud file (OneDrive and similar: `IO_REPARSE_TAG_CLOUD*`).
pub fn is_cloud_tag(tag: u32) -> bool {
    tag & !0xF000 == 0x9000_001A
}

/// Path in UTF-16 with a trailing NUL and `\\?\` prefix (long paths, without Win32 normalization; therefore `/` is
/// converted to `\` first).
fn wide_long(path: &Path) -> Vec<u16> {
    let mut v: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .map(|c| if c == b'/' as u16 { b'\\' as u16 } else { c })
        .collect();
    let verbatim = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    if !v.starts_with(&verbatim) {
        if v.starts_with(&[b'\\' as u16, b'\\' as u16]) {
            // UNC form (two slashes, server, and share) -> `verbatim` form with `UNC`.
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
    // SAFETY: `GetLastError` has no preconditions.
    unsafe { GetLastError() }
}

fn nt_to_win32(status: i32) -> Win32 {
    // SAFETY: `RtlNtStatusToDosError` is a pure translation function with no preconditions.
    unsafe { RtlNtStatusToDosError(status) }
}

/// Opens `path` by path without following a final reparse point. `access` and `share` are the `CreateFileW` values.
/// Used to capture identities and open category roots; destructive operations use [`open_rel`] or
/// [`open_file_exclusive`] and verify the handle's identity.
pub fn open_path(path: &Path, access: u32, share: u32) -> Result<Handle, Win32> {
    let p = wide_long(path);
    // SAFETY: `p` is a NUL-terminated UTF-16 string that remains alive for the call; the other arguments are constants or
    // permitted null pointers (no security attributes or template).
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

/// Opens `path` only to query attributes and identity (shares everything, so it does not block other access).
pub fn open_for_query(path: &Path) -> Result<Handle, Win32> {
    open_path(path, FILE_READ_ATTRIBUTES, FILE_SHARE_ALL)
}

/// Opens a directory for listing and querying (sharing everything). It has no `DELETE` access and cannot be used to delete.
pub fn open_dir_for_listing(path: &Path) -> Result<Handle, Win32> {
    open_path(
        path,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
        FILE_SHARE_ALL,
    )
}

/// Expected type when opening a relative entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    File,
    Dir,
}

/// Opens RELATIVE to `parent` (`NtCreateFile` with `RootDirectory`): `name` is an entry name without separators.
/// Always uses `FILE_OPEN_REPARSE_POINT` (a link is opened itself) and asks the system to enforce the expected type
/// (`FILE_DIRECTORY_FILE` / `FILE_NON_DIRECTORY_FILE`): if the type changed since listing, the open fails.
fn open_rel(
    parent: &Handle,
    name: &[u16],
    access: u32,
    share: u32,
    expect: Expect,
) -> Result<Handle, Win32> {
    // An empty name or one containing separators is not an entry name, so reject it before reaching the kernel. The same
    // applies to `:`: on NTFS it opens an alternate data stream (`file:stream`) or an attribute
    // (`folder::$INDEX_ALLOCATION`), not the listed entry (H6.10).
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
    // SAFETY: IO_STATUS_BLOCK is a POD (integer and pointer); all-zero is valid.
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: `oa` refers to `us` and `name`, which remain alive for the call; `parent.0` is a valid directory handle
    // owned by `parent`; `h` and `iosb` are writable; there are no EA or allocation size.
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

/// Opens a child FILE of `parent` exclusively for deletion: `DELETE`, `dwShareMode = 0`. Returns
/// [`SHARING_VIOLATION`] if anyone else has it open (including with `FILE_SHARE_DELETE`).
pub fn open_file_exclusive(parent: &Handle, name: &[u16]) -> Result<Handle, Win32> {
    open_rel(parent, name, DELETE | FILE_READ_ATTRIBUTES, 0, Expect::File)
}

/// Opens a child DIRECTORY of `parent` for listing and, once empty, deleting through this same handle. Shares everything
/// so it can be enumerated while Explorer or antivirus software is watching it; identity is still verified.
pub fn open_dir_for_delete(parent: &Handle, name: &[u16]) -> Result<Handle, Win32> {
    open_rel(
        parent,
        name,
        DELETE | FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
        FILE_SHARE_ALL,
        Expect::Dir,
    )
}

/// Opens a child (file or directory) only to query its metadata, sharing everything.
pub fn open_child_for_query(
    parent: &Handle,
    name: &[u16],
    expect: Expect,
) -> Result<Handle, Win32> {
    open_rel(parent, name, FILE_READ_ATTRIBUTES, FILE_SHARE_ALL, expect)
}

/// Opens a child directory only for listing (without `DELETE`), sharing everything.
pub fn open_child_dir_for_listing(parent: &Handle, name: &[u16]) -> Result<Handle, Win32> {
    open_rel(
        parent,
        name,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
        FILE_SHARE_ALL,
        Expect::Dir,
    )
}

/// Why [`open_reparse_free_dir`] failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChainError {
    /// The path is not an absolute drive-letter path with normal components (UNC, relative, `..`, `\\.\`, etc.).
    BadPath,
    /// Component `component` (the name as written in the path) is a reparse point (junction, symlink, mount point, cloud):
    /// the chain cannot be trusted.
    Reparse { component: String },
    /// System error while opening a component (or reading its identity).
    Os(Win32),
}

/// Opens directory `path` by walking its components one by one from the volume root, using RELATIVE opens
/// (`NtCreateFile` + `RootDirectory` + `FILE_OPEN_REPARSE_POINT`), and requires that NO component (including the drive root
/// and final component) is a reparse point. Returns the final handle.
///
/// This checks category roots (PLAN.md §3.7.3): `GetFinalPathNameByHandleW` silently resolves links in ANCESTORS, so
/// comparing canonical and lexical paths is not enough to detect a junction above `User Data` or `%TEMP%`. Each component
/// must be checked AS WRITTEN (8.3 names in `%TEMP%` are accepted because the file system resolves them when opened).
pub fn open_reparse_free_dir(path: &Path) -> Result<Handle, ChainError> {
    open_chain_dir(path, |_| false)
}

/// Like [`open_reparse_free_dir`], but a component that is a reparse point is ACCEPTED if `tolerate` allows it (its metadata
/// is passed in). Each step's identity is still verified when opened relative to the previous one. Basket approval uses
/// this to allow cloud sync folders (M1): a cloud placeholder does not redirect name resolution, while junctions, symlinks,
/// and mount points do and remain rejected.
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
    // Request attributes only (not `FILE_LIST_DIRECTORY`): a non-elevated user may be unable to list some directories
    // (`Windows\Temp`), and the chain does not need to list anything.
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

/// Opens by path with `DELETE` access and full sharing: a pre-check for an item going to the Recycle Bin (captures identity
/// and RELEASES the handle before submitting the path to the shell).
pub fn open_path_for_rename(path: &Path) -> Result<Handle, Win32> {
    open_path(path, DELETE | FILE_READ_ATTRIBUTES, FILE_SHARE_ALL)
}

/// Opens a FILE by path exclusively with `DELETE` access and `dwShareMode = 0` (permanent deletion of a file in the basket).
pub fn open_path_exclusive(path: &Path) -> Result<Handle, Win32> {
    open_path(path, DELETE | FILE_READ_ATTRIBUTES, 0)
}

/// Opens a file by path exclusively (`dwShareMode = 0`) to READ it and delete it through the same handle.
pub fn open_path_exclusive_read(path: &Path) -> Result<Handle, Win32> {
    open_path(path, DELETE | FILE_READ_ATTRIBUTES | FILE_READ_DATA, 0)
}

/// Reads a small file (up to `max` bytes) through a handle opened with `FILE_READ_DATA`. Returns `ERROR_FILE_TOO_LARGE` if
/// it contains more than `max` bytes.
pub fn read_small(h: &Handle, max: usize) -> Result<Vec<u8>, Win32> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let mut got = 0u32;
        // SAFETY: `h.0` is a valid handle opened with `FILE_READ_DATA`; `buf` is writable for `buf.len()` bytes and `got` is
        // writable as well; there is no OVERLAPPED structure (synchronous read).
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

/// Opens a DIRECTORY by path for listing and deleting it (when empty) through this same handle.
pub fn open_path_dir_for_delete(path: &Path) -> Result<Handle, Win32> {
    open_path(
        path,
        DELETE | FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
        FILE_SHARE_ALL,
    )
}

fn query_info<T>(h: &Handle, class: i32) -> Result<T, Win32> {
    // SAFETY: `T` is one of the API's POD structures (integers and byte arrays); all-zero is valid.
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
        return Err(last_error());
    }
    Ok(v)
}

/// Metadata for the open object. `FileIdInfo` is REQUIRED (nothing is approved without identity): if the file system does
/// not support it (FAT, exFAT), its error is returned and the item is ineligible.
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

/// The open object's CURRENT size, read through its handle (only `FileStandardInfo`: one query, without reopening the path).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sizes {
    /// `AllocationSize`: bytes on disk.
    pub alloc: u64,
    /// `EndOfFile`: logical size.
    pub logical: u64,
}

impl Sizes {
    /// The larger of the two sizes: the amount counted against the Recycle Bin budget.
    pub fn charge(&self) -> u64 {
        self.alloc.max(self.logical)
    }
}

/// Current sizes of the open object (A2-B1: the shell receiver remeasures each file through its guard handle just before the
/// shell acts on it).
pub fn sizes(h: &Handle) -> Result<Sizes, Win32> {
    let std_info: FILE_STANDARD_INFO = query_info(h, FileStandardInfo)?;
    Ok(Sizes {
        alloc: std_info.AllocationSize.max(0) as u64,
        logical: std_info.EndOfFile.max(0) as u64,
    })
}

/// LONG canonical path of the open object (`GetFinalPathNameByHandleW`, normalized and without `\\?\`). Resolves ancestors
/// and 8.3 names; for a link opened with `OPEN_REPARSE_POINT`, this is the link's own path.
pub fn final_path(h: &Handle) -> Result<PathBuf, Win32> {
    let mut buf = vec![0u16; 512];
    loop {
        // SAFETY: `h.0` is a valid handle; `buf` is writable for `buf.len()` UTF-16 units.
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

/// Removes the `verbatim` prefix (`\\?\C:\x` -> `C:\x`; the `UNC` form returns to the two-slash form).
///
/// Operates on UTF-16: names with unpaired code units (valid in NTFS) are preserved without `U+FFFD`.
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

/// Lists ALL entries in a directory opened with `FILE_LIST_DIRECTORY` (excluding `.` and `..`) using
/// `FileIdExtdDirectoryInfo`. Returns the Windows error code on failure. Fields are read with checked indices: an
/// inconsistent buffer returns an error and never causes an out-of-bounds read.
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
        // SAFETY: `h.0` is a valid directory handle; `buf` is writable memory of `buf_len` bytes, aligned to 8
        // (it is a `[u64]`), as required by the API.
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
        // SAFETY: `buf` is initialized memory of `buf_len` bytes and is not modified while this view exists (the next API
        // call occurs only after the view is dropped).
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

/// Marks the open object for deletion: applies `FileDispositionInfoEx` (DELETE | POSIX_SEMANTICS | IGNORE_READONLY) to
/// THIS handle. A directory is deleted only if empty (`DIR_NOT_EMPTY` otherwise). If the file system does not support the
/// extended class (or POSIX semantics), falls back to `FileDispositionInfo`, which deletes when the handle closes.
pub(crate) fn delete_on_handle(h: &Handle) -> Result<(), Win32> {
    let info = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE
            | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
            | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    // SAFETY: `h.0` is a valid handle opened with DELETE access; the pointer refers to `info` and its size is passed.
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
    // SAFETY: same as above, using the structure for the legacy class.
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

// ---- Directory change monitoring (A3-M1) -----------------------------------------------------------------------------

/// Why a directory could not be monitored. FAILS CLOSED: without monitoring, it is not submitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchError {
    /// The open object is not the approved one (identity, type, or reparse point).
    Changed,
    /// System error while opening it, reading its identity, or starting monitoring.
    Os(Win32),
}

/// 32-bit words in the `ReadDirectoryChangesW` buffer. They are never read (any completion invalidates the measurement): the
/// buffer only needs to exist and remain DWORD-aligned while the read is pending.
const WATCH_BUF_WORDS: usize = 256;

/// Memory touched by the kernel while a read is pending: its address MUST NOT change until completion (therefore it lives in
/// a `Box` that is never moved).
struct WatchInner {
    /// Directory opened with `FILE_FLAG_OVERLAPPED`.
    handle: Handle,
    /// Structure for the pending read (its `hEvent` is `event`).
    ov: OVERLAPPED,
    /// Manual-reset event for `ov`.
    event: Handle,
    buf: [u32; WATCH_BUF_WORDS],
    /// Is a read pending in the kernel? Only then does the kernel write to `ov` and `buf`.
    pending: bool,
}

// SAFETY: `OVERLAPPED` contains a `HANDLE` (raw pointer), so it is not `Send` by itself. The kernel writes to `ov` and
// `buf` only while `pending` is true; the box does not move, and all Rust access goes through `DirWatch`'s `Mutex`.
unsafe impl Send for WatchInner {}

/// SOLO PRUEBAS: raíces bajo las que `DirWatch::start` falla con "acceso denegado" (para probar que sin vigilancia no se
/// envía). Solo actúa sobre rutas que cuelgan de la carpeta de la prueba que lo instaló.
#[cfg(test)]
static FAIL_WATCH: std::sync::Mutex<Vec<(u64, crate::paths::PathKey)>> =
    std::sync::Mutex::new(Vec::new());
#[cfg(test)]
static FAIL_WATCH_NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// SOLO PRUEBAS: guarda que quita el fallo forzado al soltarla.
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

/// SOLO PRUEBAS: hace que `DirWatch::start` falle para lo que cuelgue de `root`.
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

/// SOLO PARA PRUEBAS: preasigna `bytes` en disco (`FileAllocationInfo`) SIN cambiar el tamaño lógico (`EndOfFile`).
#[cfg(test)]
pub fn preallocate_for_test(path: &Path, bytes: i64) -> Result<(), Win32> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ALLOCATION_INFO, FILE_WRITE_DATA, FileAllocationInfo,
    };
    let h = open_path(path, FILE_WRITE_DATA | FILE_READ_ATTRIBUTES, FILE_SHARE_ALL)?;
    let info = FILE_ALLOCATION_INFO {
        AllocationSize: bytes,
    };
    // SAFETY: `h.0` es un handle válido abierto con FILE_WRITE_DATA; el puntero apunta a `info` y se pasa su tamaño.
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

/// SOLO PARA PRUEBAS: borra `path` con la semántica CLÁSICA de Windows (`FileDispositionInfo`, sin POSIX), como hace la shell:
/// si otro handle sigue abierto, el nombre queda visible como "pendiente de borrar" hasta que se cierre.
#[cfg(test)]
pub fn delete_legacy_for_test(path: &Path) -> Result<(), Win32> {
    let h = open_path(path, DELETE | FILE_READ_ATTRIBUTES, FILE_SHARE_ALL)?;
    let legacy = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: `h.0` es un handle válido abierto con DELETE; el puntero apunta a `legacy` y se pasa su tamaño.
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

/// SOLO PARA PRUEBAS: fija creación, último acceso, última escritura y cambio del objeto a `filetime` (FILETIME).
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
    // SAFETY: `h.0` es un handle válido con FILE_WRITE_ATTRIBUTES; el puntero apunta a `info` y se pasa su tamaño.
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
        // Las rutas de red se construyen por partes: el repositorio no admite rutas UNC literales.
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
        // La misma identidad por otro handle.
        let m2 = meta(&open_for_query(&f).unwrap()).unwrap();
        assert_eq!(m.identity, m2.identity);
        // Un directorio.
        let d = open_for_query(&sb.path("a")).unwrap();
        let md = meta(&d).unwrap();
        assert!(md.is_dir);
        assert_ne!(md.identity, m.identity);
        // Un enlace duro cuenta 2 enlaces y conserva la identidad.
        sb.hard_link("a/uno.bin", "a/dos.bin");
        let m3 = meta(&open_for_query(&f).unwrap()).unwrap();
        assert_eq!(m3.links, 2);
        assert_eq!(m3.identity, m.identity);
        // Un archivo inexistente falla con "no encontrado".
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
        // Abrir cada hijo RELATIVO y comparar la identidad con la del listado.
        for e in &es {
            let ex = if e.is_dir() {
                Expect::Dir
            } else {
                Expect::File
            };
            let h = open_child_for_query(&dh, &e.name, ex).unwrap();
            assert_eq!(meta(&h).unwrap().identity.file_id, e.file_id);
        }
        // El tipo impuesto por el sistema: un archivo no se abre como directorio ni al revés.
        assert!(open_child_for_query(&dh, &w("x.bin"), Expect::Dir).is_err());
        assert!(open_child_for_query(&dh, &w("sub"), Expect::File).is_err());
        // Nombres con separadores o vacíos se rechazan antes del kernel.
        assert_eq!(
            open_child_for_query(&dh, &w(r"sub\x"), Expect::File).err(),
            Some(ERROR_INVALID_PARAMETER)
        );
        assert_eq!(
            open_child_for_query(&dh, &w(""), Expect::File).err(),
            Some(ERROR_INVALID_PARAMETER)
        );
        assert!(open_child_for_query(&dh, &w(".."), Expect::Dir).is_err());
        // H6.10: un `:` (flujo de datos alternativo, `::$INDEX_ALLOCATION`) no es un nombre de entrada: se rechaza antes
        // del kernel aunque el flujo exista.
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
        // `std::fs::File::open` comparte lectura, escritura Y borrado (el caso de Chromium).
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
        // Un archivo de solo lectura se borra igual (IGNORE_READONLY).
        let mut perm = fs::metadata(&f).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&f, perm).unwrap();
        let fh = open_file_exclusive(&bh, &w("f.bin")).unwrap();
        delete_on_handle(&fh).unwrap();
        drop(fh);
        assert!(!f.exists());
        // Un directorio no vacío no se borra.
        let dh = open_dir_for_delete(&bh, &w("dir")).unwrap();
        assert_eq!(delete_on_handle(&dh).err(), Some(DIR_NOT_EMPTY));
        assert!(sb.path("b/dir/g.bin").exists());
        // Vacío sí.
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
        // Con el handle abierto en exclusiva nadie más lo abre (ni para leer).
        assert_eq!(
            fs::File::open(&f).err().and_then(|e| e.raw_os_error()),
            Some(SHARING_VIOLATION as i32)
        );
        let bytes = read_small(&h, 70_000).unwrap();
        assert_eq!(bytes.len(), 9_000);
        assert_eq!(bytes[0], 1);
        // Pasarse del tope falla.
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
        // La ruta final del propio enlace es la del enlace, no la del destino.
        assert_eq!(final_path(&h).unwrap(), sb.long("l/enlace"));
        // Y su identidad es distinta de la del destino.
        let target = meta(&open_for_query(&sb.path("t")).unwrap()).unwrap();
        assert_ne!(m.identity, target.identity);
    }

    #[test]
    fn la_cadena_se_recorre_por_componentes_y_rechaza_cualquier_reparse_point() {
        let mut sb = Sandbox::new();
        sb.mkdir("real/a/b");
        sb.mkdir("destino/dentro");
        sb.junction("real/enlace", "destino");
        // Una cadena limpia (incluida la del propio `%TEMP%` de la prueba) se abre y da la ruta canónica.
        let h = open_reparse_free_dir(&sb.path("real/a/b")).unwrap();
        assert_eq!(final_path(&h).unwrap(), sb.long("real/a/b"));
        // Con el prefijo verbatim y otra capitalización, igual.
        let upper = PathBuf::from(sb.long("real/a").to_string_lossy().to_uppercase());
        assert!(open_reparse_free_dir(&upper).is_ok());
        // Un junction como ÚLTIMO componente se rechaza (se abre a sí mismo, no a su destino).
        assert_eq!(
            open_reparse_free_dir(&sb.path("real/enlace")).err(),
            Some(ChainError::Reparse {
                component: "enlace".into()
            })
        );
        // Un junction ANCESTRO también, aunque el destino exista y sea un directorio normal.
        assert_eq!(
            open_reparse_free_dir(&sb.path("real/enlace/dentro")).err(),
            Some(ChainError::Reparse {
                component: "enlace".into()
            })
        );
        // Rutas que no son absolutas con letra de unidad, o que no existen.
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
        // Un archivo no es un directorio de la cadena.
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

    /// Espera (poco) a que la vigilancia vea algo: las altas y bajas se notifican al momento, pero el sistema entrega la
    /// finalización de forma asíncrona.
    fn sees_change(w: &DirWatch) -> bool {
        for _ in 0..40 {
            if w.changed() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        w.changed()
    }

    /// Lo que hizo ya la APROBACIÓN, que precede siempre a la vigilancia: abrir y cerrar cada directorio y cada archivo. NTFS
    /// retrasa la actualización de la copia de la fecha de escritura y del tamaño de una entrada en el índice de su padre
    /// hasta que alguien abre y cierra el objeto: medido, esa actualización pendiente llega como notificación al primer
    /// cierre. Como la medida de la aprobación ya la provocó, no queda pendiente cuando arranca la vigilancia de la ejecución.
    fn settle(sb: &Sandbox) {
        for rel in ["W", "W/sub", "W/sub/hondo"] {
            drop(open_dir_for_listing(&sb.long(rel)).unwrap());
        }
        for rel in ["W/a.bin", "W/sub/b.bin", "W/sub/hondo/c.bin"] {
            drop(open_for_query(&sb.long(rel)).unwrap());
        }
    }

    /// A3-M1: lo que NO es un cambio. El recorrido de medida (aperturas de solo lectura de cada hijo, listados, lecturas de
    /// atributos y de contenido) no genera ninguna notificación con estos filtros: si no, ninguna carpeta se enviaría nunca.
    #[test]
    fn la_vigilancia_no_ve_los_accesos_de_solo_lectura() {
        let sb = Sandbox::new();
        let (dir, id) = watch_tree(&sb);
        settle(&sb);
        let w = DirWatch::start(&dir, id).expect("arranca");
        // Lo que hace el recorrido de medida y la guarda: abrir, leer metadatos, listar.
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
        // Y lecturas de contenido y de metadatos de la biblioteca estándar.
        assert_eq!(fs::read(sb.path("W/a.bin")).unwrap().len(), 1_000);
        let _ = fs::metadata(sb.path("W/sub")).unwrap();
        let _ = fs::read_dir(sb.path("W/sub/hondo")).unwrap().count();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!w.changed(), "los accesos de solo lectura no son cambios");
    }

    /// A3-M1: lo que SÍ es un cambio, en cualquier profundidad del subárbol: alta, baja, renombrado, directorio nuevo y un
    /// archivo existente que se agranda (y se cierra).
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
            // Y no vuelve a `false`.
            assert!(w.changed(), "{name}");
        }
    }

    /// A3-M1: sin identidad aprobada no hay vigilancia (falla cerrado): identidad distinta, un directorio que no existe y un
    /// junction (no se sigue).
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
        // Un archivo no es un directorio.
        let f = sb.long("W/a.bin");
        let fid = meta(&open_for_query(&f).unwrap()).unwrap().identity;
        assert!(DirWatch::start(&f, fid).is_err());
        // Control: con la identidad buena, arranca.
        assert!(DirWatch::start(&dir, id).is_ok());
    }

    /// A3-M1: parar la vigilancia cancela la lectura pendiente y suelta el handle (el directorio se puede mover); soltarla sin
    /// parar también termina sin colgarse. Lo que cambió ANTES de parar se recuerda; lo posterior no se mira.
    #[test]
    fn parar_la_vigilancia_suelta_el_handle_y_recuerda_lo_anterior() {
        let sb = Sandbox::new();
        let (dir, id) = watch_tree(&sb);
        let w = DirWatch::start(&dir, id).expect("arranca");
        w.stop();
        assert!(!w.changed(), "sin cambios antes de parar");
        sb.write("W/despues.bin", 10);
        assert!(!w.changed(), "tras parar ya no se mira");
        // Sin handle abierto: se puede mover el directorio.
        fs::rename(sb.path("W"), sb.path("W2")).expect("el directorio se mueve");
        fs::rename(sb.path("W2"), sb.path("W")).unwrap();
        // Lo que cambió antes de parar consta después.
        let w2 = DirWatch::start(&dir, id).expect("arranca");
        sb.write("W/antes.bin", 10);
        assert!(sees_change(&w2));
        w2.stop();
        assert!(w2.changed());
        // Soltar sin parar: no se cuelga y suelta el handle.
        let t = std::time::Instant::now();
        let w3 = DirWatch::start(&dir, id).expect("arranca");
        drop(w3);
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
        fs::rename(sb.path("W"), sb.path("W3")).expect("sin handles abiertos");
    }

    /// A3-M1 (medición, no afirmación): la vigilancia pendiente NO impide mover el directorio (comparte `FILE_SHARE_DELETE`,
    /// como la guarda); aun así se para tras el último `PreDeleteItem`. Y lo que NTFS retrasa: un escritor que mantiene el
    /// archivo abierto y lo agranda. Se imprime lo observado.
    #[test]
    fn medicion_de_la_vigilancia_pendiente_y_de_un_escritor_abierto() {
        use std::io::Write;
        let sb = Sandbox::new();
        let (dir, id) = watch_tree(&sb);
        // 1. Con la lectura pendiente, ¿se puede renombrar el directorio?
        let w = DirWatch::start(&dir, id).expect("arranca");
        let moved = fs::rename(sb.path("W"), sb.path("W2"));
        eprintln!("renombrar con la vigilancia pendiente: {moved:?}");
        if moved.is_ok() {
            fs::rename(sb.path("W2"), sb.path("W")).unwrap();
        }
        drop(w);
        // 2. Un escritor con el archivo abierto: ¿cuándo se ve el crecimiento?
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
