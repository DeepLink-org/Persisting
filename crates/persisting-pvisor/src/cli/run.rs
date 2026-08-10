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
    OverlayFsBackend, OverlayFsCommit, OverlayFsSettings, OverlayNetMode, OverlayNetPolicy,
    OverlayNetSettings, RunConfig, RunExecutorKind, RunPolicy, RunStdio,
};
use crate::runtime::{default_run_home, resolve_run, RunLineage};
use crate::{
    latest_logical_checkpoint, restore_logical_checkpoint, ContainerExecutor, GatewayDriverConfig,
    KvmExecutor, LogicalCheckpoint, OverlayHint, PVisor, ProcessExecutor, RunBundle, RunExecutor,
    TrajectoryEventSink,
};

use super::trajectory::{chronicle_sink, ChronicleWriter};

#[cfg(target_os = "linux")]
pub(super) const RUN_COMMAND_ABOUT: &str =
    "Execute one Agent Run; --safe selects the rootless Linux sandbox";
#[cfg(target_os = "linux")]
pub(super) const RUN_COMMAND_LONG_ABOUT: &str = "Execute one Agent Run under pVisor management.\n\nFor a local executable, `--safe` stages workspace writes and automatically enforces the Linux rootless boundary. No root daemon, setuid helper, container image, or sandbox policy file is required. The command fails closed if a required namespace, mount, chroot, or Landlock control cannot be installed.";

#[cfg(target_os = "macos")]
pub(super) const RUN_COMMAND_ABOUT: &str =
    "Execute one Agent Run; --safe selects the macOS Seatbelt sandbox";
#[cfg(target_os = "macos")]
pub(super) const RUN_COMMAND_LONG_ABOUT: &str = "Execute one Agent Run under pVisor management.\n\nFor a local executable, `--safe` stages workspace writes through macFUSE and installs a fail-closed macOS Seatbelt policy before Agent code starts. Writes are limited to the staged workspace, explicit read-write capabilities, and a Run-owned temporary directory. Reads remain ambient for toolchain compatibility.";

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) const RUN_COMMAND_ABOUT: &str = "Execute one Agent Run under pVisor management";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) const RUN_COMMAND_LONG_ABOUT: &str = RUN_COMMAND_ABOUT;

#[cfg(target_os = "linux")]
const SAFE_HELP: &str =
    "Stage writes and run a host executable in the fail-closed Linux rootless sandbox";
#[cfg(target_os = "linux")]
const SAFE_LONG_HELP: &str = "Stage workspace writes for review and, with `--executor host`, enforce a rootless Linux boundary using user and mount namespaces, a minimal synthetic root with chroot, Landlock ABI v3, no_new_privs, closed inherited file descriptors, and an empty capability set. Public or allowlisted networking remains cooperative; `--overlaynet-deny-all` adds a private network namespace.";

#[cfg(target_os = "macos")]
const SAFE_HELP: &str =
    "Stage writes and run a local executable in the fail-closed macOS Seatbelt sandbox";
#[cfg(target_os = "macos")]
const SAFE_LONG_HELP: &str = "Stage workspace writes through macFUSE and enforce write confinement with macOS Seatbelt. The generated policy admits only the staged workspace, explicit read-write capabilities, device handles, and a Run-owned temporary directory. Full-disk reads remain ambient. `--overlaynet-deny-all` additionally blocks IP and ambient host Unix sockets while retaining Run-local IPC. Missing or rejected Seatbelt controls fail before Agent execution.";

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const SAFE_HELP: &str = "Stage workspace writes for review before apply or drop";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const SAFE_LONG_HELP: &str = SAFE_HELP;

#[cfg(target_os = "linux")]
const EXECUTOR_HELP: &str = "Execution provider. `host` plus `--safe` automatically selects the rootless Linux sandbox; `host` without `--safe` is an ordinary host process";
#[cfg(target_os = "macos")]
const EXECUTOR_HELP: &str = "Execution provider. `host` plus `--safe` selects macFUSE staging with macOS Seatbelt write confinement";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const EXECUTOR_HELP: &str = "Execution provider for the Agent command";

#[cfg(target_os = "linux")]
const DENY_ALL_HELP: &str = "Deny all proxy egress. With `--safe --executor host`, also create a private network namespace so direct sockets cannot bypass the denial";
#[cfg(target_os = "macos")]
const DENY_ALL_HELP: &str = "Deny all proxy egress. With `--safe --executor host`, Seatbelt also blocks IP and ambient host Unix sockets while retaining Run-local IPC";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const DENY_ALL_HELP: &str =
    "Deny all forward-proxy egress; direct sockets remain outside this cooperative rule";

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

    #[arg(long, help = SAFE_HELP, long_help = SAFE_LONG_HELP)]
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
    /// Reusable project workspace for the child Run; defaults to the source workspace.
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
    #[arg(long, short = 'o', default_value = ".persisting/capture")]
    output_dir: PathBuf,
    /// Agent command; defaults to the source Run command.
    #[arg(last = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

#[derive(Debug, Clone, Default, Args)]
struct RunOverrides {
    /// Reusable project workspace. Defaults to the current directory.
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
    #[arg(long)]
    agent: Option<String>,
    #[arg(long, value_enum, help = EXECUTOR_HELP)]
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
    /// Directory containing libkrun, libkrun_init, and libkrunfw.
    #[arg(long, value_name = "PATH")]
    kvm_library_dir: Option<PathBuf>,
    #[arg(long, value_name = "MIB")]
    kvm_memory_mib: Option<u32>,
    #[arg(long, value_name = "COUNT")]
    kvm_cpus: Option<u16>,
    #[arg(long, value_name = "PORT")]
    kvm_proxy_vsock_port: Option<u32>,
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
    /// Bottom read-only layer and default apply destination. Defaults to the Run workspace.
    #[arg(long, value_name = "DIR")]
    overlayfs_base: Option<PathBuf>,
    /// Read-only layer composed above the base; repeat to add multiple layers.
    #[arg(long, value_name = "DIR")]
    overlayfs_compose: Vec<PathBuf>,
    /// Durable writable stage root. Defaults to the generated per-Run directory.
    #[arg(long, value_name = "DIR")]
    overlayfs_stage: Option<PathBuf>,
    #[arg(long, value_enum)]
    overlayfs_backend: Option<OverlayFsBackend>,
    #[arg(long, value_enum)]
    overlayfs_commit: Option<OverlayFsCommit>,
}

#[derive(Debug, Clone, Default, Args)]
struct OverlayNetOverrides {
    #[arg(long, value_enum, hide = true)]
    overlaynet_mode: Option<OverlayNetMode>,
    /// Explicit proxy listen address; supplying it enables OverlayNet.
    #[arg(long, value_name = "ADDR")]
    overlaynet_listen: Option<String>,
    #[arg(long, value_enum, hide = true)]
    overlaynet_policy: Option<OverlayNetPolicy>,
    /// Allowed HOST[:PORT] or CIDR[:PORT]; enables the cooperative proxy.
    #[arg(long, value_name = "TARGET")]
    overlaynet_allow: Vec<OverlayNetTargetArg>,
    /// Denied HOST[:PORT] or CIDR[:PORT]; enables the cooperative proxy.
    #[arg(long, value_name = "TARGET")]
    overlaynet_deny: Vec<OverlayNetTargetArg>,
    /// Aggregate bandwidth limit; enables the cooperative proxy.
    #[arg(long, value_name = "[TARGET=]RATE")]
    overlaynet_limit: Vec<OverlayNetLimitArg>,
    #[arg(
        long,
        help = DENY_ALL_HELP,
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
        apply_safe_defaults(&mut config)?;
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
        .executors(vec![Arc::new(ProcessExecutor::default())])
        .build();
    let handle = pvisor.run(spec).await?;
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
    let output = crate::delegated::DelegatedRunOutput { result };
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
    let fork_workspace = args
        .workspace
        .or_else(|| source.workspace.clone())
        .unwrap_or_else(|| checkpoint.target.clone());
    let fork_workspace = resolve_workspace(&fork_workspace)?;
    let run_id = format!("run-{}", uuid::Uuid::new_v4());
    let mut config = RunConfig::default();
    if source.executor.as_ref().is_some_and(|executor| {
        executor.isolation == persisting_control::IsolationKind::VirtualMachine
    }) {
        config.run.executor = RunExecutorKind::Kvm;
    }
    config.run.workspace = Some(fork_workspace.clone());
    let (agent, command) = fork_command(&source.agent, &source.command, args.command);
    config.run.agent = agent;
    config.run.command = command;
    config.overlayfs = Some(OverlayFsSettings {
        base: Some(checkpoint.target.clone()),
        compose: checkpoint
            .lower_dirs
            .iter()
            .filter(|lower| *lower != &checkpoint.target)
            .cloned()
            .collect(),
        stage: None,
        backend: OverlayFsBackend::Directory,
        commit: OverlayFsCommit::Manual,
    });
    let stage = select_run_storage(&config, &fork_workspace, &run_id)?;
    std::fs::create_dir_all(&stage)?;
    let upper = stage.join("upper");
    if let Err(error) = restore_logical_checkpoint(&checkpoint, &upper) {
        let _ = std::fs::remove_dir_all(&stage);
        return Err(error);
    }
    config.overlayfs.as_mut().expect("configured above").stage = Some(stage);
    apply_safe_defaults(&mut config)?;
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
    mut config: RunConfig,
    run_id: String,
    safe_profile_requested: bool,
    lineage: Option<RunLineage>,
) -> anyhow::Result<i32> {
    if config.run.executor == RunExecutorKind::Kvm {
        let overlay = config
            .overlayfs
            .get_or_insert_with(OverlayFsSettings::default);
        overlay.base = Some(PathBuf::from("/"));
        overlay.commit = OverlayFsCommit::Manual;
    }
    validate(&config)?;

    if config.overlaynet.mode == OverlayNetMode::Proxy {
        if config.run.executor == RunExecutorKind::Kvm {
            eprintln!(
                "pVisor OverlayNet boundary: guest direct sockets are disabled; proxy traffic crosses an explicit vsock relay"
            );
        } else {
            eprintln!(
                "pVisor OverlayNet boundary: explicit cooperative proxy; direct sockets remain ambient"
            );
        }
    }

    let workspace = config
        .run
        .workspace
        .as_deref()
        .map(Path::to_path_buf)
        .unwrap_or(std::env::current_dir()?);
    let workspace = resolve_workspace(&workspace)?;
    let storage = resolve_run_storage(&select_run_storage(&config, &workspace, &run_id)?)?;
    let overlay = resolve_overlay(&config, &workspace, &storage, &run_id)?;
    let overlay_enabled = overlay.is_some();
    let proxy = resolve_proxy(&config)?;

    if config.gateway.debug {
        persisting_gateway::runtime::debug::enable_debug(&storage)?;
    }

    let chronicle_dir = config
        .chronicle
        .dir
        .clone()
        .or_else(|| Some(storage.join("chronicle")));
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
            let (sink, event_sink, writer) = chronicle_sink(&dir, &config.run.agent, &run_id)?;
            (sink, event_sink, Some(writer))
        }
    };

    let executor: Arc<dyn RunExecutor> = match config.run.executor {
        #[cfg(target_os = "linux")]
        RunExecutorKind::Host if safe_profile_requested => Arc::new(
            ProcessExecutor::rootless_with_launcher(std::env::current_exe()?)
                .context("initialize rootless local process executor")?,
        ),
        #[cfg(target_os = "macos")]
        RunExecutorKind::Host if safe_profile_requested => Arc::new(
            ProcessExecutor::seatbelt_with_launcher(std::env::current_exe()?)
                .context("initialize macOS Seatbelt process executor")?,
        ),
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        RunExecutorKind::Host if safe_profile_requested => Arc::new(ProcessExecutor::default()),
        RunExecutorKind::Host => Arc::new(ProcessExecutor::default()),
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
    if !overlay_enabled {
        process.cwd = Some(workspace.display().to_string());
    }
    spec.runtime.timeout_ms = config.run.timeout_ms;
    spec.metadata.insert(
        "pvisor.workspace".into(),
        serde_json::Value::String(workspace.display().to_string()),
    );
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
        let network_boundary = if cfg!(any(target_os = "linux", target_os = "macos"))
            && config.run.executor == RunExecutorKind::Host
            && config.overlaynet.policy == OverlayNetPolicy::Deny
        {
            if cfg!(target_os = "linux") {
                "private deny-all network namespace"
            } else {
                "Seatbelt deny-all socket policy"
            }
        } else {
            "cooperative network review"
        };
        eprintln!("pVisor safe profile: staged workspace + {network_boundary}");
        eprintln!("workspace: {}", workspace.display());
        eprintln!("Run storage: {}", storage.display());
        match config.run.executor {
            RunExecutorKind::Host => {
                #[cfg(target_os = "linux")]
                eprintln!(
                    "boundary: rootless namespace + synthetic root + Landlock filesystem; network remains cooperative unless explicitly denied"
                );
                #[cfg(target_os = "macos")]
                eprintln!(
                    "boundary: Seatbelt-enforced staged writes; reads and selective network policies remain ambient/cooperative"
                );
                #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                eprintln!("boundary: review-only host process");
            }
            RunExecutorKind::Container => eprintln!(
                "boundary: OCI container process; direct sockets remain outside proxy enforcement"
            ),
            RunExecutorKind::Kvm => {
                eprintln!("boundary: KVM virtual machine; host Gateway integration is disabled")
            }
        }
    }
    let handle = pvisor.run(spec).await?;
    let result = handle.wait().await?;
    drop(pvisor);
    if let Some(writer) = writer {
        writer.finish()?;
    }
    let record = resolve_run(Some(Path::new(&run_id)), &storage)
        .with_context(|| format!("load finalized Run record for {run_id}"))?;
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

fn apply_safe_defaults(config: &mut RunConfig) -> anyhow::Result<()> {
    config
        .overlayfs
        .get_or_insert_with(OverlayFsSettings::default)
        .commit = OverlayFsCommit::Manual;
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

    let enables_kvm = args.kvm.kvm_library_dir.is_some()
        || args.kvm.kvm_memory_mib.is_some()
        || args.kvm.kvm_cpus.is_some()
        || args.kvm.kvm_proxy_vsock_port.is_some();
    if let Some(value) = args.kvm.kvm_library_dir {
        config.kvm.library_dir = Some(value);
    }
    if let Some(value) = args.kvm.kvm_memory_mib {
        config.kvm.memory_mib = value;
    }
    if let Some(value) = args.kvm.kvm_cpus {
        config.kvm.cpus = value;
    }
    if let Some(value) = args.kvm.kvm_proxy_vsock_port {
        config.kvm.proxy_vsock_port = value;
    }
    if enables_kvm && explicit_executor.is_none() {
        config.run.executor = RunExecutorKind::Kvm;
    }

    let enables_overlayfs = args.overlayfs.overlayfs_base.is_some()
        || !args.overlayfs.overlayfs_compose.is_empty()
        || args.overlayfs.overlayfs_stage.is_some()
        || args.overlayfs.overlayfs_backend.is_some()
        || args.overlayfs.overlayfs_commit.is_some();
    if enables_overlayfs {
        let overlayfs = config
            .overlayfs
            .get_or_insert_with(OverlayFsSettings::default);
        if let Some(value) = args.overlayfs.overlayfs_base {
            overlayfs.base = Some(value);
        }
        if !args.overlayfs.overlayfs_compose.is_empty() {
            overlayfs.compose = args.overlayfs.overlayfs_compose;
        }
        if let Some(value) = args.overlayfs.overlayfs_stage {
            overlayfs.stage = Some(value);
        }
        if let Some(value) = args.overlayfs.overlayfs_backend {
            overlayfs.backend = value;
        }
        if let Some(value) = args.overlayfs.overlayfs_commit {
            overlayfs.commit = value;
        }
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
        anyhow::ensure!(
            config
                .overlayfs
                .as_ref()
                .and_then(|overlay| overlay.base.as_deref())
                == Some(Path::new("/")),
            "libkrun KVM execution requires the host root as OverlayFS base"
        );
    }
    if let Some(overlayfs) = &config.overlayfs {
        if overlayfs.commit == OverlayFsCommit::Apply && !overlayfs.compose.is_empty() {
            bail!(
                "--overlayfs-commit apply cannot be combined with --overlayfs-compose until composed layers can be materialized safely"
            );
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
    let workspace = workspace
        .canonicalize()
        .with_context(|| format!("resolve pVisor workspace {}", workspace.display()))?;
    anyhow::ensure!(
        workspace.is_dir(),
        "pVisor workspace must be a directory: {}",
        workspace.display()
    );
    Ok(workspace)
}

fn resolve_run_storage(storage: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(storage)
        .with_context(|| format!("create pVisor Run storage {}", storage.display()))?;
    storage
        .canonicalize()
        .with_context(|| format!("resolve pVisor Run storage {}", storage.display()))
}

fn select_run_storage(
    config: &RunConfig,
    workspace: &Path,
    run_id: &str,
) -> anyhow::Result<PathBuf> {
    let run_home = default_run_home();
    let run_home = if run_home.is_absolute() {
        run_home
    } else {
        std::env::current_dir()?.join(run_home)
    };
    let preferred = run_home.join(run_id);
    let Some(overlayfs) = &config.overlayfs else {
        return Ok(preferred);
    };
    let mut read_only_layers = Vec::with_capacity(overlayfs.compose.len() + 1);
    for layer in &overlayfs.compose {
        read_only_layers.push(resolve_directory(layer, "OverlayFS compose layer")?);
    }
    read_only_layers.push(resolve_directory(
        overlayfs.base.as_deref().unwrap_or(workspace),
        "OverlayFS base",
    )?);
    if read_only_layers
        .iter()
        .any(|layer| paths_overlap(layer, &preferred))
    {
        Ok(std::env::temp_dir().join("persisting-runs").join(run_id))
    } else {
        Ok(preferred)
    }
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
    workspace: &Path,
    storage: &Path,
    run_id: &str,
) -> anyhow::Result<Option<OverlayHint>> {
    let Some(overlayfs) = &config.overlayfs else {
        return Ok(None);
    };
    let base = resolve_directory(
        overlayfs.base.as_deref().unwrap_or(workspace),
        "OverlayFS base",
    )?;
    let stage = overlayfs
        .stage
        .clone()
        .unwrap_or_else(|| storage.to_path_buf());
    let stage = if stage.exists() {
        stage
            .canonicalize()
            .with_context(|| format!("resolve OverlayFS stage {}", stage.display()))?
    } else {
        std::fs::create_dir_all(&stage)
            .with_context(|| format!("create OverlayFS stage {}", stage.display()))?;
        stage
            .canonicalize()
            .with_context(|| format!("resolve OverlayFS stage {}", stage.display()))?
    };
    anyhow::ensure!(
        base == Path::new("/") || !paths_overlap(&base, &stage),
        "OverlayFS base and stage must not overlap: base={}, stage={}",
        base.display(),
        stage.display()
    );
    let mut compose = Vec::with_capacity(overlayfs.compose.len());
    for layer in &overlayfs.compose {
        let layer = resolve_directory(layer, "OverlayFS compose layer")?;
        anyhow::ensure!(
            base == Path::new("/") || !paths_overlap(&layer, &stage),
            "OverlayFS compose layer and stage must not overlap: compose={}, stage={}",
            layer.display(),
            stage.display()
        );
        compose.push(layer);
    }
    compose.push(base);
    Ok(Some(OverlayHint {
        lower_dirs: compose,
        stage_dir: Some(stage.clone()),
        backend: match overlayfs.backend {
            OverlayFsBackend::Directory => OverlayBackend::Directory,
            OverlayFsBackend::Jujutsu => OverlayBackend::Jujutsu,
        },
        jujutsu_store_path: (overlayfs.backend == OverlayFsBackend::Jujutsu)
            .then(|| stage.join("jujutsu")),
        jujutsu_workspace: (overlayfs.backend == OverlayFsBackend::Jujutsu)
            .then(|| run_id.to_owned()),
        auto_apply: overlayfs.commit == OverlayFsCommit::Apply,
        auto_discard: overlayfs.commit == OverlayFsCommit::Drop,
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
        apply_safe_defaults(&mut config).unwrap();
        let overlayfs = config.overlayfs.as_ref().expect("safe enables OverlayFS");
        assert_eq!(overlayfs.commit, OverlayFsCommit::Manual);
        assert!(overlayfs.base.is_none());
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
            "--overlayfs-base",
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
        let libraries = temporary.path().join("lib");
        std::fs::create_dir(&libraries).unwrap();
        let crate::cli::Command::Run(args) = Cli::try_parse_from([
            "pvisor",
            "run",
            "--kvm-library-dir",
            libraries.to_str().unwrap(),
            "--kvm-memory-mib",
            "4096",
            "--kvm-cpus",
            "4",
            "--kvm-proxy-vsock-port",
            "19100",
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
        assert_eq!(config.kvm.library_dir.as_deref(), Some(libraries.as_path()));
        assert_eq!(config.kvm.memory_mib, 4096);
        assert_eq!(config.kvm.cpus, 4);
        assert_eq!(config.kvm.proxy_vsock_port, 19100);
    }

    #[test]
    fn kvm_accepts_overlaynet_transport_over_vsock() {
        let temporary = tempfile::tempdir().unwrap();
        let mut config = RunConfig::default();
        config.run.command = vec!["agent".into()];
        config.run.executor = RunExecutorKind::Kvm;
        config.kvm.library_dir = Some(temporary.path().to_path_buf());
        config.overlayfs = Some(OverlayFsSettings {
            base: Some("/".into()),
            ..OverlayFsSettings::default()
        });
        config.overlaynet.mode = OverlayNetMode::Proxy;
        validate(&config).unwrap();
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
        assert!(config.run.workspace.is_none());
        validate(&config).unwrap();
    }

    #[test]
    fn help_exposes_only_the_simple_network_policy_surface() {
        let error = Cli::try_parse_from(["pvisor", "run", "--help"]).unwrap_err();
        let help = error.to_string();
        assert!(help.contains("--overlaynet-allow"));
        assert!(help.contains("--overlaynet-deny"));
        assert!(help.contains("--overlaynet-limit"));
        assert!(help.contains("--overlaynet-deny-all"));
        #[cfg(target_os = "linux")]
        {
            assert!(help.to_ascii_lowercase().contains("direct sockets"));
            assert!(help.contains("private network namespace"));
        }
        #[cfg(target_os = "macos")]
        assert!(help.contains("ambient host Unix sockets"));
        assert!(!help.contains("requires --workspace"));
        assert!(!help.contains("--overlaynet-policy"));
        assert!(!help.contains("--overlaynet-rule"));
    }

    #[test]
    fn safe_help_describes_the_effective_platform_boundary() {
        let help = Cli::try_parse_from(["pvisor", "run", "--help"])
            .unwrap_err()
            .to_string();

        #[cfg(target_os = "linux")]
        {
            assert!(help.contains("rootless Linux sandbox"));
            assert!(help.contains("synthetic root"));
            assert!(help.contains("Landlock ABI v3"));
            assert!(help.contains("fails closed"));
        }
        #[cfg(target_os = "macos")]
        {
            assert!(help.contains("macFUSE"));
            assert!(help.contains("Seatbelt"));
            assert!(help.contains("Full-disk reads remain ambient"));
            assert!(help.contains("fail before Agent execution"));
        }
    }

    #[test]
    fn help_exposes_compositional_overlayfs_without_a_mode_switch() {
        let error = Cli::try_parse_from(["pvisor", "run", "--help"]).unwrap_err();
        let help = error.to_string();
        for option in [
            "--overlayfs-base",
            "--overlayfs-compose",
            "--overlayfs-stage",
            "--overlayfs-backend",
            "--overlayfs-commit",
        ] {
            assert!(help.contains(option), "missing {option}");
        }
        for obsolete in [
            "--overlayfs-mode",
            "--overlayfs-target",
            "--overlayfs-lower",
        ] {
            assert!(!help.contains(obsolete), "obsolete option {obsolete}");
        }
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
    fn overlayfs_options_enable_the_driver_and_select_a_stage() {
        let crate::cli::Command::Run(args) = Cli::try_parse_from([
            "pvisor",
            "run",
            "--overlayfs-stage",
            "/tmp/pvisor-stage",
            "--overlayfs-backend",
            "jujutsu",
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
        let overlayfs = config.overlayfs.expect("OverlayFS should be enabled");
        assert_eq!(overlayfs.backend, OverlayFsBackend::Jujutsu);
        assert_eq!(
            overlayfs.stage.as_deref(),
            Some(Path::new("/tmp/pvisor-stage"))
        );
    }

    #[test]
    fn overlayfs_defaults_base_to_workspace_and_stage_to_run_storage() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let compose = temporary.path().join("compose");
        let storage = temporary.path().join("run");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&compose).unwrap();
        std::fs::create_dir_all(&storage).unwrap();
        let config = RunConfig {
            overlayfs: Some(OverlayFsSettings {
                compose: vec![compose.clone()],
                ..OverlayFsSettings::default()
            }),
            ..RunConfig::default()
        };

        let hint = resolve_overlay(&config, &workspace, &storage, "run-test")
            .unwrap()
            .unwrap();
        assert_eq!(
            hint.lower_dirs,
            [
                compose.canonicalize().unwrap(),
                workspace.canonicalize().unwrap()
            ]
        );
        assert_eq!(
            hint.stage_dir.as_deref(),
            Some(storage.canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn overlayfs_rejects_stage_nested_inside_base_or_compose() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let compose = temporary.path().join("compose");
        let storage = temporary.path().join("run");
        std::fs::create_dir_all(workspace.join("stage")).unwrap();
        std::fs::create_dir_all(compose.join("stage")).unwrap();
        std::fs::create_dir_all(&storage).unwrap();

        for (base, layers, stage) in [
            (workspace.clone(), Vec::new(), workspace.join("stage")),
            (
                workspace.clone(),
                vec![compose.clone()],
                compose.join("stage"),
            ),
        ] {
            let config = RunConfig {
                overlayfs: Some(OverlayFsSettings {
                    base: Some(base),
                    compose: layers,
                    stage: Some(stage),
                    ..OverlayFsSettings::default()
                }),
                ..RunConfig::default()
            };
            assert!(resolve_overlay(&config, &workspace, &storage, "run-test").is_err());
        }
    }

    #[test]
    fn composed_layers_cannot_be_auto_applied() {
        let config = RunConfig {
            run: crate::config::RunSettings {
                command: vec!["true".into()],
                ..crate::config::RunSettings::default()
            },
            overlayfs: Some(OverlayFsSettings {
                compose: vec!["/tmp/layer".into()],
                commit: OverlayFsCommit::Apply,
                ..OverlayFsSettings::default()
            }),
            ..RunConfig::default()
        };
        assert!(validate(&config)
            .unwrap_err()
            .to_string()
            .contains("cannot be combined"));
    }

    #[test]
    fn compose_replaces_configured_layers_and_enables_overlayfs() {
        let crate::cli::Command::Run(args) = Cli::try_parse_from([
            "pvisor",
            "run",
            "--overlayfs-compose",
            "/tmp/first",
            "--overlayfs-compose",
            "/tmp/second",
            "--",
            "true",
        ])
        .unwrap()
        .command
        else {
            unreachable!()
        };
        let mut config = RunConfig {
            overlayfs: Some(OverlayFsSettings {
                compose: vec!["/tmp/old".into()],
                ..OverlayFsSettings::default()
            }),
            ..RunConfig::default()
        };
        apply_cli(&mut config, *args);
        assert_eq!(
            config.overlayfs.unwrap().compose,
            [PathBuf::from("/tmp/first"), PathBuf::from("/tmp/second")]
        );
    }
}
