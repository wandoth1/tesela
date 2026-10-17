//! Windows Error Reporting (WER) reports and current-user application crash dumps: the only diagnostic folders
//! Tesela knows. READ-ONLY (`cleaner` performs cleanup and verifies each path again by handle).
//!
//! Sources (Microsoft Learn, reviewed on 2026-10-17):
//! * `win32/wer/collecting-user-mode-dumps` and `win32/wer/wer-settings`: the default folder for local WER dumps is
//!   `%LOCALAPPDATA%\CrashDumps` (`LocalDumps\DumpFolder`; the feature is disabled by default and, if redirected, is
//!   configured under `HKLM`: only the default path is known here). The archive and report queue have limits
//!   (`MaxArchiveCount` 1000, `MaxQueueCount` 50 by default).
//! * `windows-server/failover-clustering/troubleshooting-using-wer-reports` (the queue stores reports waiting to upload,
//!   the archive stores uploaded reports) and the archived article
//!   `archive/blogs/oanapl/windows-error-reporting-wer-for-developers` (per-user path `%LOCALAPPDATA%\Microsoft\Windows\WER`
//!   containing `ReportArchive` and `ReportQueue`).
//!
//! Under `WER`, only `ReportArchive` and `ReportQueue` are included: never the entire `WER` folder, `ERC`, `Temp`, or settings.

use crate::browser::{BrowserRoots, real_dir_under};
use crate::known::KnownKind;
use std::path::PathBuf;

/// The user's WER folder, relative to `%LOCALAPPDATA%`. It is NOT a cleanup root: `cleaner` denies it entirely and lifts
/// that denial only for [`WER_REPORT_DIRS`].
pub const WER_ZONE_REL: &str = r"Microsoft\Windows\WER";

/// The only report folders under [`WER_ZONE_REL`].
pub const WER_REPORT_DIRS: [&str; 2] = ["ReportArchive", "ReportQueue"];

/// Local crash dumps, relative to `%LOCALAPPDATA%`.
pub const CRASH_DUMPS_REL: &str = "CrashDumps";

/// Candidate folders (whether or not they exist) and their kind: `ReportArchive` and `ReportQueue` as [`KnownKind::WerReports`],
/// and `CrashDumps` as [`KnownKind::CrashDumps`]. Returns none if `%LOCALAPPDATA%` is unknown. `cleaner` uses these paths,
/// verifies each root by handle (component by component), and explains why a path was rejected.
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

/// Only folders that currently exist as real directories and whose ancestors below `%LOCALAPPDATA%` are also real directories
/// (a link in the path would redirect everything beneath it): these are the paths the scanner visits first.
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
