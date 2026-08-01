//! pVisor — foreground Agent Run manager and portable execution runtime.
//!
//! pVisor is a top-level Persisting component alongside pPilot and pChronicle.
//! Hosts call [`PVisor::run`] directly; pVisor assembles execution, control,
//! network, filesystem, and the optional internal Gateway driver.
//!
//! pChronicle is a peer history service, not a child module. A trajectory sink
//! connects Gateway events to a pChronicle-backed adapter when requested.

pub mod cli;
mod runtime;

mod config;
mod control;
mod event;
mod executor;
mod process;
mod pvisor;
mod util;

pub use config::{
    ChronicleMode, ChronicleSettings, GatewayDriverConfig, GatewayMode, GatewaySettings,
    OverlayFsBackend, OverlayFsCommit, OverlayFsMode, OverlayFsSettings, OverlayNetMode,
    OverlayNetPolicy, OverlayNetSettings, PVisorConfig, RunConfig, RunPolicy, RunSettings,
    RunStdio,
};
pub use control::{
    host_matches, normalize_host, parse_network_rule, ControlController, ControlEffect,
    ControlMachine, ControlReason, ControlRequest, ControlState, ControlTransition, NetworkGuard,
    NetworkRule, PolicyControlController,
};
pub use event::{EventSink, MemoryEventSink, NoopEventSink, RunEventPublisher};
pub use executor::{AttemptContext, RunExecutor};
pub use persisting_gateway::sink::CaptureEventSink as TrajectoryEventSink;
pub use process::ProcessExecutor;
pub use pvisor::{PVisor, PVisorBuilder, PVisorError, RunCancellation, RunEventStream, RunHandle};
pub use runtime::{ImplantPlan, OverlayHint, RuntimeCapabilities};
pub use util::unix_now_ms;
