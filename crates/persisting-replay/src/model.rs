use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PLAN_SCHEMA_VERSION: &str = "sandbox-replay.plan/v1";
pub const REQUEST_SCHEMA_VERSION: &str = "sandbox-playback.request/v1";
pub const RESULT_SCHEMA_VERSION: &str = "sandbox-playback.result/v2";

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
pub struct ReplayPlan {
    pub agent: AgentKind,
    pub source_path: PathBuf,
    pub source_sha256: String,
    pub after_step: usize,
    pub batches: Vec<ToolBatch>,
    pub prefix_model_turns: usize,
    pub native: Value,
    pub original_next_action: Option<Value>,
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

#[derive(Debug, Clone, Serialize)]
pub struct ReplayResult {
    pub schema_version: &'static str,
    pub status: String,
    pub run_id: String,
    pub agent: AgentResult,
    pub after_step: usize,
    pub replayed_tool_calls: usize,
    pub prefix_model_turns: usize,
    pub continued_steps: usize,
    pub output_dir: PathBuf,
    pub artifacts: Vec<Artifact>,
    pub retryable: bool,
    pub metadata: Value,
}
