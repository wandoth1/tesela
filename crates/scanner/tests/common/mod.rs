//! Utilidades de los tests de integración: un árbol temporal `%TEMP%\tesela-test-<uuid>` con guarda RAII.
//!
//! SEGURIDAD: los tests solo escriben y borran dentro de esa carpeta, creada por el propio test. La guarda comprueba,
//! antes de borrar nada, que la ruta es hija directa de `%TEMP%` y que su nombre empieza por `tesela-test-`. Los
//! junctions se eliminan antes como enlaces (nunca se entra en ellos) y los destinos están dentro del mismo árbol.
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

/// `AllocationSize` de un archivo según `FileStandardInfo` sobre su propio handle (oráculo independiente del
/// listado de directorio que usa el escáner).
pub fn allocation_size(path: &Path) -> u64 {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    let f = fs::OpenOptions::new()
        .access_mode(0x80) // FILE_READ_ATTRIBUTES
        .share_mode(7)
        .custom_flags(0x0200_0000) // FILE_FLAG_BACKUP_SEMANTICS
        .open(path)
        .expect("abre el archivo para leer atributos");
    // SAFETY: FILE_STANDARD_INFO es una estructura POD (enteros): todo-ceros es un valor válido.
    let mut info: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: el handle pertenece a `f`, que sigue vivo; el puntero apunta a `info` y se pasa su tamaño exacto.
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

/// Forma corta 8.3 de una ruta EXISTENTE (`GetShortPathNameW`). Si el volumen no tiene nombres 8.3 activos devuelve
/// la ruta sin cambios.
pub fn short_path(path: &Path) -> PathBuf {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` termina en NUL; con búfer nulo y longitud 0 la API devuelve el tamaño necesario.
    let needed = unsafe { GetShortPathNameW(wide.as_ptr(), std::ptr::null_mut(), 0) };
    if needed == 0 {
        return path.to_path_buf();
    }
    let mut buf = vec![0u16; needed as usize];
    // SAFETY: `wide` termina en NUL y `buf` es escribible con `needed` unidades UTF-16.
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

    /// Raíz tal como se creó (`temp_dir()`; en algunos runners, con componentes en forma corta 8.3). La guarda de
    /// borrado trabaja con ESTA forma y no se relaja nunca.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Raíz en forma LARGA canónica (la que reconstruye el escáner: sin 8.3 y sin `\\?\` si no hace falta). Úsala
    /// para comparar con rutas que salen del escáner.
    pub fn long_root(&self) -> PathBuf {
        dunce::canonicalize(&self.root).expect("canonicaliza la raíz")
    }

    /// `rel` bajo la raíz larga.
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

    /// Crea un archivo de `size` bytes con contenido no nulo (para que no sea disperso ni residente si es grande).
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

    /// `mklink /J` (no necesita administrador). El destino debe estar dentro de este mismo árbol.
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

    /// `mklink /J` hacia un destino que puede estar FUERA de este árbol (por ejemplo, en otro `TempTree`). El enlace se
    /// quita al soltar el árbol sin tocar el destino (`remove_dir` sobre un junction borra solo el enlace).
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

    /// Activa la distinción de mayúsculas en `rel` con `fsutil` (no necesita administrador; el directorio debe estar
    /// vacío). Devuelve `false` si el sistema no lo admite: el test debe saltarse.
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
        // Comprobación real: `X` y `x` deben poder coexistir.
        let (a, b) = (dir.join("CsProbe"), dir.join("csprobe"));
        let both = fs::create_dir(&a).is_ok() && fs::create_dir(&b).is_ok();
        let _ = fs::remove_dir(&a);
        let _ = fs::remove_dir(&b);
        both
    }

    /// Deniega a todos listar el contenido de `rel` (para provocar ERROR_ACCESS_DENIED). Se restaura al soltar.
    pub fn deny_list(&mut self, rel: &str) -> bool {
        let dir = self.path(rel);
        let out = Command::new("icacls")
            .arg(&dir)
            .args(["/deny", "*S-1-1-0:(RD)"])
            .output()
            .expect("lanza icacls");
        self.acl_dirs.push(dir.clone());
        // Si el entorno se salta la denegación (privilegios de copia de seguridad), el test no puede comprobar nada.
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
            // `remove_dir` sobre un junction borra el enlace y deja el destino intacto.
            let _ = fs::remove_dir(j);
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}
