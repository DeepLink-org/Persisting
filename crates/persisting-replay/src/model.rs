use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PLAN_SCHEMA_VERSION: &str = "sandbox-replay.plan/v1";
pub const REQUEST_SCHEMA_VERSION: &str = "sandbox-playback.request/v1";
pub const RESULT_SCHEMA_VERSION: &str = "sandbox-playback.result/v3";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentKind {
    ClaudeCode,
    MiniSweAgent,
    Openhands,
    SweAgent,
}

impl AgentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::MiniSweAgent => "mini-swe-agent",
            Self::Openhands => "openhands",
            Self::SweAgent => "swe-agent",
        }
    }

    pub fn supported_version(self) -> &'static str {
        match self {
            Self::ClaudeCode => "2.1.220",
            Self::MiniSweAgent => "2.4.6",
            Self::Openhands => "0.53.0",
            Self::SweAgent => "1.1.0",
        }
    }

    pub fn profile(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code/2.1.220/native-resume-v1",
            Self::MiniSweAgent => "mini-swe-agent/2.4.6/native-messages-v1",
            Self::Openhands => "openhands/0.53.0/native-replay-v1",
            Self::SweAgent => "swe-agent/1.1.0/replay-then-live-v1",
        }
    }
}

impl FromStr for AgentKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "claude-code" => Ok(Self::ClaudeCode),
            "mini-swe-agent" => Ok(Self::MiniSweAgent),
            "openhands" => Ok(Self::Openhands),
            "swe-agent" => Ok(Self::SweAgent),
            other => Err(format!(
                "unsupported agent {other:?}; expected claude-code, mini-swe-agent, openhands, or swe-agent"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayMode {
    PrepareOnly,
    ReplayOnly,
    ReplayAndContinue,
}

#[derive(Debug, Clone)]
pub struct PlaybackRequest {
    pub agent: AgentKind,
    pub trajectory: PathBuf,
    pub after_step: usize,
    pub workspace: PathBuf,
    pub state_dir: PathBuf,
    pub output_dir: PathBuf,
    pub agent_entrypoint: Option<PathBuf>,
    pub agent_runtime: Option<PathBuf>,
    pub disallowed_tools: Vec<String>,
    pub trajectory_assets: Option<PathBuf>,
    pub session_id: Option<String>,
    pub max_steps: Option<usize>,
    pub mode: ReplayMode,
    pub allow_stale_observations: bool,
    pub run_id: Option<String>,
    pub disable_thinking: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCall {
    pub ordinal: usize,
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
    #[serde(skip)]
    pub original_observation: Value,
    #[serde(skip)]
    pub original_is_error: bool,
    #[serde(skip)]
    pub native: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolBatch {
    pub ordinal: usize,
    pub native_locator: String,
    pub tool_calls: Vec<ToolCall>,
    #[serde(skip)]
    pub assistant_text: String,
    #[serde(skip)]
    pub native: Value,
}

#[derive(Debug, Clone)]
pub(crate) struct ReplayPlan {
    pub(crate) agent: AgentKind,
    pub(crate) source_path: PathBuf,
    pub(crate) source_sha256: String,
    pub(crate) after_step: usize,
    pub(crate) batches: Vec<ToolBatch>,
    pub(crate) prefix_model_turns: usize,
    pub(crate) native: Value,
    pub(crate) original_next_action: Option<Value>,
}

impl ReplayPlan {
    pub fn calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.batches
            .iter()
            .flat_map(|batch| batch.tool_calls.iter())
    }

    pub fn public_value(&self) -> Value {
        serde_json::json!({
            "schema_version": PLAN_SCHEMA_VERSION,
            "agent": {
                "name": self.agent.as_str(),
                "version": self.agent.supported_version(),
                "profile": self.agent.profile(),
            },
            "source": {
                "path": self.source_path,
                "sha256": self.source_sha256,
            },
            "boundary": {
                "after_step": self.after_step,
                "complete_tool_batch": true,
                "prefix_model_turns": self.prefix_model_turns,
                "tool_calls": self.calls().count(),
            },
            "batches": self.batches,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) enum AdapterPlan {
    ClaudeCode(ReplayPlan),
    MiniSweAgent(ReplayPlan),
    Openhands(ReplayPlan),
    SweAgent(ReplayPlan),
}

impl AdapterPlan {
    pub(crate) fn agent(&self) -> AgentKind {
        self.plan().agent
    }

    pub(crate) fn after_step(&self) -> usize {
        self.plan().after_step
    }

    pub(crate) fn prefix_model_turns(&self) -> usize {
        self.plan().prefix_model_turns
    }

    pub(crate) fn source_sha256(&self) -> &str {
        &self.plan().source_sha256
    }

    pub(crate) fn calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.plan().calls()
    }

    pub(crate) fn public_value(&self) -> Value {
        self.plan().public_value()
    }

    pub(crate) fn original_next_action(&self) -> Option<&Value> {
        self.plan().original_next_action.as_ref()
    }

    fn plan(&self) -> &ReplayPlan {
        match self {
            Self::ClaudeCode(plan)
            | Self::MiniSweAgent(plan)
            | Self::Openhands(plan)
            | Self::SweAgent(plan) => plan,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreshObservation {
    pub call_id: String,
    pub content: Value,
    pub is_error: bool,
    pub return_code: Option<i32>,
    pub duration_ms: u128,
    pub truncated: bool,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Debug)]
pub struct ReplayOutcome {
    pub status: String,
    pub reconstructed_path: Option<PathBuf>,
    pub continued_path: Option<PathBuf>,
    pub observations: Vec<FreshObservation>,
    pub continued_steps: usize,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct Artifact {
    pub role: String,
    pub format: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentResult {
    #[serde(rename = "type")]
    pub kind: String,
    pub version: String,
    pub entrypoint: Option<PathBuf>,
    pub launch_source: String,
    pub disallowed_tools: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayPhase {
    Prepared,
    Replayed,
    Continued,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayQuality {
    Verified,
    Degraded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Completed,
    MaxSteps,
    Failed,
    NotStarted,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplayFailure {
    pub category: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplayResult {
    pub schema_version: &'static str,
    pub phase: ReplayPhase,
    pub quality: ReplayQuality,
    pub agent_status: AgentStatus,
    pub run_id: String,
    pub agent: AgentResult,
    pub after_step: usize,
    pub replayed_tool_calls: usize,
    pub prefix_model_turns: usize,
    pub continued_steps: usize,
    pub state_dir: PathBuf,
    pub output_dir: PathBuf,
    pub artifacts: Vec<Artifact>,
    pub failure: Option<ReplayFailure>,
    pub retryable: bool,
    pub metadata: Value,
}

#[derive(Debug)]
pub struct ExecutionReport {
    pub result: ReplayResult,
    pub exit_code: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replay_plan(agent: AgentKind, marker: &str) -> ReplayPlan {
        ReplayPlan {
            agent,
            source_path: PathBuf::from(format!("/{marker}")),
            source_sha256: marker.into(),
            after_step: 1,
            batches: vec![ToolBatch {
                ordinal: 1,
                native_locator: marker.into(),
                tool_calls: Vec::new(),
                assistant_text: String::new(),
                native: serde_json::json!({"private": marker}),
            }],
            prefix_model_turns: 1,
            native: serde_json::json!({"private": marker}),
            original_next_action: None,
        }
    }

    #[test]
    fn adapter_plan_exposes_only_common_dispatch_fields() {
        let plans = [
            AdapterPlan::ClaudeCode(replay_plan(AgentKind::ClaudeCode, "claude")),
            AdapterPlan::MiniSweAgent(replay_plan(AgentKind::MiniSweAgent, "mini")),
            AdapterPlan::Openhands(replay_plan(AgentKind::Openhands, "openhands")),
            AdapterPlan::SweAgent(replay_plan(AgentKind::SweAgent, "swe")),
        ];

        for plan in plans {
            assert_eq!(plan.after_step(), 1);
            assert_eq!(plan.prefix_model_turns(), 1);
            assert_eq!(plan.calls().count(), 0);
            assert_eq!(plan.public_value()["agent"]["name"], plan.agent().as_str());
            assert!(!plan.source_sha256().is_empty());
        }
    }

    #[test]
    fn v3_result_serializes_typed_execution_state() {
        let result = ReplayResult {
            schema_version: RESULT_SCHEMA_VERSION,
            phase: ReplayPhase::Replayed,
            quality: ReplayQuality::Degraded,
            agent_status: AgentStatus::NotStarted,
            run_id: "replay-1".into(),
            agent: AgentResult {
                kind: "claude-code".into(),
                version: "2.1.220".into(),
                entrypoint: None,
                launch_source: "runtime_manifest".into(),
                disallowed_tools: Vec::new(),
            },
            after_step: 1,
            replayed_tool_calls: 1,
            prefix_model_turns: 1,
            continued_steps: 0,
            state_dir: PathBuf::from("/state/replay-1"),
            output_dir: PathBuf::from("/output/replay-1"),
            artifacts: Vec::new(),
            failure: None,
            retryable: false,
            metadata: Value::Null,
        };

        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["schema_version"], "sandbox-playback.result/v3");
        assert_eq!(value["phase"], "replayed");
        assert_eq!(value["quality"], "degraded");
        assert_eq!(value["agent_status"], "not_started");
        assert_eq!(value["failure"], Value::Null);
    }
}
