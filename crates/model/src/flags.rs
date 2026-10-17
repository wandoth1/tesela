//! Node flags (`Record::flags` and `Node::flags`).

/// This is a directory (including directory reparse points, which also have [`REPARSE`]).
pub const DIR: u16 = 1 << 0;
/// Reparse point (junction, symlink, mount point...). Directory reparse points are not followed and have size 0.
pub const REPARSE: u16 = 1 << 1;
/// Cloud file or directory (OneDrive placeholder, etc.): its `alloc` may be 0 while `logical` is nonzero.
pub const CLOUD: u16 = 1 << 2;
/// Hard link that does NOT retain the size: `alloc` and `logical` are 0 because another link already counts it.
pub const HARDLINK: u16 = 1 << 3;
/// Directory whose contents were not fully listed (scan stopped): its size is a lower bound ("≥ X").
pub const INCOMPLETE: u16 = 1 << 4;
/// Synthetic "Windows and other system data" node (unexplained space). Not a real file.
pub const SYSTEM_INFO: u16 = 1 << 5;
/// The directory could not be listed (permissions, I/O error...).
pub const ERROR: u16 = 1 << 6;
/// A descendant is `INCOMPLETE`: this directory's size is a lower bound ("≥") even if it was fully listed itself.
/// The builder computes this during aggregation, propagating `INCOMPLETE` and `INCOMPLETE_BELOW` to the root.
pub const INCOMPLETE_BELOW: u16 = 1 << 7;
