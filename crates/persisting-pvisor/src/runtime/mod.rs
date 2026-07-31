//! Attempt prepare: capture proxy, network policy, filesystem overlay.
//!
//! Driven from the shared capture TOML (`ProxyConfig`). Invoked by [`crate::PVisor`]
//! before the Agent process starts — not a separate control plane.

mod attempt;
mod implant;
mod overlay;
mod supervisor;

pub(crate) use supervisor::RuntimeSupervisor;
pub(crate) use supervisor::RuntimeSupervisorBuilder;

pub use attempt::{AttemptPrepareOpts, AttemptSession};
pub use implant::{ImplantPlan, OverlayHint};
pub use overlay::{
    apply_overlay, discard_overlay, load_overlay_by_id, load_overlay_record, mount_overlay,
    mount_overlay_record, overlay_hint_from_config, overlay_status, resolve_overlay_workspace,
    OverlayError, OverlayMount, OverlayRecord, OverlayState, OverlayStatus, OverlayUpper,
};
pub use supervisor::RuntimeCapabilities;
