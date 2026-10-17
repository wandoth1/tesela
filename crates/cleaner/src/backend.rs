//! The interface used by the UI to access the `cleaner`. The real implementation is [`Cleaner`]; `--demo` uses a
//! SIMULATED backend that never touches the disk (in `tesela-ui`).

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

/// What the UI needs from the `cleaner`.
pub trait Backend: Send + Sync {
    /// Roots (directories) for a category with the `Contents` action. The UI looks them up in the scanned tree to show sizes.
    fn category_roots(&self, id: CategoryId) -> Vec<PathBuf>;
    /// Category roots rejected for a reason the user should know about (such as a link in the path), with that reason.
    /// The UI shows the category as unavailable instead of silently hiding it.
    fn unavailable_roots(&self, _id: CategoryId) -> Vec<(PathBuf, RootRejection)> {
        Vec::new()
    }
    /// The Downloads folder.
    fn downloads_dir(&self) -> Option<PathBuf>;
    /// The user's Recycle Bin folder on `drive_root` (`C:\$Recycle.Bin\<SID>`).
    fn recycle_dir(&self, drive_root: &Path) -> Option<PathBuf>;
    /// Protection rules, used to decide what can be added to the basket.
    fn protection(&self) -> Arc<Protection>;
    /// Approves the basket after checking identities, revalidating protection, and measuring contents.
    fn approve(&self, basket: &Basket, opts: &ApproveOptions, cancel: &Arc<AtomicBool>) -> Plan;
    /// Executes an approved plan.
    fn execute(
        &self,
        plan: &Plan,
        opts: &ExecOptions,
        progress: &Arc<Progress>,
    ) -> Result<Report, ExecError>;
    /// Open browsers (for UX only).
    fn running_browsers(&self) -> Vec<Browser>;
    /// Local log and history folder (`%LOCALAPPDATA%\Tesela`).
    fn data_dir(&self) -> Option<PathBuf>;
    /// Is this the simulated backend used by `--demo`?
    fn is_demo(&self) -> bool {
        false
    }
    /// Is the process running elevated (as an administrator)? If so, real cleaning is disabled (scanning and simulation
    /// remain available), and the UI warns before execution. The simulated backend is never elevated.
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
