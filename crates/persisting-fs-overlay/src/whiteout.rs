// SPDX-License-Identifier: GPL-2.0-or-later
//
// Whiteout operations: create/delete whiteouts, opaque directory detection.
//
// Whiteout styles:
// - Privileged: mknod char device (0,0) at the file's path
// - Unprivileged: create a .wh.<name> file
//
// Opaque directories: detected via xattrs (trusted.overlay.opaque,
// user.overlay.opaque, user.fuseoverlayfs.opaque) or the .wh..wh..opq sentinel.

use crate::datasource::DataSource;
use crate::error::{FsError, FsResult};
use crate::sys::openat2;
use crate::xattr;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the current process can use mknod to create char device whiteouts.
static CAN_MKNOD: AtomicBool = AtomicBool::new(true);

/// Check if a directory is opaque (hides all entries from lower layers).
///
/// Returns: Ok(true) = opaque, Ok(false) = not opaque, Err = error
pub fn is_directory_opaque(ds: &dyn DataSource, path: &[u8]) -> FsResult<bool> {
    let mut buf = [0u8; 16];

    // Try xattr-based detection first
    for attr in [
        xattr::PRIVILEGED_OPAQUE_XATTR,
        xattr::UNPRIVILEGED_OPAQUE_XATTR,
        xattr::OPAQUE_XATTR,
    ] {
        match ds.getxattr(path, attr, &mut buf) {
            Ok(s) if s > 0 && buf[0] == b'y' => return Ok(true),
            _ => {}
        }
    }

    // Fallback: check for .wh..wh..opq sentinel file
    let mut opq_path = path.to_vec();
    opq_path.push(b'/');
    opq_path.extend_from_slice(xattr::OPAQUE_WHITEOUT.as_bytes());
    match ds.file_exists(&opq_path) {
        Ok(true) => Ok(true),
        Ok(false) => Ok(false),
        Err(e) if e.0 == libc::ENOENT => Ok(false),
        Err(e) => Err(e),
    }
}

/// Set a directory as opaque via xattr. Also creates the .wh..wh..opq sentinel.
pub fn set_fd_opaque(fd: RawFd) -> FsResult<()> {
    // Try privileged xattr first
    let mut xattr_set = false;
    match crate::sys::xattr::fsetxattr(fd, xattr::PRIVILEGED_OPAQUE_XATTR, b"y", 0) {
        Ok(()) => xattr_set = true,
        Err(e) if e.0 == libc::ENOTSUP => {}
        Err(e) if e.0 == libc::EPERM => {
            // Fall back to unprivileged xattr
            match crate::sys::xattr::fsetxattr(fd, xattr::UNPRIVILEGED_OPAQUE_XATTR, b"y", 0) {
                Ok(()) => xattr_set = true,
                Err(e) if e.0 == libc::ENOTSUP => {}
                Err(e) => return Err(e),
            }
        }
        Err(e) => return Err(e),
    }

    // Also create the sentinel file
    let opq_result = openat2::safe_openat(
        fd,
        xattr::OPAQUE_WHITEOUT.as_bytes(),
        libc::O_CREAT | libc::O_WRONLY | libc::O_NONBLOCK,
        0o700,
    );

    if opq_result.is_ok() || xattr_set {
        Ok(())
    } else {
        opq_result.map(|_| ())
    }
}

/// Delete a whiteout for the given name. Checks both mknod-style and .wh.-style.
///
/// When `dirfd` is Some, it should be a safely-opened parent directory fd.
/// When None, opens `parent_path` safely relative to `upper_fd`.
pub fn delete_whiteout(
    upper_fd: RawFd,
    dirfd: Option<RawFd>,
    parent_path: &[u8],
    name: &[u8],
) -> FsResult<()> {
    use crate::sys::fs;

    // Safely get parent dirfd
    let parent_safe = if dirfd.is_none() && parent_path != b"." {
        Some(openat2::safe_openat(
            upper_fd,
            parent_path,
            libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_PATH,
            0,
        )?)
    } else {
        None
    };
    let dfd = dirfd
        .or_else(|| parent_safe.as_ref().map(|f| f.as_raw_fd()))
        .unwrap_or(upper_fd);

    // Try mknod-style whiteout (char device 0,0)
    if CAN_MKNOD.load(Ordering::Relaxed) {
        let c_name = crate::error::cstr_bytes(name)?;
        if let Ok(st) = fs::fstatat(dfd, &c_name, libc::AT_SYMLINK_NOFOLLOW)
            && (st.st_mode & libc::S_IFMT) == libc::S_IFCHR
            && libc::major(st.st_rdev) == 0
            && libc::minor(st.st_rdev) == 0
        {
            fs::unlinkat(dfd, &c_name, 0)?;
        }
    }

    // Also delete .wh.-style whiteout
    let mut wh_bytes = b".wh.".to_vec();
    wh_bytes.extend_from_slice(name);
    let c_wh = crate::error::cstr_bytes(&wh_bytes)?;
    match fs::unlinkat(dfd, &c_wh, 0) {
        Ok(()) | Err(FsError(libc::ENOENT)) => {}
        Err(e) => return Err(e),
    }

    Ok(())
}

/// Create a whiteout for `name` under the directory at `parent_path`.
///
/// When can_mknod is true, creates a char device (0,0) at the original name
/// (kernel overlayfs format). Falls back to creating a regular `.wh.<name>` file.
pub fn create_whiteout(
    dirfd: RawFd,
    parent_path: &[u8],
    name: &[u8],
    can_mknod: bool,
) -> FsResult<()> {
    use crate::sys::fs;

    // Safely open the parent directory to avoid symlink traversal
    let parent_fd = if parent_path != b"." {
        Some(openat2::safe_openat(
            dirfd,
            parent_path,
            libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_PATH,
            0,
        )?)
    } else {
        None
    };
    let pfd = parent_fd.as_ref().map(|f| f.as_raw_fd()).unwrap_or(dirfd);

    if can_mknod {
        let c_name = crate::error::cstr_bytes(name)?;
        match fs::mknodat(pfd, &c_name, libc::S_IFCHR | 0o700, libc::makedev(0, 0)) {
            Ok(()) => return Ok(()),
            Err(FsError(libc::EEXIST)) => {
                // Check whether it is already a whiteout
                if let Ok(st) = fs::fstatat(pfd, &c_name, libc::AT_SYMLINK_NOFOLLOW)
                    && (st.st_mode & libc::S_IFMT) == libc::S_IFCHR
                    && libc::major(st.st_rdev) == 0
                    && libc::minor(st.st_rdev) == 0
                {
                    return Ok(());
                }
                return Err(FsError(libc::EEXIST));
            }
            Err(FsError(libc::EPERM)) | Err(FsError(libc::ENOTSUP)) => {
                // Fall through to .wh. file
                CAN_MKNOD.store(false, Ordering::Relaxed);
            }
            Err(e) => return Err(e),
        }
    }

    // Fallback: create .wh.<name> regular file in the safely-opened parent
    let mut wh_bytes = b".wh.".to_vec();
    wh_bytes.extend_from_slice(name);
    let fd = openat2::safe_openat(
        pfd,
        &wh_bytes,
        libc::O_CREAT | libc::O_WRONLY | libc::O_NONBLOCK,
        0o700,
    )?;
    drop(fd);
    Ok(())
}
