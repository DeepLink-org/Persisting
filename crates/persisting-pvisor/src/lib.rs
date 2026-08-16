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

mod agent_abi;
mod artifact;
mod bundle;
mod checkpoint;
mod config;
mod container;
mod control;
mod delegated;
mod event;
mod executor;
mod firmware;
mod oci;
mod process;
mod pvisor;
#[doc(hidden)]
pub mod sandbox;
mod supervisor;
mod util;
mod vm;

#[cfg(feature = "fuzzing")]
pub use agent_abi::decode_agent_abi_frame_for_fuzz;
pub use agent_abi::{
    AgentAbiControl, AgentAbiServer, AgentAbiSnapshot, AgentCheckpointQuiesced, AgentClientRole,
    AgentClientSnapshot, AgentDirective, AgentEffectBegin, AgentEffectComplete, AgentEffectOutcome,
    AgentEffectSnapshot, AgentHeartbeatAck, AgentHello, AgentLifecycleState,
    AgentProcessRegistration, AgentProcessSnapshot, AgentRequest, AgentRequestBody, AgentResponse,
    AgentResponseBody, AgentWelcome, AGENT_ABI_ENDPOINT_ENV, AGENT_ABI_MAX_EFFECTS,
    AGENT_ABI_MAX_FRAME_BYTES, AGENT_ABI_MAX_PROCESSES, AGENT_ABI_MAX_SESSIONS,
    AGENT_ABI_TOKEN_ENV, AGENT_ABI_TRANSPORT_ENV, AGENT_ABI_VERSION, AGENT_ABI_VERSION_ENV,
};
pub use bundle::{
    BundleArtifact, BundleRun, FilesystemSummary, NetworkSummary, ResourceSummary, RunBundle,
    SafetySummary, RUN_BUNDLE_FILENAME, RUN_BUNDLE_SCHEMA_VERSION,
};
pub use checkpoint::{
    create_logical_checkpoint, latest_logical_checkpoint, restore_logical_checkpoint,
    CheckpointConsistency, LogicalCheckpoint, CHECKPOINTS_DIR,
};
pub use config::{
    ChronicleMode, ChronicleSettings, ContainerMount, ContainerNetwork, ContainerPlatform,
    ContainerSettings, GatewayDriverConfig, GatewayMode, GatewaySettings, NetworkDriverConfig,
    OverlayFsBackend, OverlayFsCommit, OverlayFsSettings, OverlayNetMode, OverlayNetPolicy,
    OverlayNetSettings, PVisorConfig, RunConfig, RunExecutorKind, RunPolicy, RunSettings, RunStdio,
    VmSettings,
};
pub use container::ContainerExecutor;
pub use control::{
    host_matches, is_public_egress_ip, normalize_host, parse_network_rule, ControlController,
    ControlEffect, ControlMachine, ControlReason, ControlRequest, ControlState, ControlTransition,
    NetworkGuard, NetworkHostRule, NetworkRule, PolicyControlController,
};
pub use event::{
    EventAppendErrorKind, EventSink, MemoryEventSink, NoopEventSink, RunEventPublisher,
};
pub use executor::{AttemptContext, RunExecutor};
#[cfg(feature = "fuzzing")]
pub use oci::fuzz_oci_layer;
pub use persisting_agentctl::{
    SupervisorClientMessage, SupervisorDirective, SupervisorDirectiveAck,
    SupervisorDirectiveEnvelope, SupervisorHeartbeat, SupervisorNetworkQuotaGrant,
    SupervisorRegistration, SupervisorServerMessage, SUPERVISOR_PROTOCOL_VERSION,
};
pub use persisting_gateway::sink::CaptureEventSink as TrajectoryEventSink;
pub use process::ProcessExecutor;
pub use pvisor::{PVisor, PVisorBuilder, PVisorError, RunCancellation, RunEventStream, RunHandle};
pub use runtime::{
    ChangeEntry, ChangeEntryType, ChangeKind, ImplantPlan, OverlayHint, RunLineage,
    RuntimeCapabilities,
};
#[cfg(feature = "fuzzing")]
pub use supervisor::decode_supervisor_frame_for_fuzz;
pub use util::unix_now_ms;
pub use vm::run_internal_if_requested as run_krun_internal_if_requested;
pub use vm::VmExecutor;
