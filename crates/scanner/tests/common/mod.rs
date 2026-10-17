//! Integration test utilities: a temporary `%TEMP%\tesela-test-<uuid>` tree with an RAII guard.
//!
//! SAFETY: tests write and delete only inside this folder, which each test creates. Before deleting anything, the guard
//! checks that the path is a direct child of `%TEMP%` and that its name starts with `tesela-test-`. Junctions are removed
//! first as links (they are never entered), and their targets are inside the same tree.
#![allow(dead_code, unsafe_code)]

use std::ffi::c_void;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_STANDARD_INFO, FileStandardInfo, GetFileInformationByHandleEx, GetShortPathNameW,
};

/// A file's `AllocationSize` from `FileStandardInfo` queried on its own handle (an independent oracle from the directory
/// listing used by the scanner).
pub fn allocation_size(path: &Path) -> u64 {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    let f = fs::OpenOptions::new()
        .access_mode(0x80) // FILE_READ_ATTRIBUTES
        .share_mode(7)
        .custom_flags(0x0200_0000) // FILE_FLAG_BACKUP_SEMANTICS
        .open(path)
        .expect("abre el archivo para leer atributos");
    // SAFETY: FILE_STANDARD_INFO is a POD structure (integers): an all-zero value is valid.
    let mut info: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: the handle belongs to `f`, which is still alive; the pointer refers to `info` and its exact size is passed.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            f.as_raw_handle() as HANDLE,
            FileStandardInfo,
            &mut info as *mut _ as *mut c_void,
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    assert_ne!(ok, 0, "GetFileInformationByHandleEx falló");
    info.AllocationSize.max(0) as u64
}

/// Short 8.3 form of an EXISTING path (`GetShortPathNameW`). If the volume has no active 8.3 names, returns the path
/// unchanged.
pub fn short_path(path: &Path) -> PathBuf {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated; with a null buffer and length 0, the API returns the required size.
    let needed = unsafe { GetShortPathNameW(wide.as_ptr(), std::ptr::null_mut(), 0) };
    if needed == 0 {
        return path.to_path_buf();
    }
    let mut buf = vec![0u16; needed as usize];
    // SAFETY: `wide` is NUL-terminated and `buf` is writable for `needed` UTF-16 units.
    let n = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), needed) };
    if n == 0 || n as usize >= buf.len() {
        return path.to_path_buf();
    }
    std::ffi::OsString::from_wide(&buf[..n as usize]).into()
}

pub struct TempTree {
    root: PathBuf,
    junctions: Vec<PathBuf>,
    acl_dirs: Vec<PathBuf>,
}

impl TempTree {
    pub fn new() -> TempTree {
        let root = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).expect("crea la carpeta temporal");
        TempTree {
            root,
            junctions: Vec::new(),
            acl_dirs: Vec::new(),
        }
    }

    /// Root as created (`temp_dir()`; some runners use short 8.3 components). The deletion guard uses THIS form and is
    /// never relaxed.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Canonical LONG-form root (the form reconstructed by the scanner: no 8.3 names and no `\\?\` when unnecessary).
    /// Use it to compare paths returned by the scanner.
    pub fn long_root(&self) -> PathBuf {
        dunce::canonicalize(&self.root).expect("canonicaliza la raíz")
    }

    /// `rel` beneath the long-form root.
    pub fn long_path(&self, rel: &str) -> PathBuf {
        self.long_root().join(rel)
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    pub fn mkdir(&self, rel: &str) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(&p).expect("mkdir");
        p
    }

    /// Creates a `size`-byte file with nonzero content (so it is not sparse or resident when large).
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

    pub fn hard_link(&self, from_rel: &str, to_rel: &str) {
        let to = self.path(to_rel);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::hard_link(self.path(from_rel), to).expect("hard_link");
    }

    /// `mklink /J` (does not require an administrator). The target must be inside this same tree.
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

    /// `mklink /J` to a target that may be OUTSIDE this tree (for example, in another `TempTree`). The link is removed
    /// when the tree is dropped without touching the target (`remove_dir` on a junction removes only the link).
    pub fn junction_to(&mut self, link_rel: &str, target: &Path) {
        let link = self.path(link_rel);
        let out = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(target)
            .output()
            .expect("lanza cmd");
        assert!(
            out.status.success(),
            "mklink /J falló: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        self.junctions.push(link);
    }

    /// Enables case sensitivity on `rel` with `fsutil` (does not require an administrator; the directory must be empty).
    /// Returns `false` if the system does not support it, so the test must be skipped.
    pub fn case_sensitive(&self, rel: &str) -> bool {
        let dir = if rel.is_empty() {
            self.root.clone()
        } else {
            self.path(rel)
        };
        let ok = Command::new("fsutil")
            .args(["file", "setCaseSensitiveInfo"])
            .arg(&dir)
            .arg("enable")
            .output()
            .is_ok_and(|o| o.status.success());
        if !ok {
            return false;
        }
        // Actual check: `X` and `x` must be able to coexist.
        let (a, b) = (dir.join("CsProbe"), dir.join("csprobe"));
        let both = fs::create_dir(&a).is_ok() && fs::create_dir(&b).is_ok();
        let _ = fs::remove_dir(&a);
        let _ = fs::remove_dir(&b);
        both
    }

    /// Denies everyone permission to list the contents of `rel` (to trigger ERROR_ACCESS_DENIED). Restored on drop.
    pub fn deny_list(&mut self, rel: &str) -> bool {
        let dir = self.path(rel);
        let out = Command::new("icacls")
            .arg(&dir)
            .args(["/deny", "*S-1-1-0:(RD)"])
            .output()
            .expect("lanza icacls");
        self.acl_dirs.push(dir.clone());
        // If the environment bypasses the denial (backup privileges), the test cannot verify anything.
        out.status.success() && fs::read_dir(&dir).is_err()
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let temp = std::env::temp_dir();
        let name_ok = self
            .root
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("tesela-test-"));
        let parent_ok = self.root.parent().is_some_and(|p| p == temp);
        if !(name_ok && parent_ok) {
            eprintln!(
                "TempTree: ruta inesperada, no se borra nada: {}",
                self.root.display()
            );
            return;
        }
        for d in &self.acl_dirs {
            let _ = Command::new("icacls").arg(d).arg("/reset").output();
        }
        for j in &self.junctions {
            // `remove_dir` on a junction removes the link and leaves the target intact.
            let _ = fs::remove_dir(j);
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}
