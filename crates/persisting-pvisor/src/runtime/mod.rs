//! Attempt preparation for pVisor-owned runtime drivers.
//!
//! pVisor assembles the optional Gateway/OverlayNet driver, network policy, and
//! embedded OverlayFS before the Agent process starts.

mod attempt;
mod implant;
mod overlay;
mod registry;
mod supervisor;

pub(crate) use supervisor::RuntimeSupervisor;
pub(crate) use supervisor::RuntimeSupervisorBuilder;

pub use implant::{ImplantPlan, OverlayHint};
pub use overlay::{
    apply_overlay, discard_overlay, load_overlay_record, mount_overlay_record,
    mount_overlay_record_read_only, overlay_status, restore_overlay_upper, snapshot_overlay_upper,
    write_overlay_record, OverlayRecord, OverlayState, OverlayUpper, ReadOnlyOverlayMount,
};
pub use registry::{
    all_runs, control_mount_inspect, control_overlay_status, control_ping, control_unmount_inspect,
    default_run_home, is_live, resolve_run, RunLease, RunLineage, RunRecord,
};
pub use supervisor::RuntimeCapabilities;
