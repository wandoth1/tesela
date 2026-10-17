//! Output files for developer tools (`--screenshot`): existing files are never overwritten unless `--overwrite` is explicit.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// A requested `--screenshot` could not be saved; the process exits with a nonzero code.
static CAPTURE_FAILED: AtomicBool = AtomicBool::new(false);

/// Records that the screenshot failed (checked with [`failed`] on exit).
#[cfg_attr(not(feature = "capture"), allow(dead_code))]
pub fn mark_failed() {
    CAPTURE_FAILED.store(true, Ordering::SeqCst);
}

/// Did the screenshot fail?
pub fn failed() -> bool {
    CAPTURE_FAILED.load(Ordering::SeqCst)
}

/// Preflight before opening the window: do not overwrite an existing file (unless `overwrite` is set), the destination
/// must not be a directory, and its parent directory must exist. An unsavable screenshot therefore fails immediately,
/// before the window is rendered.
pub fn preflight(path: &Path, overwrite: bool) -> Result<(), String> {
    if path.is_dir() {
        return Err(format!("{} is a directory, not a file", path.display()));
    }
    if !overwrite && path.exists() {
        return Err(describe_error(
            path,
            &io::Error::from(io::ErrorKind::AlreadyExists),
        ));
    }
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    if !parent.is_dir() {
        return Err(format!(
            "cannot create {}: destination directory does not exist",
            path.display()
        ));
    }
    Ok(())
}

/// Opens `path` for writing WITHOUT overwriting anything (`create_new`: atomically fails with `AlreadyExists` if it exists),
/// or replaces it when `overwrite` is set.
#[cfg_attr(not(feature = "capture"), allow(dead_code))]
pub fn open_output(path: &Path, overwrite: bool) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true);
    if overwrite {
        o.create(true).truncate(true);
    } else {
        o.create_new(true);
    }
    o.open(path)
}

/// Human-readable message for an [`open_output`] error.
pub fn describe_error(path: &Path, e: &io::Error) -> String {
    if e.kind() == io::ErrorKind::AlreadyExists {
        format!(
            "{} already exists and was not overwritten. Choose another name or use --overwrite to replace it",
            path.display()
        )
    } else {
        format!("could not create {}: {e}", path.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Carpeta temporal propia con guarda RAII (solo se borra `%TEMP%\tesela-test-*` creada aquí).
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Temp {
            let p = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&p).unwrap();
            Temp(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn la_comprobacion_previa_rechaza_destinos_imposibles() {
        let t = Temp::new();
        // Carpeta de destino inexistente.
        let e = preflight(&t.0.join("no-existe").join("x.png"), false).unwrap_err();
        assert!(e.contains("does not exist"), "{e}");
        assert!(preflight(&t.0.join("no-existe").join("x.png"), true).is_err());
        // Destino que es una carpeta.
        assert!(preflight(&t.0, true).unwrap_err().contains("directory"));
        // Archivo nuevo: bien. Existente: solo con overwrite.
        let p = t.0.join("c.png");
        assert!(preflight(&p, false).is_ok());
        std::fs::write(&p, b"x").unwrap();
        assert!(preflight(&p, false).unwrap_err().contains("--overwrite"));
        assert!(preflight(&p, true).is_ok());
        // Sin carpeta en la ruta: es el directorio actual.
        assert!(preflight(Path::new("no-existe-tesela-test.png"), false).is_ok());
    }

    #[test]
    fn no_pisa_un_archivo_existente_y_con_overwrite_lo_reemplaza() {
        let t = Temp::new();
        let p = t.0.join("captura.png");
        open_output(&p, false).unwrap().write_all(b"uno").unwrap();
        let e = open_output(&p, false).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert!(describe_error(&p, &e).contains("--overwrite"));
        assert_eq!(std::fs::read(&p).unwrap(), b"uno");
        open_output(&p, true).unwrap().write_all(b"dos").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"dos");
        let e = open_output(&t.0.join("no-existe").join("x.png"), false).unwrap_err();
        assert!(describe_error(&p, &e).starts_with("could not create"));
    }
}
