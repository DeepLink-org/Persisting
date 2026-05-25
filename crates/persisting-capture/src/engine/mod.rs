//! Event-driven capture on pulsing-actor.
//!
//! ## Layers
//!
//! ```text
//! types      Domain — CaptureInvocation + CaptureEvent (proxy boundary)
//! prepare    Compute records + backfills (runtime thread, may ask registry)
//! wire       Serializable actor commands (bincode + JSON payloads)
//! actors     Mailbox handlers (registry + per-session I/O)
//! runtime    Orchestrator — prepare → dispatch
//! io         Session-local markdown helpers (actors only)
//! ```
//!
//! ## Actor topology
//!
//! ```text
//! CaptureRuntime
//!   ├── capture/subagent-registry   RegistryCommand → RegistryReply
//!   └── capture/session/{seq_key}   SessionCommand  → CaptureAck
//! ```

mod actors;
mod io;
mod prepare;
mod runtime;
mod types;
mod wire;

pub use runtime::CaptureEngine;
pub use types::{
    CaptureEvent, CaptureInvocation, LlmCallCancelled, LlmRequestCaptured, LlmResponseCompleted,
    LlmResponseDraftUpdated,
};

#[cfg(test)]
mod tests;
