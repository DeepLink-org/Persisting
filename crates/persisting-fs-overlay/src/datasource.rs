// SPDX-License-Identifier: GPL-2.0-or-later
//
// DataSource trait: abstraction for accessing lower layer filesystems.
// Replaces the C data_source vtable.

use crate::error::FsResult;
use crate::sys::openat2::SafeFd;
use std::os::fd::RawFd;

/// Xattr names for stat override detection.
pub const XATTR_OVERRIDE_STAT: &str = "user.fuseoverlayfs.override_stat";
pub const XATTR_PRIVILEGED_OVERRIDE_STAT: &str = "security.fuseoverlayfs.override_stat";
pub const XATTR_OVERRIDE_CONTAINERS_STAT: &str = "user.containers.override_stat";

/// Stat override mode, matching the C enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatOverrideMode {
    None,
    User,
    Privileged,
    Containers,
}

/// Iterator over directory entries.
pub trait DirIterator: Send {
    /// Returns the next directory entry, or None when exhausted.
    fn next_entry(&mut self) -> Option<DirEntry>;
}

/// A single directory entry from readdir.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: Vec<u8>,
    pub ino: u64,
    pub dtype: u8,
}

/// Abstraction for accessing a filesystem layer.
/// All implementations must use safe_openat internally for file opens,
/// ensuring RESOLVE_IN_ROOT is applied.
pub trait DataSource: Send + Sync {
    /// Initialize this data source for a specific layer.
    fn load_data_source(&mut self, path: &str) -> FsResult<()>;

    /// Check if a file exists at the given path relative to the layer root.
    fn file_exists(&self, path: &[u8]) -> FsResult<bool>;

    /// Stat a file relative to the layer root.
    fn statat(&self, path: &[u8], flags: libc::c_int, mask: libc::c_uint) -> FsResult<libc::stat>;

    /// Stat an open file descriptor.
    fn fstat(&self, fd: RawFd, path: &[u8], mask: libc::c_uint) -> FsResult<libc::stat>;

    /// Open a directory for iteration. Returns a boxed DirIterator.
    fn opendir(&self, path: &[u8]) -> FsResult<Box<dyn DirIterator>>;

    /// Open a file. Returns a SafeFd (enforces openat2/RESOLVE_IN_ROOT).
    fn openat(&self, path: &[u8], flags: libc::c_int, mode: libc::mode_t) -> FsResult<SafeFd>;

    /// List extended attributes.
    fn listxattr(&self, path: &[u8], buf: &mut [u8]) -> FsResult<usize>;

    /// Get an extended attribute value.
    fn getxattr(&self, path: &[u8], name: &str, buf: &mut [u8]) -> FsResult<usize>;

    /// Read a symbolic link.
    fn readlinkat(&self, path: &[u8]) -> FsResult<Vec<u8>>;

    /// Get an NFS file handle hash for stable inode numbers.
    /// Returns 0 if not supported.
    fn get_nfs_filehandle(&self, path: &[u8]) -> u64;

    /// Get the layer's root fd.
    fn root_fd(&self) -> RawFd;

    /// Get the device ID of the layer's filesystem.
    fn st_dev(&self) -> libc::dev_t;

    /// Get the stat override mode detected for this layer.
    fn stat_override_mode(&self) -> StatOverrideMode;

    /// Does this layer support NFS file handles?
    /// -1 = error, 0 = no, 1 = yes
    fn nfs_filehandles(&self) -> i32;
}
