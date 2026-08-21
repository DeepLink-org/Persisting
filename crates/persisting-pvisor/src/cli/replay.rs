use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::str::FromStr;

use clap::Args;
use persisting_replay::{
    execute, request_from_json, AgentKind, OverlayFsConfig as ReplayOverlayFsConfig,
    OverlayNetConfig as ReplayOverlayNetConfig, PlaybackRequest, ReplayConfig, ReplayError,
    ReplayMode, ReplayToml, RunConfig as ReplayRunConfig, RESULT_SCHEMA_VERSION,
};
use serde_json::json;

use crate::config::{
    OverlayFsBackend as PVisorOverlayFsBackend, OverlayFsCommit as PVisorOverlayFsCommit,
    OverlayFsSettings, OverlayNetMode as PVisorOverlayNetMode,
    OverlayNetPolicy as PVisorOverlayNetPolicy, RunConfig as PVisorRunConfig, RunExecutorKind,
    RunPolicy,
};

#[derive(Debug, Clone, Args)]
pub struct ReplayArgs {
    /// Complete replay TOML containing [replay] and optional runtime sections.
    #[arg(long, value_name = "FILE", conflicts_with = "request")]
    config: Option<PathBuf>,

    /// Versioned sandbox-playback JSON request.
    #[arg(long, value_name = "FILE", conflicts_with = "config")]
    request: Option<PathBuf>,

    /// Agent-native trajectory format.
    #[arg(long)]
    agent: Option<String>,

    /// Native trajectory file to replay.
    #[arg(long, value_name = "FILE")]
    trajectory: Option<PathBuf>,

    /// Complete tool batch ordinal after which live execution resumes.
    #[arg(long)]
    after_step: Option<usize>,

    /// Exact version-pinned Agent executable.
    #[arg(long, value_name = "PATH")]
    agent_entrypoint: Option<PathBuf>,

    /// Versioned Agent runtime directory with sandbox-playback-agent.json.
    #[arg(long, value_name = "DIR")]
    agent_runtime: Option<PathBuf>,

    /// Disable a Claude Code tool during the live continuation; repeat as needed.
    #[arg(long = "agent-disallowed-tool", value_name = "TOOL")]
    disallowed_tools: Vec<String>,

    /// Read-only trajectory assets root, primarily for SWE-agent problem files.
    #[arg(long, value_name = "DIR")]
    trajectory_assets: Option<PathBuf>,

    /// Existing fresh-sandbox workspace; defaults to the current directory.
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,

    /// Temporary replay state root; defaults to /tmp/pvisor-sandbox-replay/state.
    #[arg(long, value_name = "DIR")]
    state_dir: Option<PathBuf>,

    /// Temporary replay output root; defaults to /tmp/pvisor-sandbox-replay/output.
    #[arg(long, value_name = "DIR")]
    output_dir: Option<PathBuf>,

    #[arg(long)]
    session_id: Option<String>,

    /// Total Agent action budget including the replayed prefix and any live continuation.
    #[arg(long)]
    max_steps: Option<usize>,

    /// Parse and construct the selected prefix without executing tools or starting an Agent.
    #[arg(long, conflicts_with = "replay_only")]
    prepare_only: bool,

    /// Execute the selected tool prefix and stop before the next model request.
    #[arg(long, conflicts_with = "prepare_only")]
    replay_only: bool,

    /// Permit replay to reuse source observations that cannot be freshly reproduced.
    #[arg(long)]
    allow_stale_observations: bool,

    /// Force live continuation model requests to disable thinking.
    #[arg(long)]
    disable_thinking: bool,

    #[arg(long)]
    run_id: Option<String>,

    /// Use an outer pVisor Run with the platform safe profile.
    #[arg(long)]
    safe: bool,

    /// Optional outer execution provider: host, container, or vm.
    #[arg(long, value_name = "KIND")]
    executor: Option<String>,

    /// Timeout for the outer managed pVisor Run.
    #[arg(long, value_name = "MILLISECONDS")]
    timeout_ms: Option<u64>,

    /// Outer pVisor capability policy: observe or enforce.
    #[arg(long, value_name = "MODE")]
    policy: Option<String>,

    /// Let the outer managed Run inherit the complete host environment.
    #[arg(long)]
    inherit_env: bool,

    /// Project one host environment variable into the outer Run; repeat as needed.
    #[arg(long, value_name = "NAME")]
    pass_env: Vec<String>,

    /// Base workspace for optional outer pVisor filesystem isolation.
    #[arg(long, value_name = "DIR")]
    overlayfs_base: Option<PathBuf>,

    /// Outer OverlayFS backend: directory or jujutsu.
    #[arg(long, value_name = "BACKEND")]
    overlayfs_backend: Option<String>,

    /// Outer OverlayFS commit behavior: manual, apply, or drop.
    #[arg(long, value_name = "MODE")]
    overlayfs_commit: Option<String>,

    /// Outer OverlayNet mode: auto, off, or proxy.
    #[arg(long, value_name = "MODE")]
    overlaynet_mode: Option<String>,

    /// Outer OverlayNet policy: public, deny, or allowlist.
    #[arg(long, value_name = "POLICY")]
    overlaynet_policy: Option<String>,
}

pub fn run(args: ReplayArgs) -> i32 {
    if let Some(config_path) = args.config.as_ref() {
        match managed_if_requested(&args, config_path) {
            Ok(Some(code)) => return code,
            Ok(None) => {}
            Err(error) => {
                print_error(&error);
                return error.exit_code();
            }
        }
    }
    if args.request.is_none() && direct_managed_requested(&args) {
        match direct_managed_config(&args).and_then(|config| run_managed(&config)) {
            Ok(code) => return code,
            Err(error) => {
                print_error(&error);
                return error.exit_code();
            }
        }
    }
    match normalize(args).and_then(execute) {
        Ok(report) => {
            println!(
                "{}",
                serde_json::to_string(&report.result).expect("ReplayResult is serializable")
            );
            report.exit_code
        }
        Err(error) => {
            print_error(&error);
            error.exit_code()
        }
    }
}

fn managed_if_requested(
    args: &ReplayArgs,
    config_path: &std::path::Path,
) -> Result<Option<i32>, ReplayError> {
    reject_direct(args)?;
    let config = ReplayToml::from_file(config_path)?;
    if !needs_managed_run(&config) {
        return Ok(None);
    }
    let cwd = std::env::current_dir().map_err(|error| {
        ReplayError::configuration(format!("cannot read current directory: {error}"))
    })?;
    // Validate the inner replay contract before creating an outer Run.
    let _ = config.clone().into_request(&cwd)?;
    run_managed(&config).map(Some)
}

fn needs_managed_run(config: &ReplayToml) -> bool {
    config.run.safe
        || config.run.executor.is_some()
        || config.run.timeout_ms.is_some()
        || config.run.policy.is_some()
        || config.run.inherit_env
        || !config.run.pass_env.is_empty()
        || config.overlayfs.base.is_some()
        || config.overlayfs.backend.is_some()
        || config.overlayfs.commit.is_some()
        || config.overlaynet.mode.is_some()
        || config.overlaynet.policy.is_some()
}

fn direct_managed_requested(args: &ReplayArgs) -> bool {
    args.safe
        || args.executor.is_some()
        || args.timeout_ms.is_some()
        || args.policy.is_some()
        || args.inherit_env
        || !args.pass_env.is_empty()
        || args.overlayfs_base.is_some()
        || args.overlayfs_backend.is_some()
        || args.overlayfs_commit.is_some()
        || args.overlaynet_mode.is_some()
        || args.overlaynet_policy.is_some()
}

fn direct_managed_config(args: &ReplayArgs) -> Result<ReplayToml, ReplayError> {
    let agent = args
        .agent
        .clone()
        .ok_or_else(|| ReplayError::configuration("direct CLI mode requires --agent"))?;
    let trajectory = args
        .trajectory
        .clone()
        .ok_or_else(|| ReplayError::configuration("direct CLI mode requires --trajectory"))?;
    let after_step = args
        .after_step
        .ok_or_else(|| ReplayError::configuration("direct CLI mode requires --after-step"))?;
    Ok(ReplayToml {
        replay: ReplayConfig {
            agent,
            trajectory,
            after_step,
            agent_entrypoint: args.agent_entrypoint.clone(),
            agent_runtime: args.agent_runtime.clone(),
            disallowed_tools: args.disallowed_tools.clone(),
            trajectory_assets: args.trajectory_assets.clone(),
            max_steps: args.max_steps,
            session_id: args.session_id.clone(),
            replay_only: args.replay_only,
            prepare_only: args.prepare_only,
            allow_stale_observations: args.allow_stale_observations,
            disable_thinking: args.disable_thinking,
            run_id: args.run_id.clone(),
            workspace: args.workspace.clone(),
            state_dir: args.state_dir.clone(),
            output_dir: args.output_dir.clone(),
        },
        run: ReplayRunConfig {
            safe: args.safe,
            executor: args.executor.clone(),
            timeout_ms: args.timeout_ms,
            policy: args.policy.clone(),
            inherit_env: args.inherit_env,
            pass_env: args.pass_env.clone(),
        },
        overlayfs: ReplayOverlayFsConfig {
            base: args.overlayfs_base.clone(),
            backend: args.overlayfs_backend.clone(),
            commit: args.overlayfs_commit.clone(),
        },
        overlaynet: ReplayOverlayNetConfig {
            mode: args.overlaynet_mode.clone(),
            policy: args.overlaynet_policy.clone(),
        },
    })
}

fn run_managed(config: &ReplayToml) -> Result<i32, ReplayError> {
    let executable = std::env::current_exe().map_err(|error| {
        ReplayError::configuration(format!("cannot resolve the pVisor executable: {error}"))
    })?;
    let mut outer = PVisorRunConfig::default();
    outer.run.agent = format!("replay-{}", config.replay.agent);
    outer.run.executor = match config.run.executor.as_deref().unwrap_or("host") {
        "host" => RunExecutorKind::Host,
        "container" => RunExecutorKind::Container,
        "vm" | "kvm" => RunExecutorKind::Vm,
        other => {
            return Err(ReplayError::configuration(format!(
                "unsupported run.executor {other:?}"
            )))
        }
    };
    outer.run.timeout_ms = config.run.timeout_ms;
    outer.run.policy = match config.run.policy.as_deref().unwrap_or("observe") {
        "observe" => RunPolicy::Observe,
        "enforce" => RunPolicy::Enforce,
        other => {
            return Err(ReplayError::configuration(format!(
                "unsupported run.policy {other:?}"
            )))
        }
    };
    outer.run.inherit_env = config.run.inherit_env;
    outer.run.pass_env = config.run.pass_env.clone();
    outer.run.command = inner_replay_command(config, &executable)?;

    if config.overlayfs.base.is_some()
        || config.overlayfs.backend.is_some()
        || config.overlayfs.commit.is_some()
    {
        let mut overlay = OverlayFsSettings {
            base: config.overlayfs.base.clone(),
            ..OverlayFsSettings::default()
        };
        overlay.backend = match config.overlayfs.backend.as_deref().unwrap_or("directory") {
            "directory" => PVisorOverlayFsBackend::Directory,
            "jujutsu" => PVisorOverlayFsBackend::Jujutsu,
            other => {
                return Err(ReplayError::configuration(format!(
                    "unsupported overlayfs.backend {other:?}"
                )))
            }
        };
        overlay.commit = match config.overlayfs.commit.as_deref().unwrap_or("manual") {
            "manual" => PVisorOverlayFsCommit::Manual,
            "apply" => PVisorOverlayFsCommit::Apply,
            "drop" => PVisorOverlayFsCommit::Drop,
            other => {
                return Err(ReplayError::configuration(format!(
                    "unsupported overlayfs.commit {other:?}"
                )))
            }
        };
        outer.overlayfs = Some(overlay);
    }
    if let Some(mode) = config.overlaynet.mode.as_deref() {
        outer.overlaynet.mode = match mode {
            "auto" => PVisorOverlayNetMode::Auto,
            "off" => PVisorOverlayNetMode::Off,
            "proxy" => PVisorOverlayNetMode::Proxy,
            other => {
                return Err(ReplayError::configuration(format!(
                    "unsupported overlaynet.mode {other:?}"
                )))
            }
        };
    }
    if let Some(policy) = config.overlaynet.policy.as_deref() {
        outer.overlaynet.policy = match policy {
            "public" => PVisorOverlayNetPolicy::Public,
            "deny" => PVisorOverlayNetPolicy::Deny,
            "allowlist" => PVisorOverlayNetPolicy::Allowlist,
            other => {
                return Err(ReplayError::configuration(format!(
                    "unsupported overlaynet.policy {other:?}"
                )))
            }
        };
    }
    let rendered = toml::to_string_pretty(&outer).map_err(|error| {
        ReplayError::configuration(format!("serialize managed pVisor run config: {error}"))
    })?;
    let mut file = tempfile::Builder::new()
        .prefix("pvisor-replay-managed-")
        .suffix(".toml")
        .tempfile()
        .map_err(|error| {
            ReplayError::configuration(format!("create managed run config: {error}"))
        })?;
    file.write_all(rendered.as_bytes()).map_err(|error| {
        ReplayError::configuration(format!("write managed run config: {error}"))
    })?;
    file.flush().map_err(|error| {
        ReplayError::configuration(format!("flush managed run config: {error}"))
    })?;

    let working_dir = config
        .replay
        .workspace
        .clone()
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)
        .map_err(|error| {
            ReplayError::configuration(format!("resolve managed replay workspace: {error}"))
        })?;
    let mut command = Command::new(&executable);
    command.args(["run", "--config"]).arg(file.path());
    command.current_dir(&working_dir);
    if config.run.safe {
        command.arg("--safe");
    }
    let status = command.status().map_err(|error| {
        ReplayError::configuration(format!("start managed pVisor replay: {error}"))
    })?;
    Ok(status.code().unwrap_or(50))
}

fn inner_replay_command(
    config: &ReplayToml,
    executable: &std::path::Path,
) -> Result<Vec<String>, ReplayError> {
    let replay = &config.replay;
    let mut command = vec![
        path_string(executable)?,
        "replay".into(),
        "--agent".into(),
        replay.agent.clone(),
        "--trajectory".into(),
        path_string(&replay.trajectory)?,
        "--after-step".into(),
        replay.after_step.to_string(),
        "--workspace".into(),
        ".".into(),
        "--state-dir".into(),
        path_string(
            replay
                .state_dir
                .as_deref()
                .unwrap_or_else(|| std::path::Path::new("/tmp/pvisor-sandbox-replay/state")),
        )?,
        "--output-dir".into(),
        path_string(
            replay
                .output_dir
                .as_deref()
                .unwrap_or_else(|| std::path::Path::new("/tmp/pvisor-sandbox-replay/output")),
        )?,
    ];
    if let Some(entrypoint) = &replay.agent_entrypoint {
        command.extend(["--agent-entrypoint".into(), path_string(entrypoint)?]);
    }
    if let Some(runtime) = &replay.agent_runtime {
        command.extend(["--agent-runtime".into(), path_string(runtime)?]);
    }
    for tool in &replay.disallowed_tools {
        command.extend(["--agent-disallowed-tool".into(), tool.clone()]);
    }
    if let Some(assets) = &replay.trajectory_assets {
        command.extend(["--trajectory-assets".into(), path_string(assets)?]);
    }
    if let Some(session_id) = &replay.session_id {
        command.extend(["--session-id".into(), session_id.clone()]);
    }
    if let Some(max_steps) = replay.max_steps {
        command.extend(["--max-steps".into(), max_steps.to_string()]);
    }
    if replay.replay_only {
        command.push("--replay-only".into());
    }
    if replay.prepare_only {
        command.push("--prepare-only".into());
    }
    if replay.allow_stale_observations {
        command.push("--allow-stale-observations".into());
    }
    if replay.disable_thinking {
        command.push("--disable-thinking".into());
    }
    if let Some(run_id) = &replay.run_id {
        command.extend(["--run-id".into(), run_id.clone()]);
    }
    Ok(command)
}

fn path_string(path: &std::path::Path) -> Result<String, ReplayError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| ReplayError::configuration(format!("path is not UTF-8: {}", path.display())))
}

fn normalize(args: ReplayArgs) -> Result<PlaybackRequest, ReplayError> {
    let cwd = std::env::current_dir().map_err(|error| {
        ReplayError::configuration(format!("cannot read current directory: {error}"))
    })?;
    if let Some(config) = &args.config {
        reject_direct(&args)?;
        return ReplayToml::from_file(config)?.into_request(&cwd);
    }
    if let Some(request) = &args.request {
        reject_direct(&args)?;
        return request_from_json(request);
    }
    let agent = args
        .agent
        .as_deref()
        .ok_or_else(|| ReplayError::configuration("direct CLI mode requires --agent"))?;
    let trajectory = args
        .trajectory
        .ok_or_else(|| ReplayError::configuration("direct CLI mode requires --trajectory"))?;
    let after_step = args
        .after_step
        .ok_or_else(|| ReplayError::configuration("direct CLI mode requires --after-step"))?;
    Ok(PlaybackRequest {
        agent: AgentKind::from_str(agent).map_err(ReplayError::configuration)?,
        trajectory,
        after_step,
        workspace: args.workspace.unwrap_or(cwd),
        state_dir: args
            .state_dir
            .unwrap_or_else(|| PathBuf::from("/tmp/pvisor-sandbox-replay/state")),
        output_dir: args
            .output_dir
            .unwrap_or_else(|| PathBuf::from("/tmp/pvisor-sandbox-replay/output")),
        agent_entrypoint: args.agent_entrypoint,
        agent_runtime: args.agent_runtime,
        disallowed_tools: args.disallowed_tools,
        trajectory_assets: args.trajectory_assets,
        session_id: args.session_id,
        max_steps: args.max_steps,
        mode: if args.prepare_only {
            ReplayMode::PrepareOnly
        } else if args.replay_only {
            ReplayMode::ReplayOnly
        } else {
            ReplayMode::ReplayAndContinue
        },
        allow_stale_observations: args.allow_stale_observations,
        run_id: args.run_id,
        disable_thinking: args.disable_thinking,
    })
}

fn reject_direct(args: &ReplayArgs) -> Result<(), ReplayError> {
    let direct = args.agent.is_some()
        || args.trajectory.is_some()
        || args.after_step.is_some()
        || args.agent_entrypoint.is_some()
        || args.agent_runtime.is_some()
        || !args.disallowed_tools.is_empty()
        || args.trajectory_assets.is_some()
        || args.workspace.is_some()
        || args.state_dir.is_some()
        || args.output_dir.is_some()
        || args.session_id.is_some()
        || args.max_steps.is_some()
        || args.prepare_only
        || args.replay_only
        || args.allow_stale_observations
        || args.disable_thinking
        || args.run_id.is_some()
        || args.safe
        || args.executor.is_some()
        || args.timeout_ms.is_some()
        || args.policy.is_some()
        || args.inherit_env
        || !args.pass_env.is_empty()
        || args.overlayfs_base.is_some()
        || args.overlayfs_backend.is_some()
        || args.overlayfs_commit.is_some()
        || args.overlaynet_mode.is_some()
        || args.overlaynet_policy.is_some();
    if direct {
        return Err(ReplayError::configuration(
            "--config/--request cannot be combined with direct replay options",
        ));
    }
    Ok(())
}

fn print_error(error: &ReplayError) {
    println!("{}", failure_json(error));
}

fn failure_json(error: &ReplayError) -> serde_json::Value {
    let (run_id, state_dir, output_dir) = error
        .locations()
        .map(|(run_id, state_dir, output_dir)| (json!(run_id), json!(state_dir), json!(output_dir)))
        .unwrap_or((
            serde_json::Value::Null,
            serde_json::Value::Null,
            serde_json::Value::Null,
        ));
    json!({
        "schema_version": RESULT_SCHEMA_VERSION,
        "phase": null,
        "quality": null,
        "agent_status": "not_started",
        "run_id": run_id,
        "state_dir": state_dir,
        "output_dir": output_dir,
        "artifacts": [],
        "failure": {
            "category": error.kind.category(),
            "message": error.to_string(),
        },
        "retryable": error.kind.retryable(),
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_runtime_toml_selects_managed_outer_run() {
        let config: ReplayToml = toml::from_str(
            r#"
[replay]
agent = "claude-code"
trajectory = "/input/session.jsonl"
after_step = 30
agent_entrypoint = "/usr/bin/claude"
max_steps = 200
session_id = "task-291-attempt-1"
disallowed_tools = []
disable_thinking = true

[run]
safe = true
executor = "host"
timeout_ms = 3600000
policy = "enforce"
inherit_env = false
pass_env = ["OPENAI_BASE_URL", "OPENAI_API_KEY", "MODEL_NAME"]

[overlayfs]
base = "/workspace"
backend = "directory"
commit = "manual"

[overlaynet]
mode = "proxy"
policy = "allowlist"
"#,
        )
        .unwrap();
        assert!(needs_managed_run(&config));
        let command =
            inner_replay_command(&config, std::path::Path::new("/usr/bin/pvisor")).unwrap();
        assert_eq!(command[0], "/usr/bin/pvisor");
        assert!(command.windows(2).any(|pair| pair == ["--workspace", "."]));
        assert!(command
            .windows(2)
            .any(|pair| { pair == ["--state-dir", "/tmp/pvisor-sandbox-replay/state"] }));
        assert!(command
            .windows(2)
            .any(|pair| { pair == ["--output-dir", "/tmp/pvisor-sandbox-replay/output"] }));
        assert!(command
            .windows(2)
            .any(|pair| pair == ["--agent-entrypoint", "/usr/bin/claude"]));
        assert!(command
            .iter()
            .any(|argument| argument == "--disable-thinking"));
    }

    #[test]
    fn minimal_toml_stays_in_current_fresh_sandbox() {
        let mut config: ReplayToml = toml::from_str(
            r#"
[replay]
agent = "claude-code"
trajectory = "/input/session.jsonl"
after_step = 30
agent_entrypoint = "/usr/bin/claude"
"#,
        )
        .unwrap();
        assert!(!needs_managed_run(&config));
        config.run.inherit_env = true;
        assert!(needs_managed_run(&config));
    }

    #[test]
    fn managed_command_propagates_prepare_and_stale_observation_flags() {
        let config: ReplayToml = toml::from_str(
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

        let command =
            inner_replay_command(&config, std::path::Path::new("/usr/bin/pvisor")).unwrap();

        assert!(command.iter().any(|argument| argument == "--prepare-only"));
        assert!(command
            .iter()
            .any(|argument| argument == "--allow-stale-observations"));
        assert!(!command.iter().any(|argument| argument == "--replay-only"));
    }

    #[test]
    fn failure_json_keeps_run_locations() {
        let error = ReplayError::configuration("invalid request").with_locations(
            "replay-1",
            PathBuf::from("/state/replay-1"),
            PathBuf::from("/output/replay-1"),
        );

        let value = failure_json(&error);

        assert_eq!(value["schema_version"], "sandbox-playback.result/v3");
        assert_eq!(value["run_id"], "replay-1");
        assert_eq!(value["state_dir"], "/state/replay-1");
        assert_eq!(value["output_dir"], "/output/replay-1");
        assert_eq!(value["failure"]["category"], "configuration_error");
    }
}
