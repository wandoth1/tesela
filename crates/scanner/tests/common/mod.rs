//! Utilidades de los tests de integración: un árbol temporal `%TEMP%\tesela-test-<uuid>` con guarda RAII.
//!
//! SEGURIDAD: los tests solo escriben y borran dentro de esa carpeta, creada por el propio test. La guarda comprueba,
//! antes de borrar nada, que la ruta es hija directa de `%TEMP%` y que su nombre empieza por `tesela-test-`. Los
//! junctions se eliminan antes como enlaces (nunca se entra en ellos) y los destinos están dentro del mismo árbol.
#![allow(dead_code)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

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

    pub fn root(&self) -> &Path {
        &self.root
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
