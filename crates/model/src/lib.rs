//! Modelo de datos de Tesela (PLAN.md §3.2 y §3.5). Sin dependencias de Windows.
//!
//! * [`record`]: registros planos (`Record`, 32 B) y lotes (`Batch`) que entregan los escáneres.
//! * [`TreeBuilder`]: agrega bottom-up, deduplica enlaces duros, ordena los hijos una vez y asigna ids en BFS.
//! * [`Tree`]: el resultado inmutable en CSR (`Node`, 40 B) con las columnas laterales `mtime` e `hist`.
//! * [`wtf8`]: nombres en WTF-8 (los de Windows pueden tener sustitutos UTF-16 desparejados).
//!
//! Este crate no hace entrada/salida ni usa `unsafe`.

pub mod builder;
pub mod flags;
pub mod record;
pub mod tree;
pub mod types;
pub mod wtf8;

pub use builder::{BuildError, FinishOptions, SYSTEM_INFO_NAME, TreeBuilder};
pub use record::{Batch, DirOutcome, EntryData, NO_PARENT, Record};
pub use tree::{Hist, NONE, Node, NodeId, Tree, TreeStats};
pub use types::{AGE_BUCKETS, FileType, age_bucket};
