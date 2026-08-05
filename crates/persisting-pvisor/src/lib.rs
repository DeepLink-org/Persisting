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
mod kvm;
mod process;
mod pvisor;
mod supervisor;
mod util;

pub use agent_abi::{
    AgentAbiControl, AgentAbiServer, AgentAbiSnapshot, AgentCheckpointQuiesced, AgentClientRole,
    AgentClientSnapshot, AgentDirective, AgentEffectBegin, AgentEffectComplete, AgentEffectOutcome,
    AgentEffectSnapshot, AgentHeartbeatAck, AgentHello, AgentLifecycleState,
    AgentProcessRegistration, AgentProcessSnapshot, AgentRequest, AgentRequestBody, AgentResponse,
    AgentResponseBody, AgentWelcome, AGENT_ABI_ENDPOINT_ENV, AGENT_ABI_MAX_FRAME_BYTES,
    AGENT_ABI_TOKEN_ENV, AGENT_ABI_TRANSPORT_ENV, AGENT_ABI_VERSION, AGENT_ABI_VERSION_ENV,
};
pub use bundle::{
    BundleArtifact, BundleRun, FilesystemSummary, NetworkSummary, RunBundle, SafetySummary,
    RUN_BUNDLE_FILENAME, RUN_BUNDLE_SCHEMA_VERSION,
};
pub use checkpoint::{
    create_logical_checkpoint, latest_logical_checkpoint, restore_logical_checkpoint,
    CheckpointConsistency, LogicalCheckpoint, CHECKPOINTS_DIR,
};
pub use config::{
    ChronicleMode, ChronicleSettings, ContainerMount, ContainerNetwork, ContainerPlatform,
    ContainerSettings, GatewayDriverConfig, GatewayMode, GatewaySettings, KvmArchitecture,
    KvmImageFormat, KvmSettings, OverlayFsBackend, OverlayFsCommit, OverlayFsMode,
    OverlayFsSettings, OverlayNetMode, OverlayNetPolicy, OverlayNetSettings, PVisorConfig,
    RunConfig, RunExecutorKind, RunPolicy, RunSettings, RunStdio,
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
pub use kvm::KvmExecutor;
pub use persisting_gateway::sink::CaptureEventSink as TrajectoryEventSink;
pub use process::ProcessExecutor;
pub use pvisor::{PVisor, PVisorBuilder, PVisorError, RunCancellation, RunEventStream, RunHandle};
pub use runtime::{ImplantPlan, OverlayHint, RunLineage, RuntimeCapabilities};
pub use supervisor::{
    SupervisorClientMessage, SupervisorDirective, SupervisorDirectiveAck,
    SupervisorDirectiveEnvelope, SupervisorHeartbeat, SupervisorNetworkQuotaGrant,
    SupervisorRegistration, SupervisorServerMessage, SUPERVISOR_PROTOCOL_VERSION,
};
pub use util::unix_now_ms;
