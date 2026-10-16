//! La interfaz del `cleaner` que usa la UI. La implementación real es [`Cleaner`]; el modo `--demo` usa un respaldo
//! SIMULADO que nunca toca el disco (vive en `tesela-ui`).

use crate::basket::Basket;
use crate::exec::{ExecError, ExecOptions, Progress, Report};
use crate::plan::{ApproveOptions, Cleaner, Plan};
use crate::process::{self, Browser};
use crate::protect::Protection;
use crate::roots::RootRejection;
use crate::rules::CategoryId;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Lo que la interfaz necesita del `cleaner`.
pub trait Backend: Send + Sync {
    /// Raíces (directorios) de una categoría con acción `Contents`: la UI las busca en el árbol escaneado para dar tamaños.
    fn category_roots(&self, id: CategoryId) -> Vec<PathBuf>;
    /// Las raíces de una categoría que se descartaron por un motivo que el usuario debe conocer (un enlace en su ruta...),
    /// con ese motivo. La UI muestra la categoría como "no disponible" en vez de dejarla desaparecer en silencio.
    fn unavailable_roots(&self, _id: CategoryId) -> Vec<(PathBuf, RootRejection)> {
        Vec::new()
    }
    /// La carpeta Descargas.
    fn downloads_dir(&self) -> Option<PathBuf>;
    /// La carpeta de la Papelera del usuario en la unidad `drive_root` (`C:\$Recycle.Bin\<SID>`).
    fn recycle_dir(&self, drive_root: &Path) -> Option<PathBuf>;
    /// La protección, para decidir qué se puede añadir a la cesta.
    fn protection(&self) -> Arc<Protection>;
    /// Aprueba la cesta: identidades, protección re-validada, medida.
    fn approve(&self, basket: &Basket, opts: &ApproveOptions, cancel: &Arc<AtomicBool>) -> Plan;
    /// Ejecuta un plan aprobado.
    fn execute(
        &self,
        plan: &Plan,
        opts: &ExecOptions,
        progress: &Arc<Progress>,
    ) -> Result<Report, ExecError>;
    /// Navegadores abiertos (solo UX).
    fn running_browsers(&self) -> Vec<Browser>;
    /// Carpeta del registro local y del historial (`%LOCALAPPDATA%\Tesela`).
    fn data_dir(&self) -> Option<PathBuf>;
    /// ¿Es el respaldo simulado de `--demo`?
    fn is_demo(&self) -> bool {
        false
    }
    /// ¿Se ejecuta el proceso elevado (como administrador)? Entonces la limpieza real no se hace (el análisis y la simulación,
    /// sí): la interfaz lo avisa antes de ejecutar. El respaldo simulado nunca lo está.
    fn is_elevated(&self) -> bool {
        false
    }
}

impl Backend for Cleaner {
    fn category_roots(&self, id: CategoryId) -> Vec<PathBuf> {
        self.roots().category_roots(id)
    }
    fn unavailable_roots(&self, id: CategoryId) -> Vec<(PathBuf, RootRejection)> {
        self.roots().unavailable_roots(id)
    }
    fn downloads_dir(&self) -> Option<PathBuf> {
        self.roots().downloads.clone()
    }
    fn recycle_dir(&self, drive_root: &Path) -> Option<PathBuf> {
        tesela_scanner::current_user_sid().map(|sid| drive_root.join("$Recycle.Bin").join(sid))
    }
    fn protection(&self) -> Arc<Protection> {
        Cleaner::protection(self).clone()
    }
    fn approve(&self, basket: &Basket, opts: &ApproveOptions, cancel: &Arc<AtomicBool>) -> Plan {
        Cleaner::approve(self, basket, opts, cancel)
    }
    fn execute(
        &self,
        plan: &Plan,
        opts: &ExecOptions,
        progress: &Arc<Progress>,
    ) -> Result<Report, ExecError> {
        Cleaner::execute(self, plan, opts, progress)
    }
    fn running_browsers(&self) -> Vec<Browser> {
        process::running_browsers()
    }
    fn data_dir(&self) -> Option<PathBuf> {
        self.data_dir.clone()
    }
    fn is_elevated(&self) -> bool {
        Cleaner::is_elevated(self)
    }
}
