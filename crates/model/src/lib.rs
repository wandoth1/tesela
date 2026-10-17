//! Tesela data model (PLAN.md §3.2 and §3.5). No Windows dependencies.
//!
//! * [`record`]: flat records (`Record`, 32 B) and batches (`Batch`) delivered by scanners.
//! * [`TreeBuilder`]: aggregates bottom-up, deduplicates hard links, sorts children once, and assigns BFS ids.
//! * [`Tree`]: immutable CSR result (`Node`, 40 B) with the `mtime` and `hist` side columns.
//! * [`wtf8`]: names in WTF-8 (Windows names may contain unpaired UTF-16 surrogates).
//!
//! This crate performs no I/O and uses no `unsafe`.

pub mod builder;
pub mod flags;
pub mod overlay;
pub mod record;
pub mod tree;
pub mod types;
pub mod wtf8;

pub use builder::{BuildError, FinishOptions, SYSTEM_INFO_NAME, TreeBuilder};
pub use overlay::{Delta, Kids, Overlay, View};
pub use record::{Batch, DirOutcome, EntryData, NO_PARENT, Record};
pub use tree::{Hist, NONE, Node, NodeId, Tree, TreeStats};
pub use types::{AGE_BUCKETS, FileType, age_bucket};
