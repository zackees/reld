//! Host filesystem services: filesystem detection, preallocation, mapped-output hints,
//! positioned writes, permissions and symlinks.

pub use super::platform_imp::fs::{
    advise_huge_pages, create_symlink, filesystem_kind, invalidate_mapped_output, make_executable,
    preallocate, write_all_at,
};

/// The kind of filesystem holding a file, as far as reld's output policy cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemKind {
    /// ext2, ext3 or ext4 (they share a superblock magic).
    Ext4,
    Xfs,
    Btrfs,
    Vfat,
    Other,
}
