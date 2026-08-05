use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{bail, Context};
use clap::{Args, ValueEnum};
use persisting_control::{PolicyMode, RunInvocation, RunSpec, RunState, StdioMode};
use persisting_gateway::config::{
    CaptureLevel, ModelRoute, NetworkConfig, NetworkMode, OverlayBackend, OverlayConfig,
    ProxyConfig,
};
use persisting_gateway::sink::SeqOnlySink;
use persisting_overlaynet::{NetworkAccessRule, NetworkBandwidthLimit};
use serde::Deserialize;

use crate::config::{
    ChronicleMode, ContainerMount, ContainerNetwork, ContainerPlatform, GatewayMode,
    KvmArchitecture, KvmImageFormat, OverlayFsBackend, OverlayFsCommit, OverlayFsMode,
    OverlayNetMode, OverlayNetPolicy, OverlayNetSettings, RunConfig, RunExecutorKind, RunPolicy,
    RunStdio,
};
use crate::runtime::{default_run_home, resolve_run, RunLineage, RunRecord};
use crate::{
    latest_logical_checkpoint, restore_logical_checkpoint, ContainerExecutor, GatewayDriverConfig,
    KvmExecutor, LogicalCheckpoint, OverlayHint, PVisor, ProcessExecutor, RunBundle, RunExecutor,
    TrajectoryEventSink,
};

use super::trajectory::{chronicle_sink, ChronicleWriter};

#[derive(Debug, Clone, Args)]
pub struct RunArgs {
    /// Optional complete pVisor Run configuration; explicit CLI values replace matching fields.
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Execute a prepared RunSpec through the normal pVisor host executor.
    #[arg(long, value_name = "FILE", conflicts_with = "config")]
    run_spec: Option<PathBuf>,

    /// Atomically write the delegated RunResult as JSON.
    #[arg(long, value_name = "FILE", requires = "run_spec")]
    result_file: Option<PathBuf>,

    /// Stage workspace changes and enable best-available low-privilege network review controls.
    #[arg(long)]
    safe: bool,

    #[command(flatten, next_help_heading = "Run options")]
    run: RunOverrides,
    #[command(flatten, next_help_heading = "Container executor options")]
    container: ContainerOverrides,
    #[command(flatten, next_help_heading = "KVM executor options")]
    kvm: KvmOverrides,
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

#[derive(Debug, Clone, Args)]
pub struct ForkArgs {
    /// Source Run id, workspace, run.json, or path inside the source Run.
    source: PathBuf,
    /// Logical checkpoint id; the latest checkpoint is used when omitted.
    #[arg(long, value_name = "ID")]
    checkpoint: Option<String>,
    /// New durable Run workspace.
    #[arg(long, value_name = "DIR")]
    workspace: PathBuf,
    #[arg(long, short = 'o', default_value = ".persisting/capture")]
    output_dir: PathBuf,
    /// Agent command; defaults to the source Run command.
    #[arg(last = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

#[derive(Debug, Clone, Default, Args)]
struct RunOverrides {
    /// Exact durable directory for this Run; no Run-id child is added.
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
    #[arg(long)]
    agent: Option<String>,
    /// Execution provider for the Agent command.
    #[arg(long, value_enum)]
    executor: Option<RunExecutorKind>,
    #[arg(long)]
    timeout_ms: Option<u64>,
    #[arg(long, value_enum)]
    stdio: Option<RunStdio>,
    #[arg(long, value_enum)]
    policy: Option<RunPolicy>,
}

#[derive(Debug, Clone, Default, Args)]
struct ContainerOverrides {
    /// Docker/Podman-compatible OCI runtime executable.
    #[arg(long, value_name = "PATH")]
    container_runtime: Option<PathBuf>,
    /// OCI image containing the Agent command. Supplying this selects the container executor.
    #[arg(long, value_name = "IMAGE")]
    container_image: Option<String>,
    /// Statically linked Linux pVisor injected into the container.
    #[arg(long, value_name = "PATH")]
    container_pvisor_binary: Option<PathBuf>,
    /// OCI target platform (`linux/amd64` or `linux/arm64`).
    #[arg(long, value_name = "PLATFORM")]
    container_platform: Option<ContainerPlatform>,
    /// Container network mode; host keeps the in-process Gateway reachable.
    #[arg(long, value_enum)]
    container_network: Option<ContainerNetwork>,
    /// Container-native workdir used when pVisor does not inject an OverlayFS cwd.
    #[arg(long, value_name = "PATH")]
    container_workdir: Option<PathBuf>,
    /// Container user (`uid`, `uid:gid`, or name).
    #[arg(long, value_name = "USER")]
    container_user: Option<String>,
    /// Mount the image root filesystem read-only.
    #[arg(long, value_name = "BOOL", num_args = 0..=1, default_missing_value = "true")]
    container_read_only_rootfs: Option<bool>,
    /// TOML inline-table bind mount; repeat to replace configured mounts.
    #[arg(long, value_name = "MOUNT")]
    container_mount: Vec<ContainerMountArg>,
}

#[derive(Debug, Clone, Default, Args)]
struct KvmOverrides {
    /// QEMU system emulator executable.
    #[arg(long, value_name = "PATH")]
    kvm_qemu: Option<PathBuf>,
    /// Bootable Linux qcow2/raw guest image; supplying it selects the KVM executor.
    #[arg(long, value_name = "PATH")]
    kvm_image: Option<PathBuf>,
    #[arg(long, value_enum)]
    kvm_image_format: Option<KvmImageFormat>,
    #[arg(long, value_enum)]
    kvm_architecture: Option<KvmArchitecture>,
    /// Matching statically linked Linux pVisor copied into the guest.
    #[arg(long, value_name = "PATH")]
    kvm_pvisor_binary: Option<PathBuf>,
    #[arg(long, value_name = "MIB")]
    kvm_memory_mib: Option<u32>,
    #[arg(long, value_name = "COUNT")]
    kvm_cpus: Option<u16>,
    #[arg(long, value_name = "PATH")]
    kvm_ssh: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    kvm_scp: Option<PathBuf>,
    #[arg(long, value_name = "USER")]
    kvm_ssh_user: Option<String>,
    #[arg(long, value_name = "PATH")]
    kvm_ssh_key: Option<PathBuf>,
    #[arg(long, value_name = "PORT")]
    kvm_ssh_port: Option<u16>,
    #[arg(long, value_name = "MS")]
    kvm_boot_timeout_ms: Option<u64>,
    #[arg(long, value_name = "PATH")]
    kvm_firmware: Option<PathBuf>,
    #[arg(long, value_name = "BOOL", num_args = 0..=1, default_missing_value = "true")]
    kvm_snapshot: Option<bool>,
}

#[derive(Debug, Clone)]
struct ContainerMountArg(ContainerMount);

impl FromStr for ContainerMountArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        #[derive(Deserialize)]
        struct Wrapper {
            mount: ContainerMount,
        }
        let source = format!("mount = {{ {value} }}");
        toml::from_str::<Wrapper>(&source)
            .map(|wrapper| Self(wrapper.mount))
            .map_err(|error| format!("invalid container mount: {error}"))
    }
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
    #[arg(long, value_enum, hide = true)]
    overlaynet_mode: Option<OverlayNetMode>,
    /// Explicit proxy listen address; supplying it enables OverlayNet and requires --workspace.
    #[arg(long, value_name = "ADDR")]
    overlaynet_listen: Option<String>,
    #[arg(long, value_enum, hide = true)]
    overlaynet_policy: Option<OverlayNetPolicy>,
    /// Allowed HOST[:PORT] or CIDR[:PORT]; enables the cooperative proxy and requires --workspace.
    #[arg(long, value_name = "TARGET")]
    overlaynet_allow: Vec<OverlayNetTargetArg>,
    /// Denied HOST[:PORT] or CIDR[:PORT]; enables the cooperative proxy and requires --workspace.
    #[arg(long, value_name = "TARGET")]
    overlaynet_deny: Vec<OverlayNetTargetArg>,
    /// Aggregate bandwidth limit; enables the cooperative proxy and requires --workspace.
    #[arg(long, value_name = "[TARGET=]RATE")]
    overlaynet_limit: Vec<OverlayNetLimitArg>,
    /// Deny all forward-proxy egress and enable OverlayNet; requires --workspace.
    /// Direct sockets and local Gateway routes remain outside this rule.
    #[arg(
        long,
        conflicts_with_all = [
            "overlaynet_allow",
            "overlaynet_deny",
            "overlaynet_limit",
            "overlaynet_rule",
            "overlaynet_policy"
        ]
    )]
    overlaynet_deny_all: bool,
    /// TOML inline-table fields for one structured rule; repeat to replace configured rules.
    #[arg(long, value_name = "RULE", hide = true)]
    overlaynet_rule: Vec<OverlayNetRuleArg>,
}

#[derive(Debug, Clone)]
struct OverlayNetTargetArg(NetworkAccessRule);

impl FromStr for OverlayNetTargetArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_overlaynet_target(value).map(Self)
    }
}

#[derive(Debug, Clone)]
struct OverlayNetLimitArg(NetworkBandwidthLimit);

impl FromStr for OverlayNetLimitArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (target, rate) = value
            .rsplit_once('=')
            .map_or((None, value), |(target, rate)| (Some(target), rate));
        let bytes_per_second = parse_bandwidth(rate)?;
        let target = target.map(parse_overlaynet_target).transpose()?;
        Ok(Self(NetworkBandwidthLimit {
            host: target.as_ref().map(|target| target.host.clone()),
            port: target.and_then(|target| target.ports.first().copied()),
            bytes_per_second,
        }))
    }
}

fn parse_overlaynet_target(value: &str) -> Result<NetworkAccessRule, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("OverlayNet target cannot be empty".into());
    }
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| format!("invalid bracketed OverlayNet target `{value}`"))?;
        let host = &rest[..end];
        let suffix = &rest[end + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            Some(
                suffix
                    .strip_prefix(':')
                    .ok_or_else(|| format!("invalid OverlayNet target `{value}`"))?
                    .parse::<u16>()
                    .map_err(|_| format!("invalid port in OverlayNet target `{value}`"))?,
            )
        };
        (host, port)
    } else if value.matches(':').count() <= 1 {
        match value.rsplit_once(':') {
            Some((host, port))
                if !host.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                (
                    host,
                    Some(
                        port.parse::<u16>()
                            .map_err(|_| format!("invalid port in OverlayNet target `{value}`"))?,
                    ),
                )
            }
            _ => (value, None),
        }
    } else {
        (value, None)
    };
    if port == Some(0) {
        return Err("OverlayNet target port must not be zero".into());
    }
    persisting_control::parse_network_rule(host).map_err(|error| error.to_string())?;
    Ok(NetworkAccessRule {
        host: host.to_string(),
        ports: port.into_iter().collect(),
        transports: Vec::new(),
        allow_private_ips: false,
    })
}

fn parse_bandwidth(value: &str) -> Result<u64, String> {
    let normalized = value.trim().to_ascii_lowercase();
    let units = [
        ("gbps", 1_000_000_000_u64, true),
        ("mbps", 1_000_000, true),
        ("kbps", 1_000, true),
        ("bps", 1, true),
        ("gb/s", 1_000_000_000, false),
        ("mb/s", 1_000_000, false),
        ("kb/s", 1_000, false),
        ("b/s", 1, false),
    ];
    for (suffix, multiplier, bits) in units {
        if let Some(amount) = normalized.strip_suffix(suffix) {
            let amount = amount
                .trim()
                .parse::<u64>()
                .map_err(|_| format!("invalid OverlayNet bandwidth `{value}`"))?;
            let scaled = amount
                .checked_mul(multiplier)
                .ok_or_else(|| format!("OverlayNet bandwidth `{value}` is too large"))?;
            let bytes = if bits { scaled.div_ceil(8) } else { scaled };
            return (bytes > 0)
                .then_some(bytes)
                .ok_or_else(|| "OverlayNet bandwidth must be greater than zero".into());
        }
    }
    Err(format!(
        "invalid OverlayNet bandwidth `{value}`; use e.g. `10mbps` or `2mb/s`"
    ))
}

#[derive(Debug, Clone)]
struct OverlayNetRuleArg(NetworkAccessRule);

impl FromStr for OverlayNetRuleArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        #[derive(Deserialize)]
        struct Wrapper {
            rule: NetworkAccessRule,
        }
        let source = format!("rule = {{ {value} }}");
        toml::from_str::<Wrapper>(&source)
            .map(|wrapper| Self(wrapper.rule))
            .map_err(|error| format!("invalid OverlayNet rule: {error}"))
    }
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
    /// Canonical Lance root; accepts a local directory or s3:// URI.
    #[arg(long, value_name = "PATH|S3_URI")]
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
    if args.run_spec.is_some() {
        return run_prepared_spec(args).await;
    }
    let safe = args.safe;
    let run_id = format!("run-{}", uuid::Uuid::new_v4());
    let mut config = args
        .config
        .as_deref()
        .map(RunConfig::from_file)
        .transpose()
        .context("load pVisor Run config")?
        .unwrap_or_default();
    apply_cli(&mut config, args);
    if safe {
        apply_safe_defaults(&mut config, &run_id)?;
    }
    execute_config(config, run_id, safe, None).await
}

async fn run_prepared_spec(args: RunArgs) -> anyhow::Result<i32> {
    anyhow::ensure!(!args.safe, "--safe cannot be combined with --run-spec");
    anyhow::ensure!(
        args.command.is_empty(),
        "a command cannot be combined with --run-spec"
    );
    anyhow::ensure!(
        args.run
            .executor
            .is_none_or(|executor| executor == RunExecutorKind::Host),
        "--run-spec must execute with --executor host"
    );
    let spec_path = args.run_spec.context("missing --run-spec")?;
    let result_path = args
        .result_file
        .context("--run-spec requires --result-file")?;
    let spec: RunSpec = serde_json::from_slice(
        &std::fs::read(&spec_path)
            .with_context(|| format!("read delegated RunSpec from {}", spec_path.display()))?,
    )
    .context("decode delegated RunSpec")?;
    let pvisor = PVisor::builder()
        .executors(vec![Arc::new(ProcessExecutor)])
        .build();
    let handle = pvisor.run(spec).await?;
    let agent_abi = handle.agent_abi();
    let cancellation = handle.cancellation();
    let wait = handle.wait();
    tokio::pin!(wait);
    let result = tokio::select! {
        result = &mut wait => result?,
        _ = delegated_shutdown_signal() => {
            cancellation.cancel();
            wait.await?
        }
    };
    let output = crate::delegated::DelegatedRunOutput {
        agent_abi: agent_abi.snapshot(),
        result,
    };
    crate::delegated::write_result(&result_path, &output)
        .with_context(|| format!("write delegated RunResult to {}", result_path.display()))?;
    Ok(match output.result.state {
        RunState::Completed => output.result.exit_code.unwrap_or(0),
        RunState::Cancelled => 130,
        _ => output.result.exit_code.unwrap_or(1),
    })
}

async fn delegated_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

pub async fn fork(args: ForkArgs) -> anyhow::Result<i32> {
    let storage = args
        .output_dir
        .canonicalize()
        .unwrap_or(args.output_dir.clone());
    let source = resolve_run(Some(&args.source), &storage)?;
    let checkpoint = match args.checkpoint.as_deref() {
        Some(id) => {
            anyhow::ensure!(
                !id.trim().is_empty()
                    && id != "."
                    && id != ".."
                    && !id.contains('/')
                    && !id.contains('\\'),
                "checkpoint id must be one non-empty path-safe segment"
            );
            LogicalCheckpoint::read(&source.stage_dir().join(crate::CHECKPOINTS_DIR).join(id))?
        }
        None => latest_logical_checkpoint(&source)?,
    };
    anyhow::ensure!(
        checkpoint.run_id == source.run_id,
        "checkpoint {} belongs to Run {}, not {}",
        checkpoint.checkpoint_id,
        checkpoint.run_id,
        source.run_id
    );
    anyhow::ensure!(
        !args.workspace.exists(),
        "fork workspace already exists: {}",
        args.workspace.display()
    );
    std::fs::create_dir_all(&args.workspace)?;
    let fork_workspace = args.workspace.canonicalize()?;
    let upper = fork_workspace.join("upper");
    if let Err(error) = restore_logical_checkpoint(&checkpoint, &upper) {
        let _ = std::fs::remove_dir_all(&fork_workspace);
        return Err(error);
    }

    let mut config = RunConfig::default();
    config.run.workspace = Some(fork_workspace);
    let (agent, command) = fork_command(&source.agent, &source.command, args.command);
    config.run.agent = agent;
    config.run.command = command;
    config.overlayfs.mode = OverlayFsMode::Overlay;
    config.overlayfs.target = Some(checkpoint.target.clone());
    config.overlayfs.lower = checkpoint.lower_dirs.iter().skip(1).cloned().collect();
    config.overlayfs.backend = OverlayFsBackend::Directory;
    config.overlayfs.commit = OverlayFsCommit::Manual;
    let run_id = format!("run-{}", uuid::Uuid::new_v4());
    apply_safe_defaults(&mut config, &run_id)?;
    execute_config(
        config,
        run_id,
        true,
        Some(RunLineage {
            parent_run_id: source.run_id,
            checkpoint_id: checkpoint.checkpoint_id,
        }),
    )
    .await
}

fn fork_command(
    source_agent: &str,
    source_command: &[String],
    command: Vec<String>,
) -> (String, Vec<String>) {
    if command.is_empty() {
        return (source_agent.to_owned(), source_command.to_vec());
    }
    let agent = Path::new(&command[0])
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(source_agent)
        .to_owned();
    (agent, command)
}

async fn execute_config(
    config: RunConfig,
    run_id: String,
    safe_profile_requested: bool,
    lineage: Option<RunLineage>,
) -> anyhow::Result<i32> {
    validate(&config)?;

    if config.overlaynet.mode == OverlayNetMode::Proxy {
        eprintln!(
            "pVisor OverlayNet boundary: explicit cooperative proxy; direct sockets remain ambient"
        );
    }

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
    let (sink, event_sink, writer): (
        Arc<dyn TrajectoryEventSink>,
        Arc<dyn crate::EventSink>,
        Option<ChronicleWriter>,
    ) = match config.chronicle.mode {
        ChronicleMode::Off => (
            Arc::new(SeqOnlySink::new()),
            Arc::new(crate::NoopEventSink),
            None,
        ),
        ChronicleMode::Lance => {
            let dir = chronicle_dir.context("pChronicle requires a storage location")?;
            let (sink, event_sink, writer) = chronicle_sink(&dir, &config.run.agent, &run_id);
            (sink, event_sink, Some(writer))
        }
    };

    let executor: Arc<dyn RunExecutor> = match config.run.executor {
        RunExecutorKind::Host => Arc::new(ProcessExecutor),
        RunExecutorKind::Container => Arc::new(ContainerExecutor::new(config.container.clone())?),
        RunExecutorKind::Kvm => Arc::new(KvmExecutor::new(config.kvm.clone())?),
    };
    let mut builder = PVisor::builder()
        .storage(&storage)
        .trajectory_sink(sink)
        .event_sink(event_sink)
        .executors(vec![executor]);
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
    if let Some(lineage) = &lineage {
        spec.metadata
            .insert("pvisor.lineage".into(), serde_json::to_value(lineage)?);
    }
    if safe_profile_requested {
        spec.metadata
            .insert("pvisor.safe".into(), serde_json::Value::Bool(true));
    }

    if safe_profile_requested {
        eprintln!("pVisor safe profile: staged workspace + cooperative network review");
        eprintln!("workspace: {}", storage.display());
        match config.run.executor {
            RunExecutorKind::Host => eprintln!(
                "boundary: host process and direct sockets remain outside non-bypassable enforcement"
            ),
            RunExecutorKind::Container => eprintln!(
                "boundary: OCI container process; direct sockets remain outside proxy enforcement"
            ),
            RunExecutorKind::Kvm => eprintln!(
                "boundary: KVM virtual machine; host Gateway integration is disabled"
            ),
        }
    }
    let handle = pvisor.run(spec).await?;
    let result = handle.wait().await?;
    drop(pvisor);
    if let Some(writer) = writer {
        writer.finish()?;
    }
    let record = RunRecord::read(&storage)
        .with_context(|| format!("load finalized Run record from {}", storage.display()))?;
    let bundle = RunBundle::read(&record.stage_dir()).with_context(|| {
        format!(
            "load finalized Run Bundle from {}",
            record.stage_dir().display()
        )
    })?;
    let bundle_path = RunBundle::path(&record.stage_dir());
    if safe_profile_requested {
        eprintln!("Run Bundle: {}", bundle_path.display());
        eprintln!("Review: pvisor review {}", record.stage_dir().display());
        if bundle.filesystem.is_some() {
            eprintln!(
                "Decide: pvisor apply {} | pvisor drop {}",
                record.stage_dir().display(),
                record.stage_dir().display()
            );
        }
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

fn apply_safe_defaults(config: &mut RunConfig, run_id: &str) -> anyhow::Result<()> {
    let generated_workspace = config.run.workspace.is_none();
    if config.overlayfs.mode == OverlayFsMode::Host {
        config.overlayfs.mode = OverlayFsMode::Overlay;
        if config.overlayfs.target.is_none() {
            config.overlayfs.target = Some(std::env::current_dir()?);
        }
    }
    config.overlayfs.commit = OverlayFsCommit::Manual;
    if generated_workspace {
        let mut workspace = default_safe_workspace(run_id);
        if config
            .overlayfs
            .target
            .as_ref()
            .is_some_and(|target| paths_overlap(target, &workspace))
        {
            workspace = std::env::temp_dir().join("persisting-runs").join(run_id);
        }
        config.run.workspace = Some(workspace);
    }
    if config.overlaynet.mode == OverlayNetMode::Off {
        config.overlaynet.mode = OverlayNetMode::Proxy;
        config.overlaynet.policy = OverlayNetPolicy::Public;
    }
    if config.overlaynet.listen == OverlayNetSettings::default().listen {
        config.overlaynet.listen = free_loopback_address()?;
    }
    if config.gateway.admin_listen == crate::GatewaySettings::default().admin_listen {
        config.gateway.admin_listen = free_loopback_address()?;
    }
    if config.run.agent == "agent" {
        if let Some(program) = config.run.command.first() {
            config.run.agent = Path::new(program)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("agent")
                .to_owned();
        }
    }
    Ok(())
}

fn default_safe_workspace(run_id: &str) -> PathBuf {
    default_run_home().join(run_id)
}

fn free_loopback_address() -> anyhow::Result<String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.to_string())
}

fn apply_cli(config: &mut RunConfig, args: RunArgs) {
    let explicit_executor = args.run.executor;
    if let Some(value) = args.run.workspace {
        config.run.workspace = Some(value);
    }
    if let Some(value) = args.run.agent {
        config.run.agent = value;
    }
    if let Some(value) = explicit_executor {
        config.run.executor = value;
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

    let enables_container = args.container.container_runtime.is_some()
        || args.container.container_image.is_some()
        || args.container.container_pvisor_binary.is_some()
        || args.container.container_platform.is_some()
        || args.container.container_network.is_some()
        || args.container.container_workdir.is_some()
        || args.container.container_user.is_some()
        || args.container.container_read_only_rootfs.is_some()
        || !args.container.container_mount.is_empty();
    if let Some(value) = args.container.container_runtime {
        config.container.runtime = value;
    }
    if let Some(value) = args.container.container_image {
        config.container.image = value;
    }
    if let Some(value) = args.container.container_pvisor_binary {
        config.container.pvisor_binary = Some(value);
    }
    if let Some(value) = args.container.container_platform {
        config.container.platform = Some(value);
    }
    if let Some(value) = args.container.container_network {
        config.container.network = value;
    }
    if let Some(value) = args.container.container_workdir {
        config.container.workdir = Some(value);
    }
    if let Some(value) = args.container.container_user {
        config.container.user = Some(value);
    }
    if let Some(value) = args.container.container_read_only_rootfs {
        config.container.read_only_rootfs = value;
    }
    if !args.container.container_mount.is_empty() {
        config.container.mounts = args
            .container
            .container_mount
            .into_iter()
            .map(|mount| mount.0)
            .collect();
    }
    if enables_container && explicit_executor.is_none() {
        config.run.executor = RunExecutorKind::Container;
    }

    let enables_kvm = args.kvm.kvm_qemu.is_some()
        || args.kvm.kvm_image.is_some()
        || args.kvm.kvm_image_format.is_some()
        || args.kvm.kvm_architecture.is_some()
        || args.kvm.kvm_pvisor_binary.is_some()
        || args.kvm.kvm_memory_mib.is_some()
        || args.kvm.kvm_cpus.is_some()
        || args.kvm.kvm_ssh.is_some()
        || args.kvm.kvm_scp.is_some()
        || args.kvm.kvm_ssh_user.is_some()
        || args.kvm.kvm_ssh_key.is_some()
        || args.kvm.kvm_ssh_port.is_some()
        || args.kvm.kvm_boot_timeout_ms.is_some()
        || args.kvm.kvm_firmware.is_some()
        || args.kvm.kvm_snapshot.is_some();
    if let Some(value) = args.kvm.kvm_qemu {
        config.kvm.qemu = value;
    }
    if let Some(value) = args.kvm.kvm_image {
        config.kvm.image = Some(value);
    }
    if let Some(value) = args.kvm.kvm_image_format {
        config.kvm.image_format = value;
    }
    if let Some(value) = args.kvm.kvm_architecture {
        config.kvm.architecture = value;
    }
    if let Some(value) = args.kvm.kvm_pvisor_binary {
        config.kvm.pvisor_binary = Some(value);
    }
    if let Some(value) = args.kvm.kvm_memory_mib {
        config.kvm.memory_mib = value;
    }
    if let Some(value) = args.kvm.kvm_cpus {
        config.kvm.cpus = value;
    }
    if let Some(value) = args.kvm.kvm_ssh {
        config.kvm.ssh = value;
    }
    if let Some(value) = args.kvm.kvm_scp {
        config.kvm.scp = value;
    }
    if let Some(value) = args.kvm.kvm_ssh_user {
        config.kvm.ssh_user = value;
    }
    if let Some(value) = args.kvm.kvm_ssh_key {
        config.kvm.ssh_key = Some(value);
    }
    if let Some(value) = args.kvm.kvm_ssh_port {
        config.kvm.ssh_port = Some(value);
    }
    if let Some(value) = args.kvm.kvm_boot_timeout_ms {
        config.kvm.boot_timeout_ms = value;
    }
    if let Some(value) = args.kvm.kvm_firmware {
        config.kvm.firmware = Some(value);
    }
    if let Some(value) = args.kvm.kvm_snapshot {
        config.kvm.snapshot = value;
    }
    if enables_kvm && explicit_executor.is_none() {
        config.run.executor = RunExecutorKind::Kvm;
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

    let enables_overlaynet = !args.overlaynet.overlaynet_allow.is_empty()
        || !args.overlaynet.overlaynet_deny.is_empty()
        || !args.overlaynet.overlaynet_limit.is_empty()
        || !args.overlaynet.overlaynet_rule.is_empty()
        || args.overlaynet.overlaynet_deny_all
        || args.overlaynet.overlaynet_listen.is_some();
    if let Some(value) = args.overlaynet.overlaynet_mode {
        config.overlaynet.mode = value;
    }
    if let Some(value) = args.overlaynet.overlaynet_listen {
        config.overlaynet.listen = value;
    }
    if let Some(value) = args.overlaynet.overlaynet_policy {
        config.overlaynet.policy = value;
    }
    if args.overlaynet.overlaynet_deny_all {
        config.overlaynet.policy = OverlayNetPolicy::Deny;
        config.overlaynet.allow.clear();
        config.overlaynet.rules.clear();
        config.overlaynet.deny.clear();
        config.overlaynet.limits.clear();
    }
    if !args.overlaynet.overlaynet_allow.is_empty() {
        config.overlaynet.policy = OverlayNetPolicy::Allowlist;
        config.overlaynet.allow.clear();
        config.overlaynet.rules = args
            .overlaynet
            .overlaynet_allow
            .into_iter()
            .map(|target| target.0)
            .collect();
    }
    if !args.overlaynet.overlaynet_deny.is_empty() {
        config.overlaynet.deny = args
            .overlaynet
            .overlaynet_deny
            .into_iter()
            .map(|target| target.0)
            .collect();
    }
    if !args.overlaynet.overlaynet_limit.is_empty() {
        config.overlaynet.limits = args
            .overlaynet
            .overlaynet_limit
            .into_iter()
            .map(|limit| limit.0)
            .collect();
    }
    if !args.overlaynet.overlaynet_rule.is_empty() {
        config.overlaynet.rules = args
            .overlaynet
            .overlaynet_rule
            .into_iter()
            .map(|rule| rule.0)
            .collect();
    }
    if enables_overlaynet {
        config.overlaynet.mode = OverlayNetMode::Proxy;
    }

    if let Some(value) = args.gateway.gateway_mode {
        config.gateway.mode = value;
        if value == GatewayMode::Capture {
            config.overlaynet.mode = OverlayNetMode::Proxy;
        }
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
    if config.run.executor == RunExecutorKind::Container {
        ContainerExecutor::new(config.container.clone())?;
        if config.overlaynet.mode == OverlayNetMode::Proxy
            && config.container.network != ContainerNetwork::Host
        {
            bail!("the in-process OverlayNet/Gateway requires container.network = \"host\"");
        }
    }
    if config.run.executor == RunExecutorKind::Kvm {
        KvmExecutor::new(config.kvm.clone())?;
        if config.overlaynet.mode == OverlayNetMode::Proxy
            || config.gateway.mode == GatewayMode::Capture
        {
            bail!("KVM executor does not yet expose the host Gateway/OverlayNet endpoint to the guest");
        }
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
            || !config.overlaynet.rules.is_empty()
            || !config.overlaynet.deny.is_empty()
            || !config.overlaynet.limits.is_empty()
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
        && (!config.overlaynet.allow.is_empty() || !config.overlaynet.rules.is_empty())
    {
        bail!("OverlayNet allow entries and rules require --overlaynet-policy allowlist");
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
        rules: config.overlaynet.rules.clone(),
        deny_rules: config.overlaynet.deny.clone(),
        limits: config.overlaynet.limits.clone(),
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
    fn safe_profile_builds_a_reviewable_default_run() {
        let crate::cli::Command::Run(args) = Cli::try_parse_from([
            "pvisor",
            "run",
            "--safe",
            "--workspace",
            "/tmp/pvisor-safe-test",
            "--",
            "/usr/bin/true",
        ])
        .unwrap()
        .command
        else {
            unreachable!()
        };
        assert!(args.safe);
        let mut config = RunConfig::default();
        apply_cli(&mut config, *args);
        apply_safe_defaults(&mut config, "run-safe").unwrap();
        assert_eq!(config.overlayfs.mode, OverlayFsMode::Overlay);
        assert_eq!(config.overlayfs.commit, OverlayFsCommit::Manual);
        assert!(config.overlayfs.target.is_some());
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Proxy);
        assert_eq!(config.overlaynet.policy, OverlayNetPolicy::Public);
        assert_eq!(config.run.agent, "true");
        assert_ne!(
            config.overlaynet.listen,
            OverlayNetSettings::default().listen
        );
    }

    #[test]
    fn fork_inherits_or_reidentifies_the_agent_with_its_command() {
        let source = vec!["/bin/sh".into(), "-c".into(), "work".into()];
        assert_eq!(
            fork_command("sh", &source, Vec::new()),
            ("sh".into(), source)
        );
        assert_eq!(
            fork_command("sh", &[], vec!["/usr/local/bin/codex".into()]),
            ("codex".into(), vec!["/usr/local/bin/codex".into()])
        );
    }

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
            "--overlaynet-rule",
            r#"host="api.openai.com", ports=[443], transports=["tcp_tunnel"]"#,
            "--gateway-mode",
            "capture",
            "--gateway-route",
            r#"name="openai", upstream="https://api.openai.com/v1""#,
            "--chronicle-mode",
            "lance",
            "--chronicle-dir",
            "s3://trajectory-bucket/pvisor-runs",
            "--",
            "codex",
        ])
        .expect("complete command line should parse");
    }

    #[test]
    fn cli_selects_and_configures_container_executor() {
        let crate::cli::Command::Run(args) = Cli::try_parse_from([
            "pvisor",
            "run",
            "--container-runtime",
            "podman",
            "--container-image",
            "example/agent:latest",
            "--container-pvisor-binary",
            "/opt/artifacts/pvisor-linux-amd64",
            "--container-platform",
            "linux/amd64",
            "--container-network",
            "none",
            "--container-read-only-rootfs",
            "--container-mount",
            r#"source="/tmp", target="/workspace", read_only=true"#,
            "--",
            "agent",
        ])
        .unwrap()
        .command
        else {
            unreachable!()
        };
        let mut config = RunConfig::default();
        apply_cli(&mut config, *args);
        assert_eq!(config.run.executor, RunExecutorKind::Container);
        assert_eq!(config.container.runtime, Path::new("podman"));
        assert_eq!(config.container.image, "example/agent:latest");
        assert_eq!(
            config.container.pvisor_binary.as_deref(),
            Some(Path::new("/opt/artifacts/pvisor-linux-amd64"))
        );
        assert_eq!(
            config.container.platform,
            Some(ContainerPlatform::LinuxAmd64)
        );
        assert_eq!(config.container.network, ContainerNetwork::None);
        assert!(config.container.read_only_rootfs);
        assert_eq!(config.container.mounts.len(), 1);
        assert!(config.container.mounts[0].read_only);
        validate(&config).unwrap();
    }

    #[test]
    fn proxy_requires_host_network_for_container_executor() {
        let mut config = RunConfig::default();
        config.run.command = vec!["agent".into()];
        config.run.executor = RunExecutorKind::Container;
        config.container.image = "example/agent:latest".into();
        config.container.network = ContainerNetwork::Bridge;
        config.run.workspace = Some("/tmp/run".into());
        config.overlaynet.mode = OverlayNetMode::Proxy;
        let error = validate(&config).unwrap_err();
        assert!(error.to_string().contains("container.network = \"host\""));
    }

    #[test]
    fn cli_selects_and_configures_kvm_executor() {
        let temporary = tempfile::tempdir().unwrap();
        let image = temporary.path().join("guest.qcow2");
        let key = temporary.path().join("id_ed25519");
        let firmware = temporary.path().join("edk2-aarch64-code.fd");
        std::fs::write(&image, b"image").unwrap();
        std::fs::write(&key, b"key").unwrap();
        std::fs::write(&firmware, b"firmware").unwrap();
        let crate::cli::Command::Run(args) = Cli::try_parse_from([
            "pvisor",
            "run",
            "--kvm-image",
            image.to_str().unwrap(),
            "--kvm-ssh-key",
            key.to_str().unwrap(),
            "--kvm-architecture",
            "aarch64",
            "--kvm-firmware",
            firmware.to_str().unwrap(),
            "--kvm-memory-mib",
            "4096",
            "--kvm-cpus",
            "4",
            "--",
            "agent",
        ])
        .unwrap()
        .command
        else {
            unreachable!()
        };
        let mut config = RunConfig::default();
        apply_cli(&mut config, *args);
        assert_eq!(config.run.executor, RunExecutorKind::Kvm);
        assert_eq!(config.kvm.image.as_deref(), Some(image.as_path()));
        assert_eq!(config.kvm.architecture, KvmArchitecture::Aarch64);
        assert_eq!(config.kvm.memory_mib, 4096);
        assert_eq!(config.kvm.cpus, 4);
        validate(&config).unwrap();
    }

    #[test]
    fn kvm_rejects_host_loopback_gateway_transport() {
        let temporary = tempfile::tempdir().unwrap();
        let image = temporary.path().join("guest.qcow2");
        let key = temporary.path().join("id_ed25519");
        std::fs::write(&image, b"image").unwrap();
        std::fs::write(&key, b"key").unwrap();
        let mut config = RunConfig::default();
        config.run.command = vec!["agent".into()];
        config.run.executor = RunExecutorKind::Kvm;
        config.kvm.image = Some(image);
        config.kvm.ssh_key = Some(key);
        config.gateway.mode = GatewayMode::Capture;
        let error = validate(&config).unwrap_err();
        assert!(error.to_string().contains("does not yet expose"));
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
        assert!(config.overlaynet.allow.is_empty());
        assert_eq!(config.overlaynet.rules.len(), 1);
        assert_eq!(config.overlaynet.rules[0].host, "new.example");
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Proxy);
        assert_eq!(config.overlaynet.policy, OverlayNetPolicy::Allowlist);
    }

    #[test]
    fn simple_network_flags_repeat_and_infer_policy() {
        let crate::cli::Command::Run(args) = Cli::try_parse_from([
            "pvisor",
            "run",
            "--overlaynet-allow",
            "api.example.com:443",
            "--overlaynet-allow",
            "packages.example.com",
            "--overlaynet-deny",
            "169.254.0.0/16",
            "--overlaynet-deny",
            "bad.example.com:80",
            "--overlaynet-limit",
            "10mbps",
            "--overlaynet-limit",
            "api.example.com:443=2mbps",
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

        assert_eq!(config.overlaynet.mode, OverlayNetMode::Proxy);
        assert_eq!(config.overlaynet.policy, OverlayNetPolicy::Allowlist);
        assert_eq!(config.overlaynet.rules.len(), 2);
        assert_eq!(config.overlaynet.rules[0].ports, [443]);
        assert_eq!(config.overlaynet.deny.len(), 2);
        assert_eq!(config.overlaynet.deny[1].ports, [80]);
        assert_eq!(config.overlaynet.limits.len(), 2);
        assert_eq!(config.overlaynet.limits[0].bytes_per_second, 1_250_000);
        assert_eq!(
            config.overlaynet.limits[1].host.as_deref(),
            Some("api.example.com")
        );
        assert_eq!(config.overlaynet.limits[1].bytes_per_second, 250_000);
    }

    #[test]
    fn help_exposes_only_the_simple_network_policy_surface() {
        let error = Cli::try_parse_from(["pvisor", "run", "--help"]).unwrap_err();
        let help = error.to_string();
        assert!(help.contains("--overlaynet-allow"));
        assert!(help.contains("--overlaynet-deny"));
        assert!(help.contains("--overlaynet-limit"));
        assert!(help.contains("--overlaynet-deny-all"));
        assert!(help.contains("direct sockets"));
        assert!(!help.contains("--overlaynet-policy"));
        assert!(!help.contains("--overlaynet-rule"));
    }

    #[test]
    fn deny_all_is_discoverable_and_replaces_configured_policy_details() {
        let crate::cli::Command::Run(args) =
            Cli::try_parse_from(["pvisor", "run", "--overlaynet-deny-all", "--", "true"])
                .unwrap()
                .command
        else {
            unreachable!()
        };
        let mut config = RunConfig::default();
        config.overlaynet.allow = vec!["old.example".into()];
        config.overlaynet.deny = vec![NetworkAccessRule {
            host: "blocked.example".into(),
            ports: Vec::new(),
            transports: Vec::new(),
            allow_private_ips: false,
        }];
        config.overlaynet.limits = vec![NetworkBandwidthLimit {
            host: None,
            port: None,
            bytes_per_second: 1_000,
        }];

        apply_cli(&mut config, *args);

        assert_eq!(config.overlaynet.mode, OverlayNetMode::Proxy);
        assert_eq!(config.overlaynet.policy, OverlayNetPolicy::Deny);
        assert!(config.overlaynet.allow.is_empty());
        assert!(config.overlaynet.rules.is_empty());
        assert!(config.overlaynet.deny.is_empty());
        assert!(config.overlaynet.limits.is_empty());
    }

    #[test]
    fn gateway_capture_enables_overlaynet_without_a_driver_flag() {
        let crate::cli::Command::Run(args) = Cli::try_parse_from([
            "pvisor",
            "run",
            "--gateway-mode",
            "capture",
            "--gateway-route",
            r#"name="openai", upstream="https://api.openai.com/v1""#,
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
        assert_eq!(config.gateway.mode, GatewayMode::Capture);
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Proxy);
    }

    #[test]
    fn simple_target_parser_handles_ports_cidrs_and_ipv6() {
        let domain = parse_overlaynet_target("api.example.com:443").unwrap();
        assert_eq!(domain.host, "api.example.com");
        assert_eq!(domain.ports, [443]);

        let cidr = parse_overlaynet_target("10.0.0.0/8:8080").unwrap();
        assert_eq!(cidr.host, "10.0.0.0/8");
        assert_eq!(cidr.ports, [8080]);

        let ipv6 = parse_overlaynet_target("[::1]:8443").unwrap();
        assert_eq!(ipv6.host, "::1");
        assert_eq!(ipv6.ports, [8443]);
        assert!(parse_overlaynet_target("2001:db8::1")
            .unwrap()
            .ports
            .is_empty());

        for invalid in [
            "",
            "api.example.com:0",
            "api.example.com:65536",
            "api.example.com:",
            "https://api.example.com",
            "[::1",
        ] {
            assert!(
                parse_overlaynet_target(invalid).is_err(),
                "accepted invalid target {invalid:?}"
            );
        }
    }

    #[test]
    fn bandwidth_parser_uses_explicit_bit_and_byte_units() {
        assert_eq!(parse_bandwidth("8bps").unwrap(), 1);
        assert_eq!(parse_bandwidth("10mbps").unwrap(), 1_250_000);
        assert_eq!(parse_bandwidth("2mb/s").unwrap(), 2_000_000);
        assert_eq!(parse_bandwidth("1GB/S").unwrap(), 1_000_000_000);
        for invalid in [
            "",
            "0mbps",
            "10",
            "fast",
            "1tbps",
            "18446744073709551615gbps",
        ] {
            assert!(
                parse_bandwidth(invalid).is_err(),
                "accepted invalid bandwidth {invalid:?}"
            );
        }
    }

    #[test]
    fn cli_structured_rules_replace_config_rules() {
        let mut config = RunConfig::default();
        config.overlaynet.rules = vec![NetworkAccessRule {
            host: "old.example".into(),
            ports: vec![80],
            transports: Vec::new(),
            allow_private_ips: false,
        }];
        let crate::cli::Command::Run(args) = Cli::try_parse_from([
            "pvisor",
            "run",
            "--overlaynet-rule",
            r#"host="new.example", ports=[443], transports=["tcp_tunnel"]"#,
            "--",
            "true",
        ])
        .unwrap()
        .command
        else {
            unreachable!()
        };
        apply_cli(&mut config, *args);
        assert_eq!(config.overlaynet.rules.len(), 1);
        assert_eq!(config.overlaynet.rules[0].host, "new.example");
        assert_eq!(config.overlaynet.rules[0].ports, [443]);
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
