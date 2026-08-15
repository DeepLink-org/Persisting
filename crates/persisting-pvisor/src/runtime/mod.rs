//! Attempt preparation for pVisor-owned runtime drivers.
//!
//! pVisor assembles the optional Gateway/OverlayNet driver, network policy, and
//! embedded OverlayFS before the Agent process starts.

mod attempt;
mod implant;
mod overlay;
mod registry;
mod supervisor;

pub(crate) use attempt::AttemptTeardown;
pub(crate) use attempt::VmNetworkAttachment;
pub(crate) use supervisor::RuntimeSupervisor;
pub(crate) use supervisor::RuntimeSupervisorBuilder;

pub use implant::{ImplantPlan, OverlayHint};
#[cfg(all(test, target_os = "macos"))]
pub use overlay::apply_overlay;
pub use overlay::{
    apply_overlay_selected, discard_overlay, load_apply_records, load_overlay_record,
    mount_overlay_record, mount_overlay_record_read_only, overlay_changes, overlay_status,
    restore_overlay_upper, snapshot_overlay_upper, write_overlay_record, ApplySelection,
    ChangeEntry, ChangeEntryType, ChangeKind, OverlayRecord, OverlayState, OverlayUpper,
    ReadOnlyOverlayMount,
};
pub use registry::{
    all_runs, control_mount_inspect, control_overlay_status, control_ping, control_unmount_inspect,
    default_run_home, is_live, resolve_run, EnvironmentProjection, RunLease, RunLineage, RunRecord,
};
pub use supervisor::RuntimeCapabilities;
