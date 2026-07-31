// SPDX-License-Identifier: GPL-2.0-or-later
//
// DirectAccess: implements DataSource for direct filesystem access.
// All file opens go through safe_openat (openat2 + RESOLVE_IN_ROOT).

use crate::datasource::*;
use crate::error::{FsError, FsResult};
use crate::sys::handle;
use crate::sys::openat2::{self, SafeFd};
use crate::sys::xattr as sxattr;
use log::warn;
use std::ffi::CString;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};

pub struct DirectAccess {
    /// Resolved absolute path to the layer root.
    resolved_path: String,
    /// Open file descriptor to the layer root directory.
    fd: Option<OwnedFd>,
    /// Device ID of the layer's filesystem.
    dev: libc::dev_t,
    /// Detected stat override mode for this layer.
    stat_override: StatOverrideMode,
    /// NFS file handle support: -1=error, 0=no, 1=yes.
    nfs_fh: i32,
    /// Pre-allocated file handle buffer for name_to_handle_at.
    fh_size: u32,
}

/// A directory iterator wrapping sys::dir::DirStream.
struct DirectDirIterator {
    stream: crate::sys::dir::DirStream,
}

impl DirIterator for DirectDirIterator {
    fn next_entry(&mut self) -> Option<DirEntry> {
        let raw = self.stream.next_entry()?;
        Some(DirEntry {
            name: raw.name,
            ino: raw.ino,
            dtype: raw.dtype,
        })
    }
}

impl DirectAccess {
    fn fd(&self) -> RawFd {
        self.fd.as_ref().map(|f| f.as_raw_fd()).unwrap_or(-1)
    }

    /// Detect NFS file handle support for a given path.
    fn probe_nfs_handles(path: &str) -> (i32, u32) {
        match handle::name_to_handle_at(libc::AT_FDCWD, path.as_bytes(), 0) {
            Ok(fh) => (1, fh.handle_bytes),
            Err(FsError(libc::ENOTSUP)) => (0, 0),
            Err(_) => (-1, 0),
        }
    }
}

impl DataSource for DirectAccess {
    fn load_data_source(&mut self, path: &str) -> FsResult<()> {
        let c_path = crate::error::cstr(path)?;
        let resolved = match crate::sys::fs::realpath(&c_path) {
            Ok(r) => r,
            Err(e) => {
                warn!("cannot resolve path {}", path);
                return Err(e);
            }
        };

        let fd = openat2::open_trusted(path, libc::O_DIRECTORY, 0)?;

        let st = crate::sys::fs::fstat(fd.as_raw_fd())?;

        let (nfs_fh, fh_size) = Self::probe_nfs_handles(&resolved);

        let mut tmp = [0u8; 64];
        let stat_override = if sxattr::fgetxattr(
            fd.as_raw_fd(),
            XATTR_PRIVILEGED_OVERRIDE_STAT,
            &mut tmp,
        )
        .is_ok()
        {
            StatOverrideMode::Privileged
        } else if sxattr::fgetxattr(fd.as_raw_fd(), XATTR_OVERRIDE_CONTAINERS_STAT, &mut tmp)
            .is_ok()
        {
            StatOverrideMode::Containers
        } else if sxattr::fgetxattr(fd.as_raw_fd(), XATTR_OVERRIDE_STAT, &mut tmp).is_ok() {
            StatOverrideMode::User
        } else {
            StatOverrideMode::None
        };

        self.resolved_path = resolved;
        self.fd = Some(fd);
        self.dev = st.st_dev as libc::dev_t;
        self.stat_override = stat_override;
        self.nfs_fh = nfs_fh;
        self.fh_size = fh_size;

        Ok(())
    }

    fn file_exists(&self, path: &[u8]) -> FsResult<bool> {
        // Open parent safely for multi-component paths
        let (parent_fd, basename) = openat2::open_parent_safe(self.fd(), path)?;
        let dirfd = parent_fd
            .as_ref()
            .map(|f| f.as_raw_fd())
            .unwrap_or(self.fd());
        Ok(openat2::file_exists_at(dirfd, basename))
    }

    fn statat(&self, path: &[u8], flags: libc::c_int, mask: libc::c_uint) -> FsResult<libc::stat> {
        // Open parent safely to prevent symlink traversal in intermediate components
        let (parent_fd, basename) = openat2::open_parent_safe(self.fd(), path)?;
        let dirfd = parent_fd
            .as_ref()
            .map(|f| f.as_raw_fd())
            .unwrap_or(self.fd());
        let c_name = crate::error::cstr_bytes(basename)?;

        match crate::sys::fs::statx(dirfd, &c_name, libc::AT_STATX_DONT_SYNC | flags, mask) {
            Ok(stx) => return Ok(crate::sys::statx::statx_to_stat(&stx)),
            Err(FsError(e)) if e == libc::ENOSYS || e == libc::EINVAL => {
                // Fall through to fstatat
            }
            Err(e) => return Err(e),
        }

        crate::sys::fs::fstatat(dirfd, &c_name, flags)
    }

    fn fstat(&self, fd: RawFd, _path: &[u8], mask: libc::c_uint) -> FsResult<libc::stat> {
        let empty = CString::new("").expect("empty CString");

        match crate::sys::fs::statx(
            fd,
            &empty,
            libc::AT_STATX_DONT_SYNC | libc::AT_EMPTY_PATH,
            mask,
        ) {
            Ok(stx) => return Ok(crate::sys::statx::statx_to_stat(&stx)),
            Err(FsError(e)) if e == libc::ENOSYS || e == libc::EINVAL => {
                // Fall through to fstat
            }
            Err(e) => return Err(e),
        }

        crate::sys::fs::fstat(fd)
    }

    fn opendir(&self, path: &[u8]) -> FsResult<Box<dyn DirIterator>> {
        let safe_fd =
            openat2::safe_openat(self.fd(), path, libc::O_DIRECTORY | libc::O_NOFOLLOW, 0)?;
        let owned_fd = safe_fd.into_owned();
        let stream = crate::sys::dir::DirStream::from_fd(owned_fd)?;
        Ok(Box::new(DirectDirIterator { stream }))
    }

    fn openat(&self, path: &[u8], flags: libc::c_int, mode: libc::mode_t) -> FsResult<SafeFd> {
        openat2::safe_openat(self.fd(), path, flags, mode)
    }

    fn listxattr(&self, path: &[u8], buf: &mut [u8]) -> FsResult<usize> {
        // Open parent safely then use /proc/self/fd/<parent_fd>/<basename>
        let (parent_fd, basename) = openat2::open_parent_safe(self.fd(), path)?;
        let dirfd = parent_fd
            .as_ref()
            .map(|f| f.as_raw_fd())
            .unwrap_or(self.fd());
        let full_path = openat2::proc_fd_path(dirfd, basename);
        sxattr::llistxattr(&full_path, buf)
    }

    fn getxattr(&self, path: &[u8], name: &str, buf: &mut [u8]) -> FsResult<usize> {
        let (parent_fd, basename) = openat2::open_parent_safe(self.fd(), path)?;
        let dirfd = parent_fd
            .as_ref()
            .map(|f| f.as_raw_fd())
            .unwrap_or(self.fd());
        let full_path = openat2::proc_fd_path(dirfd, basename);
        sxattr::lgetxattr(&full_path, name, buf)
    }

    fn readlinkat(&self, path: &[u8]) -> FsResult<Vec<u8>> {
        let (parent_fd, basename) = openat2::open_parent_safe(self.fd(), path)?;
        let dirfd = parent_fd
            .as_ref()
            .map(|f| f.as_raw_fd())
            .unwrap_or(self.fd());
        let c_name = crate::error::cstr_bytes(basename)?;
        crate::sys::fs::readlinkat(dirfd, &c_name)
    }

    fn get_nfs_filehandle(&self, path: &[u8]) -> u64 {
        if self.nfs_fh != 1 || self.fh_size == 0 {
            return 0;
        }
        // Open parent safely for multi-component paths
        let (parent_fd, basename) = match openat2::open_parent_safe(self.fd(), path) {
            Ok(v) => v,
            Err(_) => return 0,
        };
        let dirfd = parent_fd
            .as_ref()
            .map(|f| f.as_raw_fd())
            .unwrap_or(self.fd());
        match handle::name_to_handle_at(dirfd, basename, 0) {
            Ok(fh) => handle::fnv1a_hash(&fh.handle),
            Err(_) => 0,
        }
    }

    fn root_fd(&self) -> RawFd {
        self.fd()
    }

    fn st_dev(&self) -> libc::dev_t {
        self.dev
    }

    fn stat_override_mode(&self) -> StatOverrideMode {
        self.stat_override
    }

    fn nfs_filehandles(&self) -> i32 {
        self.nfs_fh
    }
}

impl DirectAccess {
    /// Override the detected stat override mode (used when xattr_permissions is configured).
    pub fn set_stat_override(&mut self, mode: StatOverrideMode) {
        self.stat_override = mode;
    }
}

/// Create a new uninitialized DirectAccess. Call load_data_source() to initialize.
pub fn new() -> DirectAccess {
    DirectAccess {
        resolved_path: String::new(),
        fd: None,
        dev: 0,
        stat_override: StatOverrideMode::None,
        nfs_fh: 0,
        fh_size: 0,
    }
}
