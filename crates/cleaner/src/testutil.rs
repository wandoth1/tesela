//! Test utilities: a temporary `%TEMP%\tesela-test-<uuid>` tree with an RAII guard.
//!
//! SAFETY: tests write and delete only inside that test-created folder. Before cleanup, the guard verifies
//! that the path is a direct child of `%TEMP%` and its name starts with `tesela-test-`. Junctions
//! are removed as links first (they are never traversed), and their targets are inside the same tree.
//! No test touches real `%TEMP%`, browser caches, or the real Recycle Bin except for OWN items
//! (ver `recycle`).
#![allow(dead_code, unsafe_code)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Sandbox {
    root: PathBuf,
    junctions: Vec<PathBuf>,
}

impl Sandbox {
    pub fn new() -> Sandbox {
        let root = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).expect("crea la carpeta temporal");
        Sandbox {
            root,
            junctions: Vec::new(),
        }
    }

    /// Like [`Sandbox::new`], but with the given UUID (a child test process receives it from its parent, which already monitors its
    /// `BinGuard`).
    pub fn with_uuid(uuid: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!("tesela-test-{uuid}"));
        fs::create_dir(&root).expect("crea la carpeta temporal");
        Sandbox {
            root,
            junctions: Vec::new(),
        }
    }

    /// Root as initially created (`temp_dir()`; some runners use 8.3 components).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// This folder's UUID (the suffix in `tesela-test-<uuid>`).
    pub fn uuid(&self) -> String {
        self.root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .trim_start_matches("tesela-test-")
            .to_string()
    }

    /// Root in canonical LONG form (without 8.3 components or the `\\?\` prefix).
    pub fn long_root(&self) -> PathBuf {
        dunce::canonicalize(&self.root).expect("canonicaliza la raíz")
    }

    pub fn long(&self, rel: &str) -> PathBuf {
        self.long_root().join(rel.replace('/', "\\"))
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel.replace('/', "\\"))
    }

    pub fn mkdir(&self, rel: &str) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(&p).expect("mkdir");
        p
    }

    /// Creates a `size`-byte file with non-zero contents.
    pub fn write(&self, rel: &str, size: usize) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).expect("mkdir padre");
        let mut f = fs::File::create(&p).expect("crea archivo");
        let chunk: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8 + 1).collect();
        let mut left = size;
        while left > 0 {
            let n = left.min(chunk.len());
            f.write_all(&chunk[..n]).unwrap();
            left -= n;
        }
        f.sync_all().ok();
        p
    }

    /// Creates a file and sets its modification and creation times to `age_hours` hours ago.
    pub fn write_aged(&self, rel: &str, size: usize, age_hours: u64) -> PathBuf {
        let p = self.write(rel, size);
        self.set_age(&p, age_hours);
        p
    }

    /// Sets all four object timestamps (creation, access, write, and change) to `age_hours` hours ago.
    pub fn set_age(&self, p: &Path, age_hours: u64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // FILETIME: intervalos de 100 ns desde 1601.
        let ft = ((now - age_hours * 3600) as i64 + 11_644_473_600) * 10_000_000;
        crate::sys::set_all_times_for_test(p, ft).expect("fija las fechas");
    }

    pub fn hard_link(&self, from_rel: &str, to_rel: &str) {
        let to = self.path(to_rel);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::hard_link(self.path(from_rel), to).expect("hard_link");
    }

    /// Uses `mklink /J` (does not require administrator privileges). The target must be inside this tree.
    pub fn junction(&mut self, link_rel: &str, target_rel: &str) {
        let (link, target) = (self.path(link_rel), self.path(target_rel));
        assert!(
            target.starts_with(&self.root),
            "el destino del junction debe estar dentro del árbol de prueba"
        );
        let out = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .expect("lanza cmd");
        assert!(
            out.status.success(),
            "mklink /J falló: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        self.junctions.push(link);
    }

    /// Registers a junction created elsewhere (e.g. in a race hook) so it can be removed as a link on drop.
    pub fn register_junction(&mut self, link_rel: &str) {
        self.junctions.push(self.path(link_rel));
    }

    /// Removes a junction created with [`Sandbox::junction`] (the link only; the target remains untouched).
    pub fn remove_junction(&mut self, link_rel: &str) {
        let link = self.path(link_rel);
        fs::remove_dir(&link).expect("quita el junction");
        self.junctions.retain(|j| j != &link);
    }

    /// Counts entries (files and folders) under `rel` without following links.
    pub fn count_entries(&self, rel: &str) -> usize {
        fn go(p: &Path) -> usize {
            let Ok(rd) = fs::read_dir(p) else { return 0 };
            rd.flatten()
                .map(|e| {
                    let is_real_dir = e.file_type().is_ok_and(|t| t.is_dir())
                        && !e.file_type().is_ok_and(|t| t.is_symlink());
                    1 + if is_real_dir { go(&e.path()) } else { 0 }
                })
                .sum()
        }
        go(&self.path(rel))
    }
}

impl Default for Sandbox {
    fn default() -> Sandbox {
        Sandbox::new()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let temp = std::env::temp_dir();
        let name_ok = self
            .root
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("tesela-test-"));
        let parent_ok = self.root.parent().is_some_and(|p| p == temp);
        if !(name_ok && parent_ok) {
            eprintln!(
                "Sandbox: ruta inesperada, no se borra nada: {}",
                self.root.display()
            );
            return;
        }
        for j in &self.junctions {
            // `remove_dir` on a junction deletes the link and leaves the target untouched.
            let _ = fs::remove_dir(j);
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Serializes tests that touch the real Recycle Bin (they compare the ID set before and after).
pub static BIN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquires [`BIN_LOCK`] even if another test panicked while holding it. When real Recycle Bin tests are
/// enabled, first clean up leftovers from previous runs (ONLY items whose source is under `tesela-test-*`).
pub fn bin_lock() -> std::sync::MutexGuard<'static, ()> {
    let g = BIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if crate::recycle::bintest::real_recycle_requested() {
        crate::recycle::bintest::sweep_leftovers();
    }
    g
}

/// Deletes a test-owned file or folder on drop (e.g. a `$R` item set aside inside the Recycle Bin). A
/// folder is deleted recursively, but links are never followed.
pub struct RemoveOnDrop(pub PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        match fs::symlink_metadata(&self.0) {
            Ok(m) if m.is_dir() => {
                let _ = fs::remove_dir_all(&self.0);
            }
            Ok(_) => {
                let _ = fs::remove_file(&self.0);
            }
            Err(_) => {}
        }
    }
}

/// Creates a SPARSE file with `len` logical bytes (it occupies no disk space), to test items that are "too
/// large" for the Recycle Bin without filling the disk.
pub fn sparse_file(path: &Path, len: u64) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
    let f = fs::File::create(path).expect("crea el archivo disperso");
    let mut returned = 0u32;
    // SAFETY: the handle belongs to `f`, which remains alive; FSCTL_SET_SPARSE with no input buffer marks the file
    // sparse; `returned` is writable, and there is no OVERLAPPED structure (synchronous call).
    let ok = unsafe {
        DeviceIoControl(
            f.as_raw_handle() as _,
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        ok, 0,
        "FSCTL_SET_SPARSE falló (¿volumen sin soporte de archivos dispersos?)"
    );
    f.set_len(len).expect("fija la longitud lógica");
}

/// Returns the 8.3 short form of an EXISTING path (`GetShortPathNameW`); if 8.3 names are disabled, returns the path unchanged.
pub fn short_path(path: &Path) -> PathBuf {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
    let w: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut buf = vec![0u16; 2048];
    // SAFETY: `w` is NUL-terminated; `buf` is writable for `buf.len()` UTF-16 code units.
    let n = unsafe { GetShortPathNameW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if n == 0 || n >= buf.len() {
        return path.to_path_buf();
    }
    PathBuf::from(std::ffi::OsString::from_wide(&buf[..n]))
}

unsafe extern "system" {
    /// `kernel32`: restricts DLL search for the entire process (as `tesela.exe` does at startup).
    fn SetDefaultDllDirectories(directory_flags: u32) -> i32;
}

/// `LOAD_LIBRARY_SEARCH_SYSTEM32`: search for DLLs only in `System32`.
const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

/// Calls `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` in THIS process (without adding any crate feature:
/// the `kernel32` function is declared directly). Returns `true` if Windows accepts it.
pub fn restrict_dll_search_to_system32() -> bool {
    // SAFETY: `kernel32` call with no pointers; it only changes DLL search for the process.
    unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) != 0 }
}

/// Converts a file into a generic reparse point (a third-party GUID tag with no filter to handle it), with
/// `logical` SPARSE logical bytes (no disk space). Used to test how reparse points inside a folder affect its
/// size (GM-B1) without relying on OneDrive or special privileges. Returns `false` if the volume does not support it.
pub fn make_reparse_file(path: &Path, logical: u64) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;
    let f = fs::File::create(path).expect("crea el archivo");
    // Make it sparse and set the requested logical size BEFORE adding the reparse point (afterward, it cannot be opened normally).
    let mut returned = 0u32;
    // SAFETY: `f`'s handle is live; FSCTL_SET_SPARSE has no buffer; `returned` is writable; synchronous call.
    let sparse = unsafe {
        DeviceIoControl(
            f.as_raw_handle() as _,
            windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if sparse == 0 || f.set_len(logical).is_err() {
        return false;
    }
    // REPARSE_GUID_DATA_BUFFER: etiqueta (u32), longitud de datos (u16), reservado (u16), GUID (16 bytes), datos.
    let mut buf = Vec::new();
    buf.extend_from_slice(&0x0000_0123u32.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&[0x5A; 16]);
    // SAFETY: `f`'s handle is live and writable; `buf` remains alive during the call and its length is passed; there is no
    // output buffer; `returned` is writable; synchronous call.
    let ok = unsafe {
        DeviceIoControl(
            f.as_raw_handle() as _,
            FSCTL_SET_REPARSE_POINT,
            buf.as_ptr().cast(),
            buf.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    ok != 0
}

/// Adds a generic reparse point to an EMPTY directory (a third-party GUID tag that no filter handles), to test how the
/// ancestor check handles a component that is a reparse point but not a junction or
/// symlink. Returns `false` if the volume does not support it.
pub fn make_reparse_dir(path: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;
    fs::create_dir_all(path).expect("crea el directorio");
    // `FILE_FLAG_BACKUP_SEMANTICS` (0x0200_0000) is required to open a directory for writing.
    let Ok(f) = fs::OpenOptions::new()
        .write(true)
        .custom_flags(0x0200_0000)
        .open(path)
    else {
        return false;
    };
    let mut buf = Vec::new();
    buf.extend_from_slice(&0x0000_0123u32.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&[0x5A; 16]);
    let mut returned = 0u32;
    // SAFETY: `f`'s handle is live and writable; `buf` remains alive during the call and its length is passed; there is no
    // output buffer; `returned` is writable; synchronous call.
    let ok = unsafe {
        DeviceIoControl(
            f.as_raw_handle() as _,
            FSCTL_SET_REPARSE_POINT,
            buf.as_ptr().cast(),
            buf.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    ok != 0
}
