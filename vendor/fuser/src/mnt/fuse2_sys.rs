//! Native FFI bindings to libfuse2.
//!
//! This is a small set of bindings that are required to mount/unmount FUSE filesystems and
//! open/close a fd to the FUSE kernel driver.

#![warn(missing_debug_implementations)]
#![allow(missing_docs)]

use libc::{c_char, c_int};

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

#[cfg(fuser_mount_impl = "libfuse2")]
extern "C" {
    // *_compat25 functions were introduced in FUSE 2.6 when function signatures changed.
    // Therefore, the minimum version requirement for *_compat25 functions is libfuse-2.6.0.

    #[cfg(not(all(target_os = "macos", feature = "macfuse-5")))]
    pub fn fuse_mount_compat25(mountpoint: *const c_char, args: *const fuse_args) -> c_int;
    #[cfg(all(target_os = "macos", feature = "macfuse-5"))]
    pub fn fuse_mount(mountpoint: *const c_char, args: *const fuse_args) -> *mut fuse_chan;
    #[cfg(all(target_os = "macos", feature = "macfuse-5"))]
    pub fn fuse_chan_fd(channel: *mut fuse_chan) -> c_int;
    #[cfg(all(target_os = "macos", feature = "macfuse-5"))]
    pub fn fuse_unmount(mountpoint: *const c_char, channel: *mut fuse_chan);
    #[cfg(not(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "openbsd",
        target_os = "netbsd"
    )))]
    pub fn fuse_unmount_compat22(mountpoint: *const c_char);
}
