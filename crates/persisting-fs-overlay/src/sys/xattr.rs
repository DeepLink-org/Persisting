// SPDX-License-Identifier: GPL-2.0-or-later
//
// Safe wrappers for xattr syscalls over libc.
//
// SAFETY (applies to all functions): Each wraps a single libc xattr FFI call.
// String arguments are converted to CString (null-terminated). Buffers are
// Rust-managed slices. Return values are checked for errors.

use crate::error::{FsError, FsResult, cstr as to_cstr, cstr_bytes};
use std::os::fd::RawFd;

pub fn fgetxattr(fd: RawFd, name: &str, buf: &mut [u8]) -> FsResult<usize> {
    let c_name = to_cstr(name)?;
    let ret =
        unsafe { libc::fgetxattr(fd, c_name.as_ptr(), buf.as_mut_ptr() as *mut _, buf.len()) };
    if ret < 0 {
        Err(FsError::last())
    } else {
        Ok(ret as usize)
    }
}

pub fn lgetxattr(path: &[u8], name: &str, buf: &mut [u8]) -> FsResult<usize> {
    let c_path = cstr_bytes(path)?;
    let c_name = to_cstr(name)?;
    let ret = unsafe {
        libc::lgetxattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            buf.as_mut_ptr() as *mut _,
            buf.len(),
        )
    };
    if ret < 0 {
        Err(FsError::last())
    } else {
        Ok(ret as usize)
    }
}

pub fn fsetxattr(fd: RawFd, name: &str, value: &[u8], flags: libc::c_int) -> FsResult<()> {
    let c_name = to_cstr(name)?;
    let ret = unsafe {
        libc::fsetxattr(
            fd,
            c_name.as_ptr(),
            value.as_ptr() as *const _,
            value.len(),
            flags,
        )
    };
    if ret < 0 {
        Err(FsError::last())
    } else {
        Ok(())
    }
}

pub fn lsetxattr(path: &[u8], name: &str, value: &[u8], flags: libc::c_int) -> FsResult<()> {
    let c_path = cstr_bytes(path)?;
    let c_name = to_cstr(name)?;
    let ret = unsafe {
        libc::lsetxattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            value.as_ptr() as *const _,
            value.len(),
            flags,
        )
    };
    if ret < 0 {
        Err(FsError::last())
    } else {
        Ok(())
    }
}

pub fn llistxattr(path: &[u8], buf: &mut [u8]) -> FsResult<usize> {
    let c_path = cstr_bytes(path)?;
    let ret = unsafe { libc::llistxattr(c_path.as_ptr(), buf.as_mut_ptr() as *mut _, buf.len()) };
    if ret < 0 {
        Err(FsError::last())
    } else {
        Ok(ret as usize)
    }
}

pub fn flistxattr(fd: RawFd, buf: &mut [u8]) -> FsResult<usize> {
    let ret = unsafe { libc::flistxattr(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
    if ret < 0 {
        Err(FsError::last())
    } else {
        Ok(ret as usize)
    }
}

pub fn lremovexattr(path: &[u8], name: &str) -> FsResult<()> {
    let c_path = cstr_bytes(path)?;
    let c_name = to_cstr(name)?;
    let ret = unsafe { libc::lremovexattr(c_path.as_ptr(), c_name.as_ptr()) };
    if ret < 0 {
        Err(FsError::last())
    } else {
        Ok(())
    }
}
