//! La interfaz del `cleaner` que usa la UI. La implementación real es [`Cleaner`]; el modo `--demo` usa un respaldo
//! SIMULADO que nunca toca el disco (vive en `tesela-ui`).

use crate::basket::Basket;
use crate::exec::{ExecError, ExecOptions, Progress, Report};
use crate::plan::{ApproveOptions, Cleaner, Plan};
use crate::process::{self, Browser};
use crate::protect::Protection;
use crate::roots::Roots;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Lo que la interfaz necesita del `cleaner`.
pub trait Backend: Send + Sync {
    /// Raíces de las categorías (la UI las busca en el árbol escaneado para dar tamaños).
    fn roots(&self) -> &Roots;
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
}

impl Backend for Cleaner {
    fn roots(&self) -> &Roots {
        Cleaner::roots(self)
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
}
