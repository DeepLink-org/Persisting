use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::Deserialize;

use crate::error::{ReplayError, ReplayErrorKind, ResultExt};
use crate::model::{AgentKind, PlaybackRequest, REQUEST_SCHEMA_VERSION, ReplayMode};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayToml {
    pub replay: ReplayConfig,
    #[serde(default)]
    pub run: RunConfig,
    #[serde(default)]
    pub overlayfs: OverlayFsConfig,
    #[serde(default)]
    pub overlaynet: OverlayNetConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayConfig {
    pub agent: String,
    pub trajectory: PathBuf,
    pub after_step: usize,
    pub agent_entrypoint: Option<PathBuf>,
    pub agent_runtime: Option<PathBuf>,
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
    pub trajectory_assets: Option<PathBuf>,
    pub max_steps: Option<usize>,
    pub session_id: Option<String>,
    #[serde(default)]
    pub replay_only: bool,
    #[serde(default)]
    pub prepare_only: bool,
    #[serde(default)]
    pub allow_stale_observations: bool,
    #[serde(default)]
    pub disable_thinking: bool,
    pub boundary_user_prompt: Option<String>,
    pub run_id: Option<String>,
    pub workspace: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub output_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    #[serde(default)]
    pub safe: bool,
    pub executor: Option<String>,
    pub timeout_ms: Option<u64>,
    pub policy: Option<String>,
    #[serde(default)]
    pub inherit_env: bool,
    #[serde(default)]
    pub pass_env: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayFsConfig {
    pub base: Option<PathBuf>,
    pub backend: Option<String>,
    pub commit: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayNetConfig {
    pub mode: Option<String>,
    pub policy: Option<String>,
}

impl ReplayToml {
    pub fn from_file(path: &Path) -> Result<Self, ReplayError> {
        let source = fs::read_to_string(path).replay_context(
            ReplayErrorKind::Configuration,
            format!("read replay config {}", path.display()),
        )?;
        toml::from_str(&source).replay_context(
            ReplayErrorKind::Configuration,
            format!("parse replay config {}", path.display()),
        )
    }

    pub fn into_request(self, cwd: &Path) -> Result<PlaybackRequest, ReplayError> {
        let replay = self.replay;
        let mode = replay_mode(replay.prepare_only, replay.replay_only)?;
        Ok(PlaybackRequest {
            agent: AgentKind::from_str(&replay.agent)
                .map_err(|message| ReplayError::new(ReplayErrorKind::UnsupportedAgent, message))?,
            trajectory: replay.trajectory,
            after_step: replay.after_step,
            workspace: replay.workspace.unwrap_or_else(|| cwd.to_path_buf()),
            state_dir: replay
                .state_dir
                .unwrap_or_else(|| PathBuf::from("/tmp/pvisor-sandbox-replay/state")),
            output_dir: replay
                .output_dir
                .unwrap_or_else(|| PathBuf::from("/tmp/pvisor-sandbox-replay/output")),
            agent_entrypoint: replay.agent_entrypoint,
            agent_runtime: replay.agent_runtime,
            disallowed_tools: replay.disallowed_tools,
            trajectory_assets: replay.trajectory_assets,
            session_id: replay.session_id,
            max_steps: replay.max_steps,
            mode,
            allow_stale_observations: replay.allow_stale_observations,
            run_id: replay.run_id,
            disable_thinking: replay.disable_thinking,
            boundary_user_prompt: replay.boundary_user_prompt,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest {
    schema_version: String,
    agent: JsonAgent,
    trajectory: PathBuf,
    trajectory_assets: Option<PathBuf>,
    after_step: usize,
    workspace: PathBuf,
    state_dir: PathBuf,
    output_dir: PathBuf,
    max_steps: Option<usize>,
    session_id: Option<String>,
    #[serde(default)]
    replay_only: bool,
    #[serde(default)]
    prepare_only: bool,
    #[serde(default)]
    allow_stale_observations: bool,
    #[serde(default)]
    disable_thinking: bool,
    boundary_user_prompt: Option<String>,
    run_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonAgent {
    #[serde(rename = "type")]
    kind: String,
    entrypoint: Option<PathBuf>,
    runtime: Option<PathBuf>,
    #[serde(default)]
    disallowed_tools: Vec<String>,
}

pub fn request_from_json(path: &Path) -> Result<PlaybackRequest, ReplayError> {
    let bytes = fs::read(path).replay_context(
        ReplayErrorKind::Configuration,
        format!("read request {}", path.display()),
    )?;
    let request: JsonRequest = serde_json::from_slice(&bytes).replay_context(
        ReplayErrorKind::Configuration,
        "parse sandbox-playback request",
    )?;
    if request.schema_version != REQUEST_SCHEMA_VERSION {
        return Err(ReplayError::configuration(format!(
            "request schema_version must be {REQUEST_SCHEMA_VERSION:?}"
        )));
    }
    let mode = replay_mode(request.prepare_only, request.replay_only)?;
    Ok(PlaybackRequest {
        agent: AgentKind::from_str(&request.agent.kind)
            .map_err(|message| ReplayError::new(ReplayErrorKind::UnsupportedAgent, message))?,
        trajectory: request.trajectory,
        after_step: request.after_step,
        workspace: request.workspace,
        state_dir: request.state_dir,
        output_dir: request.output_dir,
        agent_entrypoint: request.agent.entrypoint,
        agent_runtime: request.agent.runtime,
        disallowed_tools: request.agent.disallowed_tools,
        trajectory_assets: request.trajectory_assets,
        session_id: request.session_id,
        max_steps: request.max_steps,
        mode,
        allow_stale_observations: request.allow_stale_observations,
        run_id: request.run_id,
        disable_thinking: request.disable_thinking,
        boundary_user_prompt: request.boundary_user_prompt,
    })
}

fn replay_mode(prepare_only: bool, replay_only: bool) -> Result<ReplayMode, ReplayError> {
    match (prepare_only, replay_only) {
        (true, true) => Err(ReplayError::configuration(
            "prepare_only and replay_only are mutually exclusive",
        )),
        (true, false) => Ok(ReplayMode::PrepareOnly),
        (false, true) => Ok(ReplayMode::ReplayOnly),
        (false, false) => Ok(ReplayMode::ReplayAndContinue),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ReplayMode;

    #[test]
    fn toml_maps_prepare_and_replay_modes_and_rejects_both() {
        let prepare: ReplayToml = toml::from_str(
            r#"
[replay]
agent = "claude-code"
trajectory = "/input/session.jsonl"
after_step = 1
prepare_only = true
allow_stale_observations = true
"#,
        )
        .unwrap();
        let prepare = prepare.into_request(Path::new("/workspace")).unwrap();
        assert_eq!(prepare.mode, ReplayMode::PrepareOnly);
        assert!(prepare.allow_stale_observations);

        let replay: ReplayToml = toml::from_str(
            r#"
[replay]
agent = "claude-code"
trajectory = "/input/session.jsonl"
after_step = 1
replay_only = true
"#,
        )
        .unwrap();
        assert_eq!(
            replay.into_request(Path::new("/workspace")).unwrap().mode,
            ReplayMode::ReplayOnly
        );

        let both: ReplayToml = toml::from_str(
            r#"
[replay]
agent = "claude-code"
trajectory = "/input/session.jsonl"
after_step = 1
prepare_only = true
replay_only = true
"#,
        )
        .unwrap();
        assert!(both.into_request(Path::new("/workspace")).is_err());
    }

    #[test]
    fn minimal_toml_defaults_to_prepared_sandbox() {
        let config: ReplayToml = toml::from_str(
            r#"
[replay]
agent = "claude-code"
trajectory = "/input/session.jsonl"
after_step = 30
agent_entrypoint = "/usr/bin/claude"
"#,
        )
        .unwrap();
        let request = config.into_request(Path::new("/workspace")).unwrap();
        assert_eq!(request.workspace, Path::new("/workspace"));
        assert_eq!(
            request.state_dir,
            Path::new("/tmp/pvisor-sandbox-replay/state")
        );
        assert_eq!(
            request.output_dir,
            Path::new("/tmp/pvisor-sandbox-replay/output")
        );
        assert!(!request.disable_thinking);
    }

    #[test]
    fn toml_can_disable_thinking_for_live_continuation() {
        let config: ReplayToml = toml::from_str(
            r#"
[replay]
agent = "claude-code"
trajectory = "/input/session.jsonl"
after_step = 30
agent_entrypoint = "/usr/bin/claude"
disable_thinking = true
"#,
        )
        .unwrap();

        let request = config.into_request(Path::new("/workspace")).unwrap();

        assert!(request.disable_thinking);
    }

    #[test]
    fn toml_accepts_a_boundary_user_prompt() {
        let config: ReplayToml = toml::from_str(
            r#"
[replay]
agent = "claude-code"
trajectory = "/input/session.jsonl"
after_step = 30
boundary_user_prompt = "Review the fresh observation before continuing."
"#,
        )
        .unwrap();

        let request = config.into_request(Path::new("/workspace")).unwrap();

        assert_eq!(
            request.boundary_user_prompt(),
            Some("Review the fresh observation before continuing.")
        );
    }

    #[test]
    fn toml_rejects_removed_gateway_section() {
        let config = toml::from_str::<ReplayToml>(
            r#"
[replay]
agent = "claude-code"
trajectory = "/input/session.jsonl"
after_step = 1
agent_entrypoint = "/usr/bin/claude"
[gateway]
mode = "off"
"#,
        );
        assert!(config.is_err());
    }

    #[test]
    fn toml_rejects_removed_chronicle_section() {
        let config = toml::from_str::<ReplayToml>(
            r#"
[replay]
agent = "claude-code"
trajectory = "/input/session.jsonl"
after_step = 1
agent_entrypoint = "/usr/bin/claude"
[chronicle]
mode = "off"
"#,
        );
        assert!(config.is_err());
    }

    #[test]
    fn json_request_accepts_the_sweeval_contract() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("request.json");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": "sandbox-playback.request/v1",
                "agent": {
                    "type": "mini-swe-agent",
                    "entrypoint": "/root/.local/bin/mini-swe-agent"
                },
                "trajectory": "/tmp/sweeval-sandbox-replay/input/trajectory.json",
                "after_step": 49,
                "workspace": "/app",
                "state_dir": "/tmp/sweeval-sandbox-replay/state",
                "output_dir": "/tmp/sweeval-sandbox-replay/output",
                "max_steps": 200,
                "session_id": "task-291-attempt-1",
                "run_id": "sweeval"
            }))
            .unwrap(),
        )
        .unwrap();

        let request = request_from_json(&path).unwrap();

        assert_eq!(request.agent, AgentKind::MiniSweAgent);
        assert_eq!(request.after_step, 49);
        assert_eq!(request.workspace, Path::new("/app"));
        assert_eq!(request.max_steps, Some(200));
        assert_eq!(request.session_id.as_deref(), Some("task-291-attempt-1"));
        assert_eq!(request.run_id.as_deref(), Some("sweeval"));
        assert_eq!(request.mode, ReplayMode::ReplayAndContinue);
        assert!(!request.disable_thinking);
    }

    #[test]
    fn json_maps_prepare_mode_and_rejects_conflicting_modes() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("request.json");
        let mut value = serde_json::json!({
            "schema_version": "sandbox-playback.request/v1",
            "agent": { "type": "claude-code" },
            "trajectory": "/input/session.jsonl",
            "after_step": 1,
            "workspace": "/workspace",
            "state_dir": "/state",
            "output_dir": "/output",
            "prepare_only": true,
            "allow_stale_observations": true
        });
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

        let request = request_from_json(&path).unwrap();
        assert_eq!(request.mode, ReplayMode::PrepareOnly);
        assert!(request.allow_stale_observations);

        value["replay_only"] = serde_json::Value::Bool(true);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(request_from_json(&path).is_err());
    }

    #[test]
    fn json_request_can_disable_thinking() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("request.json");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": "sandbox-playback.request/v1",
                "agent": {
                    "type": "mini-swe-agent",
                    "entrypoint": "/root/.local/bin/mini-swe-agent"
                },
                "trajectory": "/tmp/sweeval-sandbox-replay/input/trajectory.json",
                "after_step": 49,
                "workspace": "/app",
                "state_dir": "/tmp/sweeval-sandbox-replay/state",
                "output_dir": "/tmp/sweeval-sandbox-replay/output",
                "disable_thinking": true
            }))
            .unwrap(),
        )
        .unwrap();

        let request = request_from_json(&path).unwrap();

        assert!(request.disable_thinking);
    }

    #[test]
    fn json_request_accepts_a_boundary_user_prompt() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("request.json");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": "sandbox-playback.request/v1",
                "agent": {"type": "mini-swe-agent"},
                "trajectory": "/input/trajectory.json",
                "after_step": 1,
                "workspace": "/workspace",
                "state_dir": "/state",
                "output_dir": "/output",
                "boundary_user_prompt": "Inspect O-prime N."
            }))
            .unwrap(),
        )
        .unwrap();

        let request = request_from_json(&path).unwrap();

        assert_eq!(request.boundary_user_prompt(), Some("Inspect O-prime N."));
    }
}
