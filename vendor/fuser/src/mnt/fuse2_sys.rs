//! Native FFI bindings to libfuse2.
//!
//! This is a small set of bindings that are required to mount/unmount FUSE filesystems and
//! open/close a fd to the FUSE kernel driver.

#![warn(missing_debug_implementations)]
#![allow(missing_docs)]

use libc::{c_char, c_int};
#[cfg(all(target_os = "macos", feature = "macfuse-5"))]
use std::{
    io,
    sync::{Arc, OnceLock},
};

#[repr(C)]
#[derive(Debug)]
pub struct fuse_args {
    pub argc: c_int,
    pub argv: *const *const c_char,
    pub allocated: c_int,
}

#[cfg(all(target_os = "macos", feature = "macfuse-5"))]
#[repr(C)]
#[derive(Debug)]
pub struct fuse_chan {
    _private: [u8; 0],
}

#[cfg(all(target_os = "macos", feature = "macfuse-5"))]
pub struct MacFuseApi {
    _library: libloading::Library,
    pub mount: unsafe extern "C" fn(*const c_char, *const fuse_args) -> *mut fuse_chan,
    pub chan_fd: unsafe extern "C" fn(*mut fuse_chan) -> c_int,
    pub unmount: unsafe extern "C" fn(*const c_char, *mut fuse_chan),
}

#[cfg(all(target_os = "macos", feature = "macfuse-5"))]
impl std::fmt::Debug for MacFuseApi {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MacFuseApi")
            .finish_non_exhaustive()
    }
}

#[cfg(all(target_os = "macos", feature = "macfuse-5"))]
impl MacFuseApi {
    pub fn load() -> io::Result<Arc<Self>> {
        static API: OnceLock<Result<Arc<MacFuseApi>, String>> = OnceLock::new();
        match API.get_or_init(|| Self::load_uncached().map_err(|error| error.to_string())) {
            Ok(api) => Ok(Arc::clone(api)),
            Err(error) => Err(io::Error::new(io::ErrorKind::NotFound, error.clone())),
        }
    }

    fn load_uncached() -> io::Result<Arc<Self>> {
        const CANDIDATES: [&str; 4] = [
            "/usr/local/lib/libfuse.2.dylib",
            "/usr/local/lib/libfuse.dylib",
            "libfuse.2.dylib",
            "libfuse.dylib",
        ];
        let mut failures = Vec::new();
        for candidate in CANDIDATES {
            let library = match unsafe { libloading::Library::new(candidate) } {
                Ok(library) => library,
                Err(error) => {
                    failures.push(format!("{candidate}: {error}"));
                    continue;
                }
            };
            let symbols = unsafe {
                let mount = *library
                    .get::<unsafe extern "C" fn(
                        *const c_char,
                        *const fuse_args,
                    ) -> *mut fuse_chan>(b"fuse_mount\0")
                    .map_err(io::Error::other)?;
                let chan_fd = *library
                    .get::<unsafe extern "C" fn(*mut fuse_chan) -> c_int>(b"fuse_chan_fd\0")
                    .map_err(io::Error::other)?;
                let unmount = *library
                    .get::<unsafe extern "C" fn(*const c_char, *mut fuse_chan)>(b"fuse_unmount\0")
                    .map_err(io::Error::other)?;
                (mount, chan_fd, unmount)
            };
            return Ok(Arc::new(Self {
                _library: library,
                mount: symbols.0,
                chan_fd: symbols.1,
                unmount: symbols.2,
            }));
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "macFUSE runtime library not found; install macFUSE before mounting ({})",
                failures.join("; ")
            ),
        ))
    }
}

#[cfg(fuser_mount_impl = "libfuse2")]
extern "C" {
    // *_compat25 functions were introduced in FUSE 2.6 when function signatures changed.
    // Therefore, the minimum version requirement for *_compat25 functions is libfuse-2.6.0.

    #[cfg(not(all(target_os = "macos", feature = "macfuse-5")))]
    pub fn fuse_mount_compat25(mountpoint: *const c_char, args: *const fuse_args) -> c_int;
    #[cfg(not(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "openbsd",
        target_os = "netbsd"
    )))]
    pub fn fuse_unmount_compat22(mountpoint: *const c_char);
}
