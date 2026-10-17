//! Live progress counters (atomic): the interface reads them 15–30 times per second without blocking worker threads.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Accumulator for a top-level folder under the scanned path.
#[derive(Debug)]
pub struct TopLevel {
    /// Folder name (index 0 represents loose files in the root and has no name).
    pub name: OsString,
    /// Files seen.
    pub files: AtomicU64,
    /// Disk bytes seen (including repeated hard links; deduplication happens when building the tree).
    pub alloc: AtomicU64,
}

impl TopLevel {
    pub(crate) fn new(name: OsString) -> TopLevel {
        TopLevel {
            name,
            files: AtomicU64::new(0),
            alloc: AtomicU64::new(0),
        }
    }
}

/// State for a priority path (temporary files, caches, the user's Recycle Bin, Downloads).
#[derive(Debug)]
pub struct PriorityTarget {
    /// Canonical folder path.
    pub path: PathBuf,
    /// Disk bytes seen so far in its subtree.
    pub alloc: AtomicU64,
    /// Files seen so far in its subtree.
    pub files: AtomicU64,
    pub(crate) pending: AtomicU32,
    pub(crate) done: AtomicBool,
}

impl PriorityTarget {
    pub(crate) fn new(path: PathBuf) -> PriorityTarget {
        PriorityTarget {
            path,
            alloc: AtomicU64::new(0),
            files: AtomicU64::new(0),
            pending: AtomicU32::new(1),
            done: AtomicBool::new(false),
        }
    }

    /// Has its entire subtree been traversed? If so, `alloc` no longer changes.
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
}

/// Scan progress. All fields can be read from another thread while scanning.
#[derive(Debug, Default)]
pub struct Progress {
    /// Files seen (entries that are not directories).
    pub files: AtomicU64,
    /// Directories seen (unfollowed reparse points are not counted).
    pub dirs: AtomicU64,
    /// Disk bytes seen (without deduplicating hard links).
    pub alloc: AtomicU64,
    /// Directories that could not be listed.
    pub errors: AtomicU64,
    /// The scan finished (or was cancelled and the tree has been built).
    pub finished: AtomicBool,
    pub(crate) top: OnceLock<Vec<TopLevel>>,
    pub(crate) priority: OnceLock<Vec<PriorityTarget>>,
}

impl Progress {
    /// Per-folder accumulators for the top level (empty until the root is listed, which happens first).
    pub fn top_levels(&self) -> &[TopLevel] {
        self.top.get().map_or(&[], Vec::as_slice)
    }

    /// Priority paths being scanned (empty if none are under the scanned path).
    pub fn priority_targets(&self) -> &[PriorityTarget] {
        self.priority.get().map_or(&[], Vec::as_slice)
    }
}
