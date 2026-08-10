//! Small, cross-platform syscall wrappers.
//!
//! Keeping the unsafe boundary here makes the overlay logic easier to audit.

use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn cvt(rc: libc::c_int) -> io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn timespec(time: SystemTime) -> libc::timespec {
    match time.duration_since(UNIX_EPOCH) {
        Ok(value) => libc::timespec {
            tv_sec: value.as_secs() as libc::time_t,
            tv_nsec: value.subsec_nanos() as libc::c_long,
        },
        Err(value) => {
            let value = value.duration();
            libc::timespec {
                tv_sec: -(value.as_secs() as libc::time_t) - 1,
                tv_nsec: 1_000_000_000 - value.subsec_nanos() as libc::c_long,
            }
        }
    }
}

pub fn set_times(
    path: &Path,
    atime: Option<SystemTime>,
    mtime: Option<SystemTime>,
    nofollow: bool,
) -> io::Result<()> {
    let path = c_path(path)?;
    let omit = libc::timespec {
        tv_sec: 0,
        tv_nsec: libc::UTIME_OMIT,
    };
    let times = [
        atime.map(timespec).unwrap_or(omit),
        mtime.map(timespec).unwrap_or(omit),
    ];
    let flags = if nofollow {
        libc::AT_SYMLINK_NOFOLLOW
    } else {
        0
    };
    // SAFETY: path and times are valid for the duration of the call.
    cvt(unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), times.as_ptr(), flags) })
}

pub fn chown(path: &Path, uid: u32, gid: u32, nofollow: bool) -> io::Result<()> {
    let path = c_path(path)?;
    let flags = if nofollow {
        libc::AT_SYMLINK_NOFOLLOW
    } else {
        0
    };
    // SAFETY: path is a valid NUL-terminated string.
    cvt(unsafe {
        libc::fchownat(
            libc::AT_FDCWD,
            path.as_ptr(),
            uid as libc::uid_t,
            gid as libc::gid_t,
            flags,
        )
    })
}

pub fn access(path: &Path, mask: i32) -> io::Result<()> {
    let path = c_path(path)?;
    // SAFETY: path is a valid NUL-terminated string.
    cvt(unsafe { libc::access(path.as_ptr(), mask) })
}

pub fn mknod(path: &Path, mode: u32, rdev: u32) -> io::Result<()> {
    let path = c_path(path)?;
    // SAFETY: path is a valid NUL-terminated string.
    cvt(unsafe { libc::mknod(path.as_ptr(), mode as libc::mode_t, rdev as libc::dev_t) })
}

pub struct StatFs {
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub bsize: u32,
    pub namelen: u32,
    pub frsize: u32,
}

pub fn statfs(path: &Path) -> io::Result<StatFs> {
    let path = c_path(path)?;
    // SAFETY: zero is a valid initial representation for statvfs.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: path and output pointer are valid.
    let rc = unsafe { libc::statvfs(path.as_ptr(), &mut stat) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(StatFs {
        blocks: stat.f_blocks as u64,
        bfree: stat.f_bfree as u64,
        bavail: stat.f_bavail as u64,
        files: stat.f_files as u64,
        ffree: stat.f_ffree as u64,
        bsize: stat.f_bsize as u32,
        namelen: stat.f_namemax as u32,
        frsize: stat.f_frsize as u32,
    })
}

fn xattr_buffer<F>(mut call: F) -> io::Result<Vec<u8>>
where
    F: FnMut(*mut libc::c_void, usize) -> libc::ssize_t,
{
    let needed = call(std::ptr::null_mut(), 0);
    if needed < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0_u8; needed as usize];
    if buffer.is_empty() {
        return Ok(buffer);
    }
    let actual = call(buffer.as_mut_ptr().cast(), buffer.len());
    if actual < 0 {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(actual as usize);
    Ok(buffer)
}

pub fn list_xattrs(path: &Path) -> io::Result<Vec<Vec<u8>>> {
    let path = c_path(path)?;
    #[cfg(target_os = "macos")]
    let data = xattr_buffer(|buf, size| {
        // SAFETY: buffers are either null/zero or valid writable allocations.
        unsafe { libc::listxattr(path.as_ptr(), buf.cast(), size, libc::XATTR_NOFOLLOW) }
    })?;
    #[cfg(not(target_os = "macos"))]
    let data = xattr_buffer(|buf, size| {
        // SAFETY: buffers are either null/zero or valid writable allocations.
        unsafe { libc::llistxattr(path.as_ptr(), buf.cast(), size) }
    })?;
    Ok(data
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

pub fn get_xattr(path: &Path, name: &OsStr) -> io::Result<Vec<u8>> {
    let path = c_path(path)?;
    let name = c_name(name)?;
    #[cfg(target_os = "macos")]
    {
        xattr_buffer(|buf, size| {
            // SAFETY: arguments remain valid for the duration of the call.
            unsafe {
                libc::getxattr(
                    path.as_ptr(),
                    name.as_ptr(),
                    buf,
                    size,
                    0,
                    libc::XATTR_NOFOLLOW,
                )
            }
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        xattr_buffer(|buf, size| {
            // SAFETY: arguments remain valid for the duration of the call.
            unsafe { libc::lgetxattr(path.as_ptr(), name.as_ptr(), buf, size) }
        })
    }
}

pub fn set_xattr(path: &Path, name: &OsStr, value: &[u8], flags: i32) -> io::Result<()> {
    let path = c_path(path)?;
    let name = c_name(name)?;
    #[cfg(target_os = "macos")]
    let rc = unsafe {
        // SAFETY: arguments remain valid for the duration of the call.
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            flags | libc::XATTR_NOFOLLOW,
        )
    };
    #[cfg(not(target_os = "macos"))]
    let rc = unsafe {
        // SAFETY: arguments remain valid for the duration of the call.
        libc::lsetxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            flags,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn remove_xattr(path: &Path, name: &OsStr) -> io::Result<()> {
    let path = c_path(path)?;
    let name = c_name(name)?;
    #[cfg(target_os = "macos")]
    let rc = unsafe {
        // SAFETY: arguments remain valid for the duration of the call.
        libc::removexattr(path.as_ptr(), name.as_ptr(), libc::XATTR_NOFOLLOW)
    };
    #[cfg(not(target_os = "macos"))]
    let rc = unsafe {
        // SAFETY: arguments remain valid for the duration of the call.
        libc::lremovexattr(path.as_ptr(), name.as_ptr())
    };
    cvt(rc)
}

pub fn copy_xattrs(source: &Path, destination: &Path) -> io::Result<()> {
    for name in list_xattrs(source)? {
        let name = OsStr::from_bytes(&name);
        let value = get_xattr(source, name)?;
        set_xattr(destination, name, &value, 0)?;
    }
    Ok(())
}

pub fn fsync(file: &File, datasync: bool) -> io::Result<()> {
    if datasync {
        file.sync_data()
    } else {
        file.sync_all()
    }
}

pub fn seek(file: &File, offset: i64, whence: i32) -> io::Result<i64> {
    // SAFETY: lseek only operates on the valid owned descriptor.
    let result = unsafe { libc::lseek(file.as_raw_fd(), offset, whence) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

#[cfg(target_os = "macos")]
pub fn set_flags(path: &Path, flags: u32) -> io::Result<()> {
    let path = c_path(path)?;
    // SAFETY: path is a valid NUL-terminated string.
    cvt(unsafe { libc::chflags(path.as_ptr(), flags) })
}
