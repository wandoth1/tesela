//! Temporary test folder with RAII guard: `%TEMP%\tesela-test-<uuid>`, created here and deleted when dropped (also if
//! test panics). SECURITY: before deleting anything, it is checked that the route is a direct child of `%TEMP%` and that
//! its name begins with `tesela-test-`; tests only write inside.

use std::ops::Deref;
use std::path::{Path, PathBuf};

pub(crate) struct TestDir(PathBuf);

impl TestDir {
    /// Create the (empty) folder.
    pub fn new() -> TestDir {
        let p = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).expect("crea la carpeta temporal de prueba");
        TestDir(p)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Deref for TestDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let name_ok = self
            .0
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("tesela-test-"));
        let parent_ok = self.0.parent().is_some_and(|p| p == std::env::temp_dir());
        if name_ok && parent_ok {
            let _ = std::fs::remove_dir_all(&self.0);
        } else {
            eprintln!(
                "TestDir: ruta inesperada, no se borra nada: {}",
                self.0.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn se_crea_y_se_borra_al_soltarla_aunque_haya_contenido() {
        let p = {
            let d = TestDir::new();
            std::fs::create_dir_all(d.join("a/b")).unwrap();
            std::fs::write(d.join("a/b/x.bin"), b"x").unwrap();
            assert!(d.is_dir());
            d.path().to_path_buf()
        };
        assert!(!p.exists());
    }

    #[test]
    fn se_borra_tambien_si_la_prueba_entra_en_panico() {
        let seen = std::sync::Mutex::new(None);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let d = TestDir::new();
            *seen.lock().unwrap() = Some(d.path().to_path_buf());
            panic!("a propósito");
        }));
        assert!(r.is_err());
        let p = seen.lock().unwrap().take().unwrap();
        assert!(!p.exists());
    }
}
