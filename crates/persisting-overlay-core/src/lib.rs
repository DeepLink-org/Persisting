//! Portable overlay filesystem semantics shared by host FUSE and libkrun virtio-fs.

mod core;
pub mod sys;

pub use core::{
    fingerprint_at, load_preimages, preimage_journal_is_complete, remove_preimages, OverlayCore,
    PathFingerprint, PathPreimage, Resolved, OPAQUE_NAME, WHITEOUT_PREFIX,
};
