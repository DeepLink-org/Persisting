// SPDX-License-Identifier: GPL-2.0-or-later
//
// Safe directory iteration wrapper around libc DIR*.
// Encapsulates all unsafe DIR* operations.

use crate::error::{FsError, FsResult};
use std::os::fd::{IntoRawFd, OwnedFd, RawFd};

/// A directory entry returned by DirIterator.
pub struct RawDirEntry {
    pub name: Vec<u8>,
    pub ino: u64,
    pub dtype: u8,
}

/// Safe wrapper around a libc DIR* for directory iteration.
///
/// # Safety invariant
/// DIR* is not thread-safe, but this type is used exclusively within
/// the OverlayFs big-lock mutex, so only one thread accesses it at a time.
pub struct DirStream {
    dir: *mut libc::DIR,
}

// Safety: DIR* is not thread-safe for concurrent access, but DirStream
// enforces exclusive access via `&mut self` on `next_entry()`. The Send
// impl allows the owning thread to change (e.g. across FUSE worker threads),
// which is safe because only one thread can access the DIR* at a time.
unsafe impl Send for DirStream {}

impl DirStream {
    /// Open a directory from an OwnedFd using fdopendir.
    /// Takes ownership of the fd (fdopendir takes ownership of the fd).
    pub fn from_fd(fd: OwnedFd) -> FsResult<Self> {
        let raw = fd.into_raw_fd();
        // SAFETY: raw is a valid fd from OwnedFd. fdopendir takes ownership.
        let dir = unsafe { libc::fdopendir(raw) };
        if dir.is_null() {
            // fdopendir failed, we need to close the fd ourselves
            // SAFETY: raw is still a valid fd since fdopendir failed.
            unsafe { libc::close(raw) };
            return Err(FsError::last());
        }
        Ok(DirStream { dir })
    }

    /// Open a directory from a raw fd. The raw fd is duplicated first
    /// so we don't take ownership of the caller's fd.
    pub fn from_raw_fd(fd: RawFd) -> FsResult<Self> {
        // SAFETY: fd is a valid open file descriptor; dup returns a new fd.
        let dup_fd = unsafe { libc::dup(fd) };
        if dup_fd < 0 {
            return Err(FsError::last());
        }
        // SAFETY: dup_fd is a valid fd from dup(). fdopendir takes ownership.
        let dir = unsafe { libc::fdopendir(dup_fd) };
        if dir.is_null() {
            // SAFETY: dup_fd is still valid since fdopendir failed.
            unsafe { libc::close(dup_fd) };
            return Err(FsError::last());
        }
        Ok(DirStream { dir })
    }

    /// Read the next directory entry. Returns None at end of directory.
    pub fn next_entry(&mut self) -> Option<RawDirEntry> {
        // SAFETY: self.dir is a valid DIR* maintained by this struct's invariant.
        let entry = unsafe { libc::readdir(self.dir) };
        if entry.is_null() {
            return None;
        }
        // SAFETY: entry is non-null and valid for the lifetime of this readdir call.
        let entry = unsafe { &*entry };
        // SAFETY: d_name is a null-terminated C string in the dirent.
        let name = unsafe {
            std::ffi::CStr::from_ptr(entry.d_name.as_ptr())
                .to_bytes()
                .to_vec()
        };
        Some(RawDirEntry {
            name,
            ino: entry.d_ino as u64,
            dtype: entry.d_type,
        })
    }
}

impl Drop for DirStream {
    fn drop(&mut self) {
        // SAFETY: self.dir is a valid DIR* that we own; closedir releases it.
        unsafe {
            libc::closedir(self.dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dir_stream_lists_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::File::create(dir.path().join("file1")).unwrap();
        std::fs::File::create(dir.path().join("file2")).unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();

        let c_dir = std::ffi::CString::new(dir.path().to_str().unwrap()).unwrap();
        let fd = unsafe { libc::open(c_dir.as_ptr(), libc::O_DIRECTORY | libc::O_RDONLY) };
        assert!(fd >= 0);

        let mut stream = DirStream::from_raw_fd(fd).unwrap();
        let mut names = Vec::new();
        while let Some(entry) = stream.next_entry() {
            names.push(entry.name);
        }

        unsafe { libc::close(fd) };

        assert!(names.contains(&b"."[..].to_vec()));
        assert!(names.contains(&b".."[..].to_vec()));
        assert!(names.contains(&b"file1"[..].to_vec()));
        assert!(names.contains(&b"file2"[..].to_vec()));
        assert!(names.contains(&b"subdir"[..].to_vec()));
    }

    #[test]
    fn test_dir_stream_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let c_dir = std::ffi::CString::new(dir.path().to_str().unwrap()).unwrap();
        let fd = unsafe { libc::open(c_dir.as_ptr(), libc::O_DIRECTORY | libc::O_RDONLY) };
        assert!(fd >= 0);

        let mut stream = DirStream::from_raw_fd(fd).unwrap();
        let mut names = Vec::new();
        while let Some(entry) = stream.next_entry() {
            names.push(entry.name);
        }

        unsafe { libc::close(fd) };

        // Empty dir should only have . and ..
        assert_eq!(names.len(), 2);
        assert!(names.contains(&b"."[..].to_vec()));
        assert!(names.contains(&b".."[..].to_vec()));
    }
}
