//! Tesela cleanup (PLAN.md §2.5–§2.7 and §3.7): the actual safeguards are here, not in the UI.
//!
//! # Flujo
//! 1. The UI builds a [`basket::Basket`] (rule-based categories, "Your file", "Contents of X", and emptying the Recycle Bin).
//! 2. [`plan::Cleaner::approve`] APPROVES it: reopens each item by handle and stores its IDENTITY (volume + 128-bit
//!    FileId + link count), revalidates protection against the path resolved from the handle, expands anything containing
//!    a protected item, measures with the same traversal used for deletion, and decides whether it fits in the Recycle Bin.
//! 3. [`exec`] EXECUTES it: first empty the Recycle Bin (the list enumerated at confirmation), then permanently delete
//!    ([`engine`]: relative handle opens, exclusive access, `FileDispositionInfoEx` through the same handle) and move items
//!    to the Recycle Bin (pre-check by handle, `IFileOperation` on the original path with a custom event sink, and identity
//!    verification of what reached the Recycle Bin; without tombstone renaming).
//!
//! All `unsafe` code is in [`sys`], [`recycle`], [`volume`], and [`process`]; each call has a `// SAFETY:` explanation and a test.
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
pub use basket::{AddOutcome, Basket, BatchKind, Source};
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
