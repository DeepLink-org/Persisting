//! pVisor — Portable Agent Execution Runtime.
//!
//! pVisor owns the execution of one [`persisting_proto::RunSpec`]. Batch
//! expansion, fleet scheduling, and result collection remain pPilot concerns.

mod access;
mod event;
mod executor;
mod process;
mod runtime;

pub use access::{
    host_matches, normalize_host, parse_network_rule, AccessController, NetworkGuard, NetworkRule,
    PolicyAccessController,
};
pub use event::{EventSink, MemoryEventSink, NoopEventSink, RunEventPublisher};
pub use executor::{AttemptContext, RunExecutor};
pub use process::ProcessExecutor;
pub use runtime::{PVisor, PVisorError, RunEventStream, RunHandle};
