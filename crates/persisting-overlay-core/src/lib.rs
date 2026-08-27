//! Portable overlay filesystem semantics shared by host FUSE and libkrun virtio-fs.

mod core;
pub mod sys;

pub use core::{
    OPAQUE_NAME, OverlayCore, PathFingerprint, PathPreimage, Resolved, WHITEOUT_PREFIX,
    fingerprint_at, load_preimages, preimage_journal_is_complete, remove_preimages,
};
