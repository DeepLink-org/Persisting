use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{bail, Context};
use clap::{Args, ValueEnum};
use persisting_gateway::config::{
    CaptureLevel, ModelRoute, NetworkConfig, NetworkMode, OverlayBackend, OverlayConfig,
    ProxyConfig,
};
use persisting_gateway::sink::SeqOnlySink;
use persisting_proto::{PolicyMode, RunInvocation, RunSpec, RunState, StdioMode};
use serde::Deserialize;

use crate::config::{
    ChronicleMode, GatewayMode, OverlayFsBackend, OverlayFsCommit, OverlayFsMode, OverlayNetMode,
    OverlayNetPolicy, RunConfig, RunPolicy, RunStdio,
};
use crate::runtime::{RunLease, RunRecord};
use crate::{unix_now_ms, GatewayDriverConfig, OverlayHint, PVisor, TrajectoryEventSink};

use super::trajectory::{chronicle_sink, ChronicleWriter};

#[derive(Debug, Clone, Args)]
pub struct RunArgs {
    /// Optional complete pVisor Run configuration; explicit CLI values replace matching fields.
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,

    #[command(flatten, next_help_heading = "Run options")]
    run: RunOverrides,
    #[command(flatten, next_help_heading = "OverlayFS options")]
    overlayfs: OverlayFsOverrides,
    #[command(flatten, next_help_heading = "OverlayNet options")]
    overlaynet: OverlayNetOverrides,
    #[command(flatten, next_help_heading = "Gateway options")]
    gateway: GatewayOverrides,
    #[command(flatten, next_help_heading = "pChronicle options")]
    chronicle: ChronicleOverrides,

    /// Agent command; replaces `run.command` from the config file.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

#[derive(Debug, Clone, Default, Args)]
struct RunOverrides {
    /// Exact durable directory for this Run; no Run-id child is added.
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
    #[arg(long)]
    agent: Option<String>,
    #[arg(long)]
    timeout_ms: Option<u64>,
    #[arg(long, value_enum)]
    stdio: Option<RunStdio>,
    #[arg(long, value_enum)]
    policy: Option<RunPolicy>,
}

#[derive(Debug, Clone, Default, Args)]
struct OverlayFsOverrides {
    #[arg(long, value_enum)]
    overlayfs_mode: Option<OverlayFsMode>,
    /// Primary read-only lower and default apply destination.
    #[arg(long, value_name = "DIR")]
    overlayfs_target: Option<PathBuf>,
    /// Additional lower layer; repeat to add multiple layers.
    #[arg(long, value_name = "DIR")]
    overlayfs_lower: Vec<PathBuf>,
    #[arg(long, value_enum)]
    overlayfs_backend: Option<OverlayFsBackend>,
    /// Versioned upper stage, for example `jj:/tmp/shared.jj@fork-a`.
    #[arg(
        long,
        value_name = "BACKEND:STORE@FORK",
        conflicts_with = "overlayfs_backend"
    )]
    overlayfs_stage: Option<OverlayFsStage>,
    #[arg(long, value_enum)]
    overlayfs_commit: Option<OverlayFsCommit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OverlayFsStage {
    Jujutsu { store: PathBuf, workspace: String },
}

impl FromStr for OverlayFsStage {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some(address) = value.strip_prefix("jj:") else {
            return Err(format!(
                "unsupported OverlayFS stage {value:?}; expected jj:<store>@<fork>"
            ));
        };
        let Some((store, workspace)) = address.rsplit_once('@') else {
            return Err("invalid Jujutsu stage; expected jj:<store>@<fork>".into());
        };
        if store.is_empty() {
            return Err("invalid Jujutsu stage: store path cannot be empty".into());
        }
        if workspace.is_empty() {
            return Err("invalid Jujutsu stage: fork name cannot be empty".into());
        }
        Ok(Self::Jujutsu {
            store: PathBuf::from(store),
            workspace: workspace.to_owned(),
        })
    }
}

#[derive(Debug, Clone, Default, Args)]
struct OverlayNetOverrides {
    #[arg(long, value_enum)]
    overlaynet_mode: Option<OverlayNetMode>,
    #[arg(long, value_name = "ADDR")]
    overlaynet_listen: Option<String>,
    #[arg(long, value_enum)]
    overlaynet_policy: Option<OverlayNetPolicy>,
    /// Allowed host or host pattern; repeat to replace the configured allow list.
    #[arg(long, value_name = "HOST")]
    overlaynet_allow: Vec<String>,
}

#[derive(Debug, Clone, Default, Args)]
struct GatewayOverrides {
    #[arg(long, value_enum)]
    gateway_mode: Option<GatewayMode>,
    #[arg(long, value_name = "ADDR")]
    gateway_admin_listen: Option<String>,
    #[arg(long, value_enum)]
    gateway_level: Option<GatewayLevel>,
    #[arg(long, value_name = "HEADER")]
    gateway_session_header: Option<String>,
    /// Enable or disable Gateway diagnostics.
    #[arg(long, value_name = "BOOL", num_args = 0..=1, default_missing_value = "true")]
    gateway_debug: Option<bool>,
    /// Enable or disable the live Markdown projection.
    #[arg(long, value_name = "BOOL", num_args = 0..=1, default_missing_value = "true")]
    gateway_stream_markdown: Option<bool>,
    /// TOML inline-table fields for one model route; repeat to replace configured routes.
    #[arg(long, value_name = "ROUTE")]
    gateway_route: Vec<GatewayRouteArg>,
}

#[derive(Debug, Clone, Default, Args)]
struct ChronicleOverrides {
    #[arg(long, value_enum)]
    chronicle_mode: Option<ChronicleMode>,
    #[arg(long, value_name = "DIR")]
    chronicle_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum GatewayLevel {
    Summary,
    Dialogue,
    Full,
}

impl From<GatewayLevel> for CaptureLevel {
    fn from(level: GatewayLevel) -> Self {
        match level {
            GatewayLevel::Summary => Self::Summary,
            GatewayLevel::Dialogue => Self::Dialogue,
            GatewayLevel::Full => Self::Full,
        }
    }
}

#[derive(Debug, Clone)]
struct GatewayRouteArg(ModelRoute);

impl FromStr for GatewayRouteArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        #[derive(Deserialize)]
        struct Wrapper {
            route: ModelRoute,
        }
        let source = format!("route = {{ {value} }}");
        toml::from_str::<Wrapper>(&source)
            .map(|wrapper| Self(wrapper.route))
            .map_err(|error| format!("invalid Gateway route: {error}"))
    }
}

pub async fn run(args: RunArgs) -> anyhow::Result<i32> {
    let mut config = args
        .config
        .as_deref()
        .map(RunConfig::from_file)
        .transpose()
        .context("load pVisor Run config")?
        .unwrap_or_default();
    apply_cli(&mut config, args);
    validate(&config)?;

    let run_id = format!("run-{}", uuid::Uuid::new_v4());
    let workspace = config
        .run
        .workspace
        .as_deref()
        .map(resolve_workspace)
        .transpose()?;
    if let Some(workspace) = &workspace {
        if let Ok(existing) = RunRecord::read(workspace) {
            bail!(
                "pVisor workspace {} already belongs to Run {}; choose a new workspace",
                workspace.display(),
                existing.run_id
            );
        }
    }
    let overlay = resolve_overlay(&config, workspace.as_deref())?;
    let proxy = resolve_proxy(&config)?;
    let storage = workspace
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(&run_id));

    if config.gateway.debug {
        persisting_gateway::debug::enable_debug(&storage)?;
    }

    let chronicle_dir = config
        .chronicle
        .dir
        .clone()
        .or_else(|| workspace.as_ref().map(|path| path.join("chronicle")));
    let (sink, writer): (Arc<dyn TrajectoryEventSink>, Option<ChronicleWriter>) =
        match config.chronicle.mode {
            ChronicleMode::Off => (Arc::new(SeqOnlySink::new()), None),
            ChronicleMode::Lance => {
                let dir = chronicle_dir.context("pChronicle requires a directory")?;
                let (sink, writer) = chronicle_sink(&dir, &config.run.agent);
                (sink, Some(writer))
            }
        };

    let mut builder = PVisor::builder().storage(&storage).trajectory_sink(sink);
    if let Some(proxy) = proxy {
        builder = builder.gateway(
            GatewayDriverConfig::new(proxy)
                .output_dir(&storage)
                .stream_markdown(config.gateway.stream_markdown)
                .gateway_enabled(config.gateway.mode == GatewayMode::Capture),
        );
    }
    if let Some(overlay) = overlay {
        builder = builder.overlay(overlay);
    }
    let pvisor = builder.build();

    let (program, program_args) = config
        .run
        .command
        .split_first()
        .context("missing Agent command; pass it after `--` or set run.command")?;
    let mut spec = RunSpec::process(run_id.as_str(), &config.run.agent, program);
    let RunInvocation::Process(process) = &mut spec.invocation;
    process.args = program_args.to_vec();
    process.stdin = StdioMode::Inherit;
    process.stdout = match config.run.stdio {
        RunStdio::Inherit => StdioMode::Inherit,
        RunStdio::Capture => StdioMode::Capture,
    };
    process.stderr = process.stdout;
    spec.runtime.timeout_ms = config.run.timeout_ms;
    if config.run.policy == RunPolicy::Enforce {
        spec.runtime.policy_mode = PolicyMode::Enforce;
    }

    let managed_by_driver = config.overlayfs.mode == OverlayFsMode::Overlay
        || config.overlaynet.mode == OverlayNetMode::Proxy;
    let mut ambient = if !managed_by_driver && workspace.is_some() {
        let record = RunRecord {
            schema_version: 1,
            run_id: spec.run_id.as_str().to_string(),
            session_id: spec.run_id.as_str().to_string(),
            agent: spec.agent.name.clone(),
            pid: std::process::id(),
            command: config.run.command.clone(),
            state: "running".into(),
            started_at_unix_ms: unix_now_ms(),
            finished_at_unix_ms: None,
            storage: storage.clone(),
            overlaynet_listen: None,
            gateway_listen: None,
            network: serde_json::to_value(&spec.capabilities.network)?,
            overlay: None,
            overlay_lowers: Vec::new(),
        };
        let lease = RunLease::acquire(&record.stage_dir())?;
        record.write()?;
        Some((record, lease))
    } else {
        None
    };

    let result = pvisor.run(spec).await?.wait().await?;
    if let Some((record, _lease)) = &mut ambient {
        record.state = match result.state {
            RunState::Completed => "completed",
            RunState::Cancelled => "cancelled",
            _ => "failed",
        }
        .into();
        record.finished_at_unix_ms = Some(unix_now_ms());
        record.write()?;
    }
    drop(ambient);
    drop(pvisor);
    if let Some(writer) = writer {
        writer.finish()?;
    }
    if result.state != RunState::Completed {
        if let Some(failure) = &result.failure {
            eprintln!("pVisor Run failed: {:?}: {}", failure.kind, failure.message);
        }
        for warning in &result.warnings {
            eprintln!("pVisor Run warning: {warning}");
        }
    }
    Ok(match result.state {
        RunState::Completed => result.exit_code.unwrap_or(0),
        RunState::Cancelled => 130,
        _ => result.exit_code.unwrap_or(1),
    })
}

fn apply_cli(config: &mut RunConfig, args: RunArgs) {
    if let Some(value) = args.run.workspace {
        config.run.workspace = Some(value);
    }
    if let Some(value) = args.run.agent {
        config.run.agent = value;
    }
    if let Some(value) = args.run.timeout_ms {
        config.run.timeout_ms = Some(value);
    }
    if let Some(value) = args.run.stdio {
        config.run.stdio = value;
    }
    if let Some(value) = args.run.policy {
        config.run.policy = value;
    }
    if !args.command.is_empty() {
        config.run.command = args.command;
    }

    if let Some(value) = args.overlayfs.overlayfs_mode {
        config.overlayfs.mode = value;
    }
    if let Some(value) = args.overlayfs.overlayfs_target {
        config.overlayfs.target = Some(value);
    }
    if !args.overlayfs.overlayfs_lower.is_empty() {
        config.overlayfs.lower = args.overlayfs.overlayfs_lower;
    }
    if let Some(value) = args.overlayfs.overlayfs_backend {
        config.overlayfs.backend = value;
    }
    if let Some(stage) = args.overlayfs.overlayfs_stage {
        match stage {
            OverlayFsStage::Jujutsu { store, workspace } => {
                config.overlayfs.backend = OverlayFsBackend::Jujutsu;
                config.overlayfs.jujutsu_store = Some(store);
                config.overlayfs.jujutsu_workspace = Some(workspace);
            }
        }
    }
    if let Some(value) = args.overlayfs.overlayfs_commit {
        config.overlayfs.commit = value;
    }

    if let Some(value) = args.overlaynet.overlaynet_mode {
        config.overlaynet.mode = value;
    }
    if let Some(value) = args.overlaynet.overlaynet_listen {
        config.overlaynet.listen = value;
    }
    if let Some(value) = args.overlaynet.overlaynet_policy {
        config.overlaynet.policy = value;
    }
    if !args.overlaynet.overlaynet_allow.is_empty() {
        config.overlaynet.allow = args.overlaynet.overlaynet_allow;
    }

    if let Some(value) = args.gateway.gateway_mode {
        config.gateway.mode = value;
    }
    if let Some(value) = args.gateway.gateway_admin_listen {
        config.gateway.admin_listen = value;
    }
    if let Some(value) = args.gateway.gateway_level {
        config.gateway.level = value.into();
    }
    if let Some(value) = args.gateway.gateway_session_header {
        config.gateway.session_header = value;
    }
    if let Some(value) = args.gateway.gateway_debug {
        config.gateway.debug = value;
    }
    if let Some(value) = args.gateway.gateway_stream_markdown {
        config.gateway.stream_markdown = value;
    }
    if !args.gateway.gateway_route.is_empty() {
        config.gateway.routes = args
            .gateway
            .gateway_route
            .into_iter()
            .map(|route| route.0)
            .collect();
    }

    if let Some(value) = args.chronicle.chronicle_mode {
        config.chronicle.mode = value;
    }
    if let Some(value) = args.chronicle.chronicle_dir {
        config.chronicle.dir = Some(value);
    }
}

fn validate(config: &RunConfig) -> anyhow::Result<()> {
    if config.run.command.is_empty() {
        bail!("missing Agent command; pass it after `--` or set run.command");
    }
    let needs_workspace = config.overlayfs.mode == OverlayFsMode::Overlay
        || config.overlaynet.mode == OverlayNetMode::Proxy
        || config.gateway.mode == GatewayMode::Capture
        || config.chronicle.mode == ChronicleMode::Lance;
    if needs_workspace && config.run.workspace.is_none() {
        bail!("--workspace is required when a persistent runtime driver is enabled");
    }
    match config.overlayfs.mode {
        OverlayFsMode::Host => {
            if config.overlayfs.target.is_some() || !config.overlayfs.lower.is_empty() {
                bail!("OverlayFS target/lower paths require --overlayfs-mode overlay");
            }
        }
        OverlayFsMode::Overlay => {
            if config.overlayfs.target.is_none() {
                bail!("--overlayfs-target is required with --overlayfs-mode overlay");
            }
        }
    }
    if config.overlaynet.mode == OverlayNetMode::Off {
        if config.overlaynet.policy != OverlayNetPolicy::Public
            || !config.overlaynet.allow.is_empty()
        {
            bail!("OverlayNet policy options require --overlaynet-mode proxy");
        }
        if config.gateway.mode == GatewayMode::Capture {
            bail!("--gateway-mode capture requires --overlaynet-mode proxy");
        }
    }
    if config.overlaynet.mode == OverlayNetMode::Proxy {
        let listen: std::net::SocketAddr = config.overlaynet.listen.parse().with_context(|| {
            format!(
                "invalid OverlayNet listen address {}",
                config.overlaynet.listen
            )
        })?;
        if listen.port() == 0 {
            bail!("OverlayNet port 0 is not supported; choose an explicit free port");
        }
    }
    if config.overlaynet.policy != OverlayNetPolicy::Allowlist
        && !config.overlaynet.allow.is_empty()
    {
        bail!("--overlaynet-allow requires --overlaynet-policy allowlist");
    }
    match config.gateway.mode {
        GatewayMode::Off if !config.gateway.routes.is_empty() => {
            bail!("Gateway routes require --gateway-mode capture");
        }
        GatewayMode::Capture if config.gateway.routes.is_empty() => {
            bail!("--gateway-mode capture requires at least one --gateway-route");
        }
        _ => {}
    }
    Ok(())
}

fn resolve_workspace(workspace: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(workspace)
        .with_context(|| format!("create pVisor workspace {}", workspace.display()))?;
    workspace
        .canonicalize()
        .with_context(|| format!("resolve pVisor workspace {}", workspace.display()))
}

fn resolve_directory(path: &Path, description: &str) -> anyhow::Result<PathBuf> {
    let path = path
        .canonicalize()
        .with_context(|| format!("resolve {description} {}", path.display()))?;
    anyhow::ensure!(
        path.is_dir(),
        "{description} must be a directory: {}",
        path.display()
    );
    Ok(path)
}

fn resolve_overlay(
    config: &RunConfig,
    workspace: Option<&Path>,
) -> anyhow::Result<Option<OverlayHint>> {
    if config.overlayfs.mode == OverlayFsMode::Host {
        return Ok(None);
    }
    let workspace = workspace.context("OverlayFS requires a workspace")?;
    let target = resolve_directory(
        config
            .overlayfs
            .target
            .as_deref()
            .context("OverlayFS target missing")?,
        "OverlayFS target",
    )?;
    anyhow::ensure!(
        !paths_overlap(&target, workspace),
        "OverlayFS target and workspace must not overlap: target={}, workspace={}",
        target.display(),
        workspace.display()
    );
    let mut lowers = vec![target];
    for lower in &config.overlayfs.lower {
        lowers.push(resolve_directory(lower, "OverlayFS lower")?);
    }
    Ok(Some(OverlayHint {
        lower_dirs: lowers,
        stage_dir: Some(workspace.to_path_buf()),
        backend: match config.overlayfs.backend {
            OverlayFsBackend::Directory => OverlayBackend::Directory,
            OverlayFsBackend::Jujutsu => OverlayBackend::Jujutsu,
        },
        jujutsu_store_path: config.overlayfs.jujutsu_store.clone(),
        jujutsu_workspace: config.overlayfs.jujutsu_workspace.clone(),
        auto_apply: config.overlayfs.commit == OverlayFsCommit::Apply,
        auto_discard: config.overlayfs.commit == OverlayFsCommit::Drop,
        ..OverlayHint::default()
    }))
}

fn resolve_proxy(config: &RunConfig) -> anyhow::Result<Option<ProxyConfig>> {
    if config.overlaynet.mode == OverlayNetMode::Off {
        return Ok(None);
    }
    let network = NetworkConfig {
        mode: match config.overlaynet.policy {
            OverlayNetPolicy::Public => NetworkMode::Public,
            OverlayNetPolicy::Deny => NetworkMode::NoNetwork,
            OverlayNetPolicy::Allowlist => NetworkMode::Allowlist,
        },
        allowed_hosts: config.overlaynet.allow.clone(),
    };
    let proxy = ProxyConfig {
        listen: config.overlaynet.listen.clone(),
        admin_listen: config.gateway.admin_listen.clone(),
        agent_id: config.run.agent.clone(),
        session_header: config.gateway.session_header.clone(),
        capture_level: config.gateway.level,
        debug: config.gateway.debug,
        network,
        overlay: OverlayConfig::default(),
        models: if config.gateway.mode == GatewayMode::Capture {
            config.gateway.routes.clone()
        } else {
            Vec::new()
        },
    };
    proxy.validate()?;
    Ok(Some(proxy))
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    use crate::cli::Cli;

    #[test]
    fn cli_can_express_all_driver_domains_without_a_config_file() {
        Cli::try_parse_from([
            "pvisor",
            "run",
            "--workspace",
            "/tmp/run",
            "--overlayfs-mode",
            "overlay",
            "--overlayfs-target",
            "/tmp/lower",
            "--overlaynet-mode",
            "proxy",
            "--overlaynet-policy",
            "allowlist",
            "--overlaynet-allow",
            "api.openai.com",
            "--gateway-mode",
            "capture",
            "--gateway-route",
            r#"name="openai", upstream="https://api.openai.com/v1""#,
            "--chronicle-mode",
            "lance",
            "--",
            "codex",
        ])
        .expect("complete command line should parse");
    }

    #[test]
    fn cli_lists_replace_config_lists() {
        let mut config = RunConfig::default();
        config.overlaynet.allow = vec!["old.example".into()];
        let crate::cli::Command::Run(args) = Cli::try_parse_from([
            "pvisor",
            "run",
            "--overlaynet-allow",
            "new.example",
            "--",
            "true",
        ])
        .unwrap()
        .command
        else {
            unreachable!()
        };
        apply_cli(&mut config, *args);
        assert_eq!(config.overlaynet.allow, ["new.example"]);
    }

    #[test]
    fn cli_selects_named_workspace_in_shared_jujutsu_store() {
        let crate::cli::Command::Run(args) = Cli::try_parse_from([
            "pvisor",
            "run",
            "--overlayfs-stage",
            "jj:/tmp/shared.jj@fork-a",
            "--",
            "true",
        ])
        .unwrap()
        .command
        else {
            unreachable!()
        };
        let mut config = RunConfig::default();
        apply_cli(&mut config, *args);
        assert_eq!(config.overlayfs.backend, OverlayFsBackend::Jujutsu);
        assert_eq!(
            config.overlayfs.jujutsu_store.as_deref(),
            Some(Path::new("/tmp/shared.jj"))
        );
        assert_eq!(
            config.overlayfs.jujutsu_workspace.as_deref(),
            Some("fork-a")
        );
    }

    #[test]
    fn overlayfs_stage_uses_the_final_at_sign_as_the_fork_separator() {
        assert_eq!(
            "jj:/tmp/user@example/store@fork-a"
                .parse::<OverlayFsStage>()
                .unwrap(),
            OverlayFsStage::Jujutsu {
                store: PathBuf::from("/tmp/user@example/store"),
                workspace: "fork-a".into(),
            }
        );
    }

    #[test]
    fn overlayfs_stage_rejects_incomplete_addresses() {
        for value in [
            "unsupported:/tmp/store@fork-a",
            "jj:/tmp/store",
            "jj:@fork-a",
            "jj:/tmp/store@",
        ] {
            assert!(value.parse::<OverlayFsStage>().is_err(), "accepted {value}");
        }
    }

    #[test]
    fn overlayfs_stage_conflicts_with_an_explicit_backend() {
        let error = Cli::try_parse_from([
            "pvisor",
            "run",
            "--overlayfs-backend",
            "directory",
            "--overlayfs-stage",
            "jj:/tmp/shared.jj@fork-a",
            "--",
            "true",
        ])
        .unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
}
