//! Archivos de salida de las herramientas de desarrollo (`--screenshot`): nunca se pisa un archivo que ya existe,
//! salvo con `--overwrite` explícito.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Abre `path` para escribir SIN pisar nada (`create_new`: falla con `AlreadyExists` si existe, de forma atómica), o
/// reemplazándolo si `overwrite`.
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

/// Mensaje legible para un error de [`open_output`].
pub fn describe_error(path: &Path, e: &io::Error) -> String {
    if e.kind() == io::ErrorKind::AlreadyExists {
        format!(
            "{} ya existe: no se pisa. Usa otro nombre o --overwrite para reemplazarlo",
            path.display()
        )
    } else {
        format!("no se pudo crear {}: {e}", path.display())
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
        assert!(describe_error(&p, &e).starts_with("no se pudo crear"));
    }
}
