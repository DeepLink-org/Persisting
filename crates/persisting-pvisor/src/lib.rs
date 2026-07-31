//! pVisor — Portable Agent Execution Runtime.
//!
//! Library API for one Agent Run. Hosts (CLI, pPilot, …) call [`PVisor::run`]
//! directly; there is no separate control plane.
//!
//! Attempt prepare (capture proxy, network policy, embedded FUSE overlay) lives under
//! [`runtime`] and is driven from the shared capture TOML.

pub mod runtime;

mod access;
mod event;
mod executor;
mod process;
mod pvisor;
mod util;

pub use access::{
    host_matches, normalize_host, parse_network_rule, AccessController, NetworkGuard, NetworkRule,
    PolicyAccessController,
};
pub use event::{EventSink, MemoryEventSink, NoopEventSink, RunEventPublisher};
pub use executor::{AttemptContext, RunExecutor};
pub use process::ProcessExecutor;
pub use pvisor::{PVisor, PVisorBuilder, PVisorError, RunEventStream, RunHandle};
pub use runtime::{
    apply_overlay, discard_overlay, load_overlay_by_id, load_overlay_record, mount_overlay,
    mount_overlay_record, overlay_hint_from_config, overlay_status, resolve_overlay_workspace,
    AttemptPrepareOpts, AttemptSession, ImplantPlan, OverlayError, OverlayHint, OverlayMount,
    OverlayRecord, OverlayState, OverlayStatus, OverlayUpper, RuntimeCapabilities,
};
pub use util::unix_now_ms;
