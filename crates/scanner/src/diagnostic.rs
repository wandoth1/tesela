//! Windows Error Reporting (WER) reports and current-user application crash dumps: the only diagnostic folders
//! Tesela knows. READ-ONLY (`cleaner` performs cleanup and verifies each path again by handle).
//!
//! Fuentes (Microsoft Learn, revisadas el 2026-10-17):
//! * `win32/wer/collecting-user-mode-dumps` y `win32/wer/wer-settings`: la carpeta por defecto de los volcados locales de
//!   WER es `%LOCALAPPDATA%\CrashDumps` (`LocalDumps\DumpFolder`; la función no viene activada por defecto y, si se
//!   redirige, es una clave de `HKLM`: aquí solo se conoce la ruta por defecto). El archivo y la cola de informes tienen
//!   tope (`MaxArchiveCount` 1000, `MaxQueueCount` 50 por defecto).
//! * `windows-server/failover-clustering/troubleshooting-using-wer-reports` (la cola guarda lo pendiente de subir, el archivo
//!   lo ya subido) y el artículo archivado `archive/blogs/oanapl/windows-error-reporting-wer-for-developers` (la ruta por
//!   usuario `%LOCALAPPDATA%\Microsoft\Windows\WER` con `ReportArchive` y `ReportQueue`).
//!
//! De `WER` solo cuentan `ReportArchive` y `ReportQueue`: nunca la carpeta `WER` entera, `ERC`, `Temp` ni la configuración.

use crate::browser::{BrowserRoots, real_dir_under};
use crate::known::KnownKind;
use std::path::PathBuf;

/// La carpeta de WER del usuario, relativa a `%LOCALAPPDATA%`. NO es una raíz de limpieza: el `cleaner` la deniega entera y
/// solo levanta esa denegación para [`WER_REPORT_DIRS`].
pub const WER_ZONE_REL: &str = r"Microsoft\Windows\WER";

/// The only report folders under [`WER_ZONE_REL`].
pub const WER_REPORT_DIRS: [&str; 2] = ["ReportArchive", "ReportQueue"];

/// Volcados locales de bloqueo, relativa a `%LOCALAPPDATA%`.
pub const CRASH_DUMPS_REL: &str = "CrashDumps";

/// Candidate folders (whether or not they exist) and their kind: `ReportArchive` and `ReportQueue` as [`KnownKind::WerReports`], and
/// `CrashDumps` as [`KnownKind::CrashDumps`]. Returns none if `%LOCALAPPDATA%` is unknown. `cleaner` uses these paths and verifies
/// each root by handle (component by component), and shows why a path was rejected.
pub fn diagnostic_dirs(roots: &BrowserRoots) -> Vec<(KnownKind, PathBuf)> {
    let Some(local) = roots.local_appdata.as_deref() else {
        return Vec::new();
    };
    let wer = local.join(WER_ZONE_REL);
    let mut v: Vec<(KnownKind, PathBuf)> = WER_REPORT_DIRS
        .iter()
        .map(|d| (KnownKind::WerReports, wer.join(d)))
        .collect();
    v.push((KnownKind::CrashDumps, local.join(CRASH_DUMPS_REL)));
    v
}

/// Only folders that currently exist as real directories and whose ancestors below `%LOCALAPPDATA%` are also real directories (a
/// link in the path would redirect everything beneath it): paths the scanner visits first.
pub fn diagnostic_dirs_verified(roots: &BrowserRoots) -> Vec<(KnownKind, PathBuf)> {
    let Some(local) = roots.local_appdata.as_deref() else {
        return Vec::new();
    };
    diagnostic_dirs(roots)
        .into_iter()
        .filter_map(|(k, p)| real_dir_under(local, &p).map(|p| (k, p)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Carpeta temporal propia (`%TEMP%\tesela-test-<uuid>`) que se borra al soltarla, también si la prueba entra en pánico.
    struct Guard(PathBuf);
    impl Guard {
        fn new() -> Guard {
            let base = dunce::canonicalize(std::env::temp_dir()).unwrap();
            let root = base.join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            Guard(root)
        }
        fn dir(&self, rel: &str) {
            std::fs::create_dir_all(self.0.join(rel)).unwrap();
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            if self
                .0
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("tesela-test-"))
            {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn roots_of(local: &Path) -> BrowserRoots {
        BrowserRoots {
            local_appdata: Some(local.to_path_buf()),
            roaming_appdata: None,
        }
    }

    #[test]
    fn las_candidatas_son_solo_las_dos_carpetas_de_informes_y_la_de_volcados() {
        let local = Path::new(r"C:\Users\<usuario>\AppData\Local");
        let c = diagnostic_dirs(&roots_of(local));
        assert_eq!(
            c,
            vec![
                (
                    KnownKind::WerReports,
                    local.join(r"Microsoft\Windows\WER\ReportArchive")
                ),
                (
                    KnownKind::WerReports,
                    local.join(r"Microsoft\Windows\WER\ReportQueue")
                ),
                (KnownKind::CrashDumps, local.join("CrashDumps")),
            ]
        );
        // Sin `%LOCALAPPDATA%`, nada.
        assert!(diagnostic_dirs(&BrowserRoots::default()).is_empty());
        assert!(diagnostic_dirs_verified(&BrowserRoots::default()).is_empty());
    }

    #[test]
    fn las_verificadas_son_las_que_existen_y_nunca_la_configuracion_de_wer() {
        let g = Guard::new();
        g.dir("Local/Microsoft/Windows/WER/ReportArchive/Informe1");
        g.dir("Local/Microsoft/Windows/WER/ERC");
        g.dir("Local/Microsoft/Windows/WER/Temp");
        g.dir("Local/CrashDumps");
        let local = g.0.join("Local");
        let v = diagnostic_dirs_verified(&roots_of(&local));
        // `ReportQueue` no existe: no sale. `ERC`, `Temp` y la propia `WER` no salen nunca.
        assert_eq!(
            v,
            vec![
                (
                    KnownKind::WerReports,
                    local.join(r"Microsoft\Windows\WER\ReportArchive")
                ),
                (KnownKind::CrashDumps, local.join("CrashDumps")),
            ]
        );
        // Un archivo con el nombre de la carpeta no vale.
        std::fs::remove_dir_all(local.join("CrashDumps")).unwrap();
        std::fs::write(local.join("CrashDumps"), b"x").unwrap();
        let v = diagnostic_dirs_verified(&roots_of(&local));
        assert_eq!(v.len(), 1, "{v:?}");
    }
}
