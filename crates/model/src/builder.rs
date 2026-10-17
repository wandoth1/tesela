//! `TreeBuilder`: from flat records (in any order) to the final `Tree` (PLAN.md §3.2).
//!
//! # Algorithm order (`finish`)
//! 1. Derived flags: a directory with no batch is `INCOMPLETE`; one whose listing failed is `ERROR`.
//! 2. Provisional adjacency (sorted by parent count) and BFS traversal from the root. Entries not attached to the root
//!    (their parent never arrived) remain outside the tree and are counted as orphans.
//! 3. **Hard-link deduplication** by `FileId`, ONLY among entries reached by BFS (an orphan cannot take size away from a
//!    link in the tree). This runs BEFORE aggregation because it sets `alloc` and `logical` to 0 on duplicate links,
//!    changing directory totals.
//! 4. Bottom-up aggregation by traversing BFS in reverse: `alloc`, `logical`, `files_below`, saturating `u64` histogram
//!    accumulators, and `INCOMPLETE_BELOW`. The 50% and 80% thresholds use the `u64` values.
//! 5. Sort each directory's children ONCE by `(alloc desc, name asc)`; the synthetic `SYSTEM_INFO` node goes last. It
//!    exists only when there are no `INCOMPLETE` directories (if scan data is missing, the difference from used space
//!    is not "unexplained space").
//! 6. Assign final ids in BFS order and write `Node`, `mtime`, and `hist`.
//!
//! # Determinism
//! The `Tree` depends only on the logical tree, not provisional ids or arrival order: size ties are broken by name, and
//! among hard links the one with the lexicographically smallest full path in WTF-8 bytes retains the size (the path is
//! built from the root, excluding the root's name).
//!
//! # No recursion
//! Everything uses indexed vectors and iterative traversals: tree depth does not consume stack space.

use crate::flags;
use crate::record::{Batch, DirOutcome, NO_PARENT, Record};
use crate::tree::{Hist, Node, Tree, TreeStats};
use crate::types::{AGE_BUCKETS, FileType, age_bucket};
use rustc_hash::FxHashMap;
use std::cmp::Ordering;

/// Errors when adding a batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildError {
    /// Batch ids do not fit in `u32` or collide with `NO_PARENT`.
    IdOverflow,
    /// The name arena would exceed 4 GiB (offsets are `u32`).
    NamesOverflow,
    /// An id was used twice.
    DuplicateId(u32),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::IdOverflow => write!(f, "demasiadas entradas: los ids no caben en u32"),
            BuildError::NamesOverflow => write!(f, "los nombres superan los 4 GiB"),
            BuildError::DuplicateId(id) => {
                write!(f, "el id provisional {id} se ha usado dos veces")
            }
        }
    }
}

impl std::error::Error for BuildError {}

/// Options for [`TreeBuilder::finish`].
#[derive(Clone, Copy, Debug)]
pub struct FinishOptions {
    /// "Now" in Unix seconds, for age bands (passed explicitly for determinism).
    pub now_unix: u32,
    /// Deduplicate hard links by `FileId` (enabled by default).
    pub dedup_hard_links: bool,
    /// Volume bytes used (`total − free`) for a completed full-volume scan. If this exceeds the scanned size, the
    /// difference is added as a synthetic `SYSTEM_INFO` child of the root (PLAN.md §2.8); it is clamped to ≥ 0.
    pub volume_used: Option<u64>,
}

impl FinishOptions {
    /// Default options with the specified "now" time.
    pub fn new(now_unix: u32) -> FinishOptions {
        FinishOptions {
            now_unix,
            dedup_hard_links: true,
            volume_used: None,
        }
    }
}

/// Name of the synthetic unexplained-space node (the interface translates it according to the flag).
pub const SYSTEM_INFO_NAME: &[u8] = b"[system-data]";

#[derive(Default)]
struct BitSet(Vec<u64>);

impl BitSet {
    fn set(&mut self, i: u32) {
        let w = (i / 64) as usize;
        if self.0.len() <= w {
            self.0.resize(w + 1, 0);
        }
        self.0[w] |= 1 << (i % 64);
    }
    fn get(&self, i: u32) -> bool {
        self.0
            .get((i / 64) as usize)
            .is_some_and(|w| w & (1 << (i % 64)) != 0)
    }
}

/// Accumulates batches and builds the [`Tree`].
#[derive(Default)]
pub struct TreeBuilder {
    /// Indexed by provisional id; `parent == NO_PARENT` means no entry.
    recs: Vec<Record>,
    /// Name arena (batches are concatenated as-is, keeping siblings contiguous).
    names: Vec<u8>,
    /// Directories for which a batch arrived.
    enumerated: BitSet,
    /// Directories whose listing failed.
    failed: BitSet,
    /// Hard-link candidates: (low half of `FileId`, provisional id). 16 B per file.
    link_lo: Vec<(u64, u32)>,
    /// High half of `FileId` when nonzero (always zero on NTFS, not on ReFS).
    link_hi: FxHashMap<u32, u64>,
}

impl TreeBuilder {
    /// Empty builder.
    pub fn new() -> TreeBuilder {
        TreeBuilder::default()
    }

    /// Builder with capacity for `entries` entries and `name_bytes` bytes of names.
    pub fn with_capacity(entries: usize, name_bytes: usize) -> TreeBuilder {
        TreeBuilder {
            recs: Vec::with_capacity(entries),
            names: Vec::with_capacity(name_bytes),
            link_lo: Vec::with_capacity(entries),
            ..TreeBuilder::default()
        }
    }

    /// Entries reserved so far (provisional ids seen, including holes).
    pub fn slots(&self) -> usize {
        self.recs.len()
    }

    /// Add a batch. Any arrival order is allowed. On failure, the builder is unchanged.
    pub fn add_batch(&mut self, batch: Batch) -> Result<(), BuildError> {
        let Batch {
            dir,
            outcome,
            base_id,
            records,
            file_ids,
            names,
        } = batch;
        let end = base_id as u64 + records.len() as u64;
        if end >= NO_PARENT as u64 {
            return Err(BuildError::IdOverflow);
        }
        let off = self.names.len();
        if off as u64 + names.len() as u64 > u32::MAX as u64 {
            return Err(BuildError::NamesOverflow);
        }
        let (start, end) = (base_id as usize, end as usize);
        for (i, r) in records.iter().enumerate() {
            if r.is_present() && self.recs.get(start + i).is_some_and(Record::is_present) {
                return Err(BuildError::DuplicateId((start + i) as u32));
            }
        }
        if self.recs.len() < end {
            self.recs.resize(end, Record::HOLE);
        }
        for (i, (mut r, fid)) in records.into_iter().zip(file_ids).enumerate() {
            if !r.is_present() {
                continue;
            }
            r.name_off += off as u32;
            let id = (start + i) as u32;
            if r.flags & flags::DIR == 0 && fid != 0 {
                self.link_lo.push((fid as u64, id));
                let hi = (fid >> 64) as u64;
                if hi != 0 {
                    self.link_hi.insert(id, hi);
                }
            }
            self.recs[start + i] = r;
        }
        self.names.extend_from_slice(&names);
        if let Some(d) = dir {
            self.enumerated.set(d);
            if let DirOutcome::Error(_) = outcome {
                self.failed.set(d);
            }
        }
        Ok(())
    }

    /// Build the tree. See the module documentation for the order of steps.
    pub fn finish(self, opts: &FinishOptions) -> Tree {
        let TreeBuilder {
            mut recs,
            mut names,
            enumerated,
            failed,
            mut link_lo,
            link_hi,
        } = self;
        let n = recs.len();
        if n == 0 || !recs[0].is_present() {
            return Tree::placeholder();
        }
        let mut stats = TreeStats::default();
        recs[0].flags |= flags::DIR; // the root is always a directory

        // 1. Derived flags.
        for (id, r) in recs.iter_mut().enumerate() {
            if !r.is_present() {
                continue;
            }
            let id = id as u32;
            // Same rule as the scanner (`classify::dir_flags`): a directory reparse point with CLOUD (cloud tag) can be
            // traversed; REPARSE without CLOUD (junction, symlink, mount) is not followed and is not INCOMPLETE. An
            // unpopulated cloud directory is not traversed, so it remains INCOMPLETE.
            let followed = r.flags & flags::REPARSE == 0 || r.flags & flags::CLOUD != 0;
            if r.flags & flags::DIR != 0 && followed && !enumerated.get(id) {
                r.flags |= flags::INCOMPLETE;
            }
            if failed.get(id) {
                r.flags |= flags::ERROR;
            }
        }

        let mut first = vec![0u32; n + 1];
        let valid_parent = |recs: &[Record], p: &Record| -> Option<usize> {
            let par = p.parent as usize;
            (p.is_present()
                && par < recs.len()
                && recs[par].is_present()
                && recs[par].flags & flags::DIR != 0)
                .then_some(par)
        };
        let mut present = 0u64;
        for p in 0..n {
            if recs[p].is_present() {
                present += 1;
            }
            if p == 0 {
                continue;
            }
            if let Some(par) = valid_parent(&recs, &recs[p]) {
                first[par + 1] += 1;
            }
        }
        for i in 0..n {
            first[i + 1] += first[i];
        }
        let mut kids = vec![0u32; first[n] as usize];
        for p in 1..n {
            if let Some(par) = valid_parent(&recs, &recs[p]) {
                kids[first[par] as usize] = p as u32;
                first[par] += 1;
            }
        }
        for i in (1..=n).rev() {
            first[i] = first[i - 1];
        }
        first[0] = 0;

        let mut order: Vec<u32> = Vec::with_capacity(n + 1);
        let mut slot = vec![u32::MAX; n]; // directory index in `acc`
        let mut dir_count = 0u32;
        order.push(0);
        let mut i = 0;
        while i < order.len() {
            let p = order[i] as usize;
            i += 1;
            if recs[p].flags & flags::DIR != 0 {
                slot[p] = dir_count;
                dir_count += 1;
            }
            order.extend_from_slice(&kids[first[p] as usize..first[p + 1] as usize]);
        }
        stats.orphans = present - order.len() as u64;

        // 3. Hard-link deduplication, ONLY among entries attached to the root: an orphan (its parent never arrived) is
        // not in the tree and cannot take size away from a link that is.
        if opts.dedup_hard_links {
            let mut reached = BitSet(vec![0u64; n / 64 + 1]);
            for &p in &order {
                reached.set(p);
            }
            link_lo.retain(|&(_, id)| reached.get(id));
            dedup_hard_links(&mut recs, &names, &mut link_lo, &link_hi, &mut stats);
        }
        drop(link_lo);
        drop(link_hi);

        // 4. Bottom-up aggregation.
        let mut acc = vec![Acc::default(); dir_count as usize];
        for idx in (1..order.len()).rev() {
            let p = order[idx] as usize;
            let r = recs[p];
            let par = r.parent as usize;
            let ps = slot[par] as usize;
            if r.flags & flags::DIR != 0 {
                let child = acc[slot[p] as usize];
                let dst = &mut acc[ps];
                for (d, s) in dst.bytes.iter_mut().zip(child.bytes) {
                    *d = d.saturating_add(s);
                }
                dst.files = dst.files.saturating_add(child.files);
                if r.flags & flags::REPARSE != 0 && r.flags & flags::CLOUD == 0 {
                    stats.reparse_dirs += 1;
                } else {
                    stats.dirs += 1;
                }
                if r.flags & flags::INCOMPLETE != 0 {
                    stats.incomplete_dirs += 1;
                }
                if r.flags & flags::ERROR != 0 {
                    stats.error_dirs += 1;
                }
                // Ancestors of an incomplete directory (or one with incomplete content below) know their size is a
                // lower bound ("≥"). The child already has its own INCOMPLETE_BELOW because it is processed first.
                if r.flags & (flags::INCOMPLETE | flags::INCOMPLETE_BELOW) != 0 {
                    recs[par].flags |= flags::INCOMPLETE_BELOW;
                }
            } else if r.flags & flags::SYSTEM_INFO == 0 {
                let dst = &mut acc[ps];
                dst.files = dst.files.saturating_add(1);
                let ty = FileType::classify(
                    &names[r.name_off as usize..r.name_off as usize + r.name_len as usize],
                );
                dst.bytes[ty as usize] = dst.bytes[ty as usize].saturating_add(r.alloc);
                // An mtime of 0 is "unknown" (or before 1970): it does not count toward the oldest age band.
                if r.mtime != 0 {
                    let a = FileType::COUNT + age_bucket(opts.now_unix, r.mtime);
                    dst.bytes[a] = dst.bytes[a].saturating_add(r.alloc);
                }
                stats.files += 1;
                if r.flags & flags::REPARSE != 0 {
                    stats.reparse_files += 1;
                }
            }
            if r.flags & flags::CLOUD != 0 {
                stats.cloud_entries += 1;
            }
            let parent = &mut recs[par];
            parent.alloc = parent.alloc.saturating_add(r.alloc);
            parent.logical = parent.logical.saturating_add(r.logical);
        }
        stats.dirs += 1; // the root
        if recs[0].flags & flags::INCOMPLETE != 0 {
            stats.incomplete_dirs += 1;
        }
        if recs[0].flags & flags::ERROR != 0 {
            stats.error_dirs += 1;
        }
        if recs[0].flags & flags::CLOUD != 0 {
            stats.cloud_entries += 1;
        }
        stats.scanned_alloc = recs[0].alloc;

        // Synthetic unexplained-space node (only a child of the root).
        let synthetic = match opts.volume_used {
            // With incomplete directories, the difference is not "unexplained space"; it is data still to be scanned.
            Some(used) if used > stats.scanned_alloc && stats.incomplete_dirs == 0 => {
                let alloc = used - stats.scanned_alloc;
                stats.unexplained_alloc = alloc;
                recs[0].alloc = recs[0].alloc.saturating_add(alloc);
                let name_off = names.len() as u32;
                names.extend_from_slice(SYSTEM_INFO_NAME);
                recs.push(Record {
                    alloc,
                    logical: 0,
                    parent: 0,
                    name_off,
                    mtime: 0,
                    name_len: SYSTEM_INFO_NAME.len() as u16,
                    flags: flags::SYSTEM_INFO,
                });
                Some(n as u32)
            }
            _ => None,
        };

        // 5 and 6. Sort children and assign final ids in BFS order.
        let total = order.len() + synthetic.is_some() as usize;
        let mut nodes: Vec<Node> = Vec::with_capacity(total);
        let mut mtime: Vec<u32> = Vec::with_capacity(total);
        let mut hist: Vec<Hist> = Vec::with_capacity(total);
        let make_node = |p: usize,
                         parent: u32,
                         nodes: &mut Vec<Node>,
                         mtime: &mut Vec<u32>,
                         hist: &mut Vec<Hist>| {
            let r = &recs[p];
            let (h, files_below) = if r.flags & flags::DIR != 0 {
                let a = &acc[slot[p] as usize];
                (a.to_hist(), a.files)
            } else {
                (Hist::EMPTY, 0)
            };
            nodes.push(Node {
                alloc: r.alloc,
                logical: r.logical,
                parent,
                first_child: 0,
                child_count: 0,
                name_off: r.name_off,
                name_len: r.name_len,
                flags: r.flags,
                files_below,
            });
            mtime.push(r.mtime);
            hist.push(h);
        };
        make_node(0, NO_PARENT, &mut nodes, &mut mtime, &mut hist);
        order.clear();
        order.push(0);
        let mut f = 0usize;
        while f < order.len() {
            let p = order[f] as usize;
            if p >= n {
                f += 1;
                continue; // the synthetic node has no children
            }
            let kslice = &mut kids[first[p] as usize..first[p + 1] as usize];
            if kslice.len() > 1 {
                kslice.sort_unstable_by(|&a, &b| compare_siblings(&recs, &names, a, b));
            }
            let extra = if p == 0 { synthetic } else { None };
            let count = kslice.len() + extra.is_some() as usize;
            if count > 0 {
                nodes[f].first_child = nodes.len() as u32;
                nodes[f].child_count = count as u32;
            }
            for &c in kslice.iter().chain(extra.iter()) {
                make_node(c as usize, f as u32, &mut nodes, &mut mtime, &mut hist);
                order.push(c);
            }
            f += 1;
        }
        names.shrink_to_fit();
        nodes.shrink_to_fit();
        Tree {
            nodes,
            names,
            mtime,
            hist,
            stats,
        }
    }
}

/// Sibling order: synthetic node last; then descending `alloc`; then ascending name.
fn compare_siblings(recs: &[Record], names: &[u8], a: u32, b: u32) -> Ordering {
    let (ra, rb) = (&recs[a as usize], &recs[b as usize]);
    let (sa, sb) = (
        ra.flags & flags::SYSTEM_INFO != 0,
        rb.flags & flags::SYSTEM_INFO != 0,
    );
    sa.cmp(&sb)
        .then(rb.alloc.cmp(&ra.alloc))
        .then_with(|| name_of(names, ra).cmp(name_of(names, rb)))
}

fn name_of<'a>(names: &'a [u8], r: &Record) -> &'a [u8] {
    &names[r.name_off as usize..r.name_off as usize + r.name_len as usize]
}

/// Directory `u64` accumulators: bytes by type (7), by age band (5), and file count.
#[derive(Clone, Copy, Default)]
struct Acc {
    bytes: [u64; FileType::COUNT + AGE_BUCKETS],
    files: u32,
}

impl Acc {
    /// Percentages (for drawing) and threshold results, all calculated from the `u64` values.
    fn to_hist(self) -> Hist {
        let types = &self.bytes[..FileType::COUNT];
        let ages = &self.bytes[FileType::COUNT..];
        let total: u128 = types.iter().map(|&b| b as u128).sum();
        if total == 0 {
            return Hist::EMPTY;
        }
        let mut h = Hist::EMPTY;
        h.type_pct = percentages(types);
        // Dominant type: STRICTLY more than half of the bytes.
        for (i, &b) in types.iter().enumerate() {
            if (b as u128) * 2 > total {
                h.dominant_type = i as u8;
            }
        }
        // Age, using bytes with a known timestamp (mtime != 0): the oldest band containing >= 80% of those bytes,
        // including any older bands.
        let known: u128 = ages.iter().map(|&b| b as u128).sum();
        if known > 0 {
            h.age_pct = percentages(ages);
            let mut cum = 0u128;
            for k in (0..AGE_BUCKETS).rev() {
                cum += ages[k] as u128;
                if cum * 5 >= known * 4 {
                    h.dominant_age = k as u8;
                    break;
                }
            }
        }
        h
    }
}

/// Distribute integer percentages that sum to exactly 100 (largest remainder method).
fn percentages<const N: usize>(v: &[u64]) -> [u8; N] {
    let total: u128 = v.iter().map(|&x| x as u128).sum();
    let mut out = [0u8; N];
    if total == 0 {
        return out;
    }
    let mut rem: Vec<(u128, usize)> = Vec::with_capacity(N);
    let mut assigned = 0u32;
    for (i, &x) in v.iter().enumerate() {
        let scaled = x as u128 * 100;
        out[i] = (scaled / total) as u8;
        assigned += out[i] as u32;
        rem.push((scaled % total, i));
    }
    rem.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for &(_, i) in rem.iter().take((100 - assigned) as usize) {
        out[i] += 1;
    }
    out
}

/// Deduplicate hard links: in each group with the same `FileId`, the link with the lexicographically smallest full path
/// (WTF-8 bytes from the root) retains the size; the others get `alloc = logical = 0` and the `HARDLINK` flag.
fn dedup_hard_links(
    recs: &mut [Record],
    names: &[u8],
    link_lo: &mut [(u64, u32)],
    link_hi: &FxHashMap<u32, u64>,
    stats: &mut TreeStats,
) {
    link_lo.sort_unstable();
    let mut i = 0;
    while i < link_lo.len() {
        let mut j = i + 1;
        while j < link_lo.len() && link_lo[j].0 == link_lo[i].0 {
            j += 1;
        }
        if j - i >= 2 {
            // Same low 64 bits of FileId: distinguish by the high half (usually 0, except on ReFS).
            let mut group: Vec<(u64, u32)> = link_lo[i..j]
                .iter()
                .map(|&(_, id)| (link_hi.get(&id).copied().unwrap_or(0), id))
                .collect();
            group.sort_unstable();
            let mut a = 0;
            while a < group.len() {
                let mut b = a + 1;
                while b < group.len() && group[b].0 == group[a].0 {
                    b += 1;
                }
                if b - a >= 2 {
                    resolve_group(recs, names, group[a..b].iter().map(|&(_, id)| id), stats);
                }
                a = b;
            }
        }
        i = j;
    }
}

fn resolve_group(
    recs: &mut [Record],
    names: &[u8],
    ids: impl Iterator<Item = u32>,
    stats: &mut TreeStats,
) {
    let mut members: Vec<(Vec<u8>, u32)> = ids.map(|id| (path_key(recs, names, id), id)).collect();
    members.sort_unstable();
    stats.hardlink_groups += 1;
    for (_, id) in members.iter().skip(1) {
        let r = &mut recs[*id as usize];
        stats.hardlink_extra_entries += 1;
        stats.hardlink_alloc_saved = stats.hardlink_alloc_saved.saturating_add(r.alloc);
        stats.hardlink_logical_saved = stats.hardlink_logical_saved.saturating_add(r.logical);
        r.alloc = 0;
        r.logical = 0;
        r.flags |= flags::HARDLINK;
    }
}

/// Path of `id` from the root (excluding the root name), in WTF-8 bytes. Called only for
/// members of collision groups, which are few.
fn path_key(recs: &[Record], names: &[u8], id: u32) -> Vec<u8> {
    let mut parts: Vec<&[u8]> = Vec::new();
    let mut cur = id as usize;
    while cur != 0 && cur < recs.len() && recs[cur].is_present() && parts.len() <= recs.len() {
        parts.push(name_of(names, &recs[cur]));
        cur = recs[cur].parent as usize;
    }
    let mut out = Vec::new();
    for (k, part) in parts.iter().rev().enumerate() {
        if k > 0 {
            out.push(b'\\');
        }
        out.extend_from_slice(part);
    }
    out
}

#[cfg(test)]
mod tests;
