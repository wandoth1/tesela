//! Flat records and batches (`Batch`) delivered by scanners to `TreeBuilder` (PLAN.md §3.2).
//!
//! # Provisional identifiers
//! Each scanner numbers its entries as it chooses (the walker uses one `AtomicU32::fetch_add` per directory, so a
//! directory's children occupy contiguous ids `base_id..base_id + n`). `TreeBuilder` requires no ordering of batches or
//! ids. It only requires:
//! * id `0` is the root (its `parent` is written as `0`);
//! * `parent` points to a directory's provisional id;
//! * no id is used twice.
//!
//! A hole (`Batch::push_hole`) reserves an id without an entry (e.g. a directory already adopted through another path).

use crate::wtf8;

/// "No entry" marker in `Record::parent`. It is also the root's `parent` in the final `Tree`.
pub const NO_PARENT: u32 = u32::MAX;

/// Flat record for an entry (32 B). Names are stored separately in the batch or builder arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Record {
    /// Bytes allocated on disk (`AllocationSize`).
    pub alloc: u64,
    /// Logical size (`EndOfFile`).
    pub logical: u64,
    /// Provisional id of the parent directory (`NO_PARENT` = hole; `0` for the root).
    pub parent: u32,
    /// Name position in the name arena.
    pub name_off: u32,
    /// Last modification time in Unix seconds (0 if unknown).
    pub mtime: u32,
    /// Name length in WTF-8 bytes.
    pub name_len: u16,
    /// Flags from [`crate::flags`].
    pub flags: u16,
}

const _: () = assert!(
    size_of::<Record>() == 32,
    "Record must be 32 B (PLAN.md §3.2)"
);

impl Record {
    /// Empty record marking a hole.
    pub const HOLE: Record = Record {
        alloc: 0,
        logical: 0,
        parent: NO_PARENT,
        name_off: 0,
        mtime: 0,
        name_len: 0,
        flags: 0,
    };

    /// Is this a real entry (not a hole or an id that has not arrived)?
    pub fn is_present(&self) -> bool {
        self.parent != NO_PARENT
    }
}

/// Entry data when adding it to a batch.
#[derive(Clone, Copy, Debug, Default)]
pub struct EntryData {
    /// Flags from [`crate::flags`].
    pub flags: u16,
    /// Bytes allocated on disk.
    pub alloc: u64,
    /// Logical size.
    pub logical: u64,
    /// Last modification time in Unix seconds.
    pub mtime: u32,
    /// 128-bit file `FileId` (0 = unknown: this file does not participate in hard-link deduplication).
    pub file_id: u128,
}

/// Result of listing a directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirOutcome {
    /// Listing completed.
    Listed,
    /// Could not list (system error code); the directory gets the `ERROR` flag.
    Error(u32),
}

/// Entries of a directory (or a group of detached entries), with their names.
#[derive(Debug)]
pub struct Batch {
    pub(crate) dir: Option<u32>,
    pub(crate) outcome: DirOutcome,
    pub(crate) base_id: u32,
    pub(crate) records: Vec<Record>,
    pub(crate) file_ids: Vec<u128>,
    pub(crate) names: Vec<u8>,
}

impl Batch {
    /// Batch containing directory `dir`'s contents; its entries occupy ids `base_id..`.
    ///
    /// Delivering a batch for EVERY directory, even if it is empty or listing failed, lets the builder distinguish an
    /// empty directory from one that was never listed (`INCOMPLETE`).
    pub fn listing(dir: u32, base_id: u32) -> Batch {
        Batch::new(Some(dir), base_id, 0, 0)
    }

    /// Like [`Batch::listing`], reserving space for `entries` entries and `name_bytes` bytes of names.
    pub fn listing_with_capacity(
        dir: u32,
        base_id: u32,
        entries: usize,
        name_bytes: usize,
    ) -> Batch {
        Batch::new(Some(dir), base_id, entries, name_bytes)
    }

    /// Batch without an associated directory (the root, or ancestors precreated by the priority queue): each entry's
    /// `parent` is specified in [`Batch::push`].
    pub fn detached(base_id: u32) -> Batch {
        Batch::new(None, base_id, 0, 0)
    }

    /// Empty batch indicating that directory `dir` could not be listed (`code` = system error).
    pub fn failed(dir: u32, code: u32) -> Batch {
        let mut b = Batch::new(Some(dir), 0, 0, 0);
        b.outcome = DirOutcome::Error(code);
        b
    }

    fn new(dir: Option<u32>, base_id: u32, entries: usize, name_bytes: usize) -> Batch {
        Batch {
            dir,
            outcome: DirOutcome::Listed,
            base_id,
            records: Vec::with_capacity(entries),
            file_ids: Vec::with_capacity(entries),
            names: Vec::with_capacity(name_bytes),
        }
    }

    /// First id in this batch.
    pub fn base_id(&self) -> u32 {
        self.base_id
    }

    /// Number of ids occupied by this batch (entries plus holes).
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Does it occupy no ids?
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Add an entry with a WTF-8 name and return its provisional id (`base_id + index`).
    /// Names longer than 65,535 bytes are truncated (Windows names are at most 255 UTF-16 code units).
    pub fn push(&mut self, parent: u32, name_wtf8: &[u8], e: EntryData) -> u32 {
        let name = &name_wtf8[..name_wtf8.len().min(u16::MAX as usize)];
        let off = self.names.len() as u32;
        self.names.extend_from_slice(name);
        self.push_record(parent, off, name.len() as u16, e)
    }

    /// Like [`Batch::push`], but with the name in UTF-16 (as returned by Windows), converting it to WTF-8.
    pub fn push_utf16(&mut self, parent: u32, name: &[u16], e: EntryData) -> u32 {
        let off = self.names.len();
        wtf8::encode_utf16(name, &mut self.names);
        let len = (self.names.len() - off).min(u16::MAX as usize);
        self.names.truncate(off + len);
        self.push_record(parent, off as u32, len as u16, e)
    }

    /// Reserve an id without an entry.
    pub fn push_hole(&mut self) -> u32 {
        self.records.push(Record::HOLE);
        self.file_ids.push(0);
        self.base_id.saturating_add((self.records.len() - 1) as u32)
    }

    fn push_record(&mut self, parent: u32, name_off: u32, name_len: u16, e: EntryData) -> u32 {
        debug_assert_ne!(parent, NO_PARENT);
        self.records.push(Record {
            alloc: e.alloc,
            logical: e.logical,
            parent,
            name_off,
            mtime: e.mtime,
            name_len,
            flags: e.flags,
        });
        self.file_ids.push(e.file_id);
        self.base_id.saturating_add((self.records.len() - 1) as u32)
    }
}
