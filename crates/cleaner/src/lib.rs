//! Limpieza de Tesela (PLAN.md §2.5 a §2.7 y §3.7): la defensa real está aquí, no en la interfaz.
//!
//! # Flujo
//! 1. La interfaz arma una [`basket::Basket`] (categorías con regla, "Tu archivo", "Contenido de X", vaciar la Papelera).
//! 2. [`plan::Cleaner::approve`] la APRUEBA: reabre cada elemento por handle y guarda su IDENTIDAD (volumen + FileId de
//!    128 bits + enlaces), re-valida la protección sobre la ruta resuelta por el handle, expande lo que contiene algo
//!    protegido, mide con el mismo recorrido que borrará y decide si cabe en la Papelera.
//! 3. [`exec`] la EJECUTA: primero vaciar la Papelera (lista enumerada al confirmar), después borrar lo definitivo
//!    ([`engine`]: apertura relativa a handles, exclusiva, `FileDispositionInfoEx` por el mismo handle) y mover a la Papelera
//!    (comprobación previa por handle, `IFileOperation` por la ruta original con un receptor de eventos propio y
//!    verificación por IDENTIDAD de lo que llegó a la Papelera; sin lápidas).
//!
//! Todo `unsafe` está en [`sys`], [`recycle`], [`volume`] y [`process`], cada llamada con su `// SAFETY:` y un test.
#![cfg(windows)]

pub mod backend;
pub mod basket;
pub mod engine;
pub mod exec;
pub mod log;
pub mod paths;
pub mod plan;
pub mod process;
pub mod protect;
pub mod recycle;
pub mod roots;
pub mod rules;
pub mod sys;
#[cfg(test)]
mod testutil;
pub mod volume;

pub use backend::Backend;
pub use basket::{AddOutcome, Basket, Source};
pub use engine::{Mode, StatsSnapshot};
pub use exec::{
    BinReport, ExecError, ExecOptions, ItemReport, ItemStatus, Phase, Progress, RecycleFail,
    Removed, Report, StopReason,
};
pub use log::{HistoryEntry, LogWriter};
pub use plan::{ApproveOptions, Cleaner, Fit, Plan, PlanItem, RejectReason, Rejected, UnfitReason};
pub use process::Browser;
pub use protect::{Protection, Verdict};
pub use roots::Roots;
pub use rules::{Action, CategoryId, Destination, Risk, Rule, Rules, rules};
