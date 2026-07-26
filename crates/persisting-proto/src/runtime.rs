//! Stable value types shared by pVisor, Compute, capture, and storage.
//!
//! The runtime and narrative dimensions are deliberately orthogonal:
//!
//! - one [`RunId`] may have multiple execution [`AttemptId`]s;
//! - events may additionally belong to a [`StorylineId`], turn, and call.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;

pub const RUNTIME_SCHEMA_VERSION: u32 = 1;
pub const EVENT_SCHEMA_VERSION: u32 = 2;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn is_empty(&self) -> bool {
                self.0.trim().is_empty()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }
    };
}

string_id!(RunId);
string_id!(AttemptId);
string_id!(StorylineId);
string_id!(EventId);
string_id!(CheckpointId);

/// A versioned logical Agent reference. It describes identity, not placement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
}

impl AgentRef {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: None,
            adapter: None,
        }
    }
}

/// One semantic Agent execution submitted to pVisor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSpec {
    #[serde(default = "runtime_schema_version")]
    pub schema_version: u32,
    pub run_id: RunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<RunId>,
    pub agent: AgentRef,
    pub invocation: RunInvocation,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub capabilities: CapabilitySet,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

impl RunSpec {
    pub fn process(
        run_id: impl Into<RunId>,
        agent: impl Into<String>,
        program: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: RUNTIME_SCHEMA_VERSION,
            run_id: run_id.into(),
            task_id: None,
            parent_run_id: None,
            agent: AgentRef::new(agent),
            invocation: RunInvocation::Process(ProcessInvocation::new(program)),
            input: Value::Null,
            runtime: RuntimeConfig::default(),
            capabilities: CapabilitySet::default(),
            metadata: BTreeMap::new(),
        }
    }
}

fn runtime_schema_version() -> u32 {
    RUNTIME_SCHEMA_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunInvocation {
    Process(ProcessInvocation),
}

/// Local or container-hosted process invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInvocation {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub inherit_env: bool,
    #[serde(default)]
    pub stdin: StdioMode,
    #[serde(default)]
    pub stdout: StdioMode,
    #[serde(default)]
    pub stderr: StdioMode,
}

impl ProcessInvocation {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            inherit_env: true,
            stdin: StdioMode::Inherit,
            stdout: StdioMode::Inherit,
            stderr: StdioMode::Inherit,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StdioMode {
    #[default]
    Inherit,
    Capture,
    Null,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    /// Wall-clock limit for one Attempt. `None` means no pVisor deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Grace period between a cooperative process-tree termination request and
    /// forced termination.
    #[serde(default = "default_termination_grace_ms")]
    pub termination_grace_ms: u64,
    /// Maximum retained bytes for each captured stdout/stderr stream.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
    #[serde(default)]
    pub policy_mode: PolicyMode,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            timeout_ms: None,
            termination_grace_ms: default_termination_grace_ms(),
            max_output_bytes: default_max_output_bytes(),
            policy_mode: PolicyMode::Audit,
        }
    }
}

fn default_max_output_bytes() -> usize {
    1024 * 1024
}

fn default_termination_grace_ms() -> u64 {
    2_000
}

/// `Audit` records requested capabilities. `Enforce` requires an executor that
/// can actually prevent ambient access.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyMode {
    #[default]
    Audit,
    Enforce,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilitySet {
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub filesystem: Vec<FilesystemCapability>,
    #[serde(default)]
    pub network: NetworkCapability,
    #[serde(default)]
    pub secrets: Vec<String>,
    #[serde(default)]
    pub allow_subprocess: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemCapability {
    pub path: String,
    pub access: FilesystemAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemAccess {
    Read,
    ReadWrite,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum NetworkCapability {
    /// Use the executor's ambient network. Only valid for audit/compatibility runs.
    #[default]
    Ambient,
    Deny,
    AllowList {
        hosts: Vec<String>,
    },
}

/// Runtime-neutral network request presented to pVisor for authorization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkAccessRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<AttemptId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storyline_id: Option<StorylineId>,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub transport: NetworkTransport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkTransport {
    Http,
    Https,
    TcpTunnel,
}

/// Model invocation metadata. Request/response bodies intentionally stay in
/// Capture; pVisor receives only the information needed for policy and audit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCallRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<AttemptId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storyline_id: Option<StorylineId>,
    pub call_id: String,
    pub client_model: String,
    pub upstream_model: String,
    pub provider: String,
    pub protocol: String,
    pub upstream_host: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelAccessPolicy {
    #[serde(default)]
    pub allowed_models: Vec<String>,
    #[serde(default)]
    pub allowed_providers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessEffect {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessReason {
    AmbientNetwork,
    TrustedLocal,
    NetworkAllowList,
    NetworkDenied,
    NetworkAllowListEmpty,
    HostNotAllowed,
    ModelAllowed,
    ModelNotAllowed,
    ProviderNotAllowed,
}

impl AccessReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::AmbientNetwork => "ambient-network",
            Self::TrustedLocal => "trusted-local",
            Self::NetworkAllowList => "network-allowlist",
            Self::NetworkDenied => "network-denied",
            Self::NetworkAllowListEmpty => "network-allowlist-empty",
            Self::HostNotAllowed => "host-not-allowed",
            Self::ModelAllowed => "model-allowed",
            Self::ModelNotAllowed => "model-not-allowed",
            Self::ProviderNotAllowed => "provider-not-allowed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessDecision {
    pub effect: AccessEffect,
    pub reason: AccessReason,
}

impl AccessDecision {
    pub fn allow(reason: AccessReason) -> Self {
        Self {
            effect: AccessEffect::Allow,
            reason,
        }
    }

    pub fn deny(reason: AccessReason) -> Self {
        Self {
            effect: AccessEffect::Deny,
            reason,
        }
    }

    pub fn is_allowed(&self) -> bool {
        self.effect == AccessEffect::Allow
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Created,
    Starting,
    Running,
    Checkpointing,
    Suspended,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

impl RunState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorKind {
    Process,
    Container,
    Wasm,
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationKind {
    HostProcess,
    Container,
    Wasm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorDescriptor {
    pub name: String,
    pub kind: ExecutorKind,
    pub isolation: IsolationKind,
    pub enforces_capabilities: bool,
    pub supports_checkpoint: bool,
    pub supports_migration: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptInfo {
    pub attempt_id: AttemptId,
    pub number: u32,
    pub executor: ExecutorDescriptor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStatus {
    pub run_id: RunId,
    pub state: RunState,
    pub attempt: AttemptInfo,
    pub updated_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunFailureKind {
    InvalidSpec,
    Unsupported,
    Spawn,
    ProcessExit,
    DeadlineExceeded,
    Infrastructure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunFailure {
    pub kind: RunFailureKind,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProcessOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(default)]
    pub stdout_truncated: bool,
    #[serde(default)]
    pub stderr_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub name: String,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub state: RunState,
    pub started_at_unix_ms: u64,
    pub finished_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<RunFailure>,
    #[serde(default)]
    pub output: ProcessOutput,
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
    #[serde(default)]
    pub artifacts: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_stream_ref: Option<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointKind {
    Logical,
    ExecutorSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRef {
    pub checkpoint_id: CheckpointId,
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub kind: CheckpointKind,
    pub uri: String,
    pub event_seq: u64,
    pub created_at_unix_ms: u64,
}

/// Canonical event envelope. Provider-specific data belongs in `payload`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    #[serde(default = "event_schema_version")]
    pub schema_version: u32,
    pub event_id: EventId,
    pub run_id: RunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<AttemptId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storyline_id: Option<StorylineId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// Monotonic within `(run_id, attempt_id, storyline_id, producer)`.
    /// Runtime lifecycle events have no storyline and therefore use Attempt scope.
    pub seq: u64,
    pub timestamp_unix_ms: u64,
    pub kind: String,
    pub source: String,
    pub producer: String,
    #[serde(default)]
    pub payload: Value,
}

fn event_schema_version() -> u32 {
    EVENT_SCHEMA_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_run_spec_json_roundtrips() {
        let mut spec = RunSpec::process("run-1", "codex", "codex");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["exec".into(), "review".into()];
        process.stdout = StdioMode::Capture;
        spec.capabilities.network = NetworkCapability::AllowList {
            hosts: vec!["api.openai.com".into()],
        };

        let json = serde_json::to_string(&spec).unwrap();
        let decoded: RunSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.run_id.as_str(), "run-1");
        assert_eq!(decoded.schema_version, RUNTIME_SCHEMA_VERSION);
        assert!(matches!(
            decoded.capabilities.network,
            NetworkCapability::AllowList { .. }
        ));
    }

    #[test]
    fn attempt_is_orthogonal_to_storyline() {
        let event = EventEnvelope {
            schema_version: EVENT_SCHEMA_VERSION,
            event_id: EventId::from("event-1"),
            run_id: RunId::from("run-1"),
            attempt_id: Some(AttemptId::from("attempt-1")),
            storyline_id: Some(StorylineId::from("main")),
            turn_id: Some("turn-0".into()),
            call_id: Some("call-0".into()),
            seq: 0,
            timestamp_unix_ms: 1,
            kind: "llm.request".into(),
            source: "model".into(),
            producer: "pvisor".into(),
            payload: Value::Null,
        };
        assert_eq!(event.attempt_id.unwrap().as_str(), "attempt-1");
        assert_eq!(event.storyline_id.unwrap().as_str(), "main");
    }
}
