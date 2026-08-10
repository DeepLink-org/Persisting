//! Portable overlay filesystem semantics shared by host FUSE and libkrun virtio-fs.

mod core;
pub mod sys;

pub use core::{OverlayCore, Resolved, OPAQUE_NAME, WHITEOUT_PREFIX};
