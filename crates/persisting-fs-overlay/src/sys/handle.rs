// SPDX-License-Identifier: GPL-2.0-or-later
//
// Safe wrappers for name_to_handle_at / open_by_handle_at.
// Used for NFS file handle-based stable inode numbers (xino mode).

use crate::error::{FsError, FsResult};

use std::os::fd::RawFd;

const MAX_HANDLE_SZ: usize = 128;

/// Result of name_to_handle_at.
#[derive(Clone)]
pub struct FileHandle {
    pub handle_bytes: u32,
    pub handle: Vec<u8>,
}

/// Call name_to_handle_at to get a file handle for the given path.
pub fn name_to_handle_at(
    dirfd: RawFd,
    pathname: &[u8],
    flags: libc::c_int,
) -> FsResult<FileHandle> {
    let c_path = crate::error::cstr_bytes(pathname)?;

    // The kernel struct file_handle has a variable-length f_handle[] array.
    // We allocate enough space for the header + MAX_HANDLE_SZ bytes of handle data.
    #[repr(C)]
    struct RawFileHandle {
        handle_bytes: u32,
        handle_type: i32,
        f_handle: [u8; MAX_HANDLE_SZ],
    }

    let mut fh = RawFileHandle {
        handle_bytes: MAX_HANDLE_SZ as u32,
        handle_type: 0,
        f_handle: [0u8; MAX_HANDLE_SZ],
    };
    let mut mount_id: libc::c_int = 0;

    let ret = unsafe {
        libc::syscall(
            libc::SYS_name_to_handle_at,
            dirfd,
            c_path.as_ptr(),
            &mut fh as *mut RawFileHandle,
            &mut mount_id as *mut libc::c_int,
            flags,
        )
    };

    if ret < 0 {
        return Err(FsError::last());
    }

    let bytes = fh.handle_bytes as usize;
    Ok(FileHandle {
        handle_bytes: fh.handle_bytes,
        handle: fh.f_handle[..bytes].to_vec(),
    })
}

/// FNV-1a hash of a file handle, matching the C code's get_nfs_filehandle.
pub fn fnv1a_hash(data: &[u8]) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET_BASIS;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}
