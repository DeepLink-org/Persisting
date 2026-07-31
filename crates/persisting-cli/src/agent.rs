//! `persisting agent` — OpenShell-like agent entry with single and batch Runs.
//!
//! - `execute`  — one controlled Agent Run (OpenShell-style) via pVisor
//! - `bexecute` — many Runs (batch); pPilot stays an internal library

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use persisting_proto::{PolicyMode, RunInvocation, RunSpec, RunState, StdioMode};
use persisting_pvisor::{OverlayHint, PVisor};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Args)]
#[command(
    about = "OpenShell-like agent entry: execute one Run or bexecute many",
    long_about = "Product surface analogous to NVIDIA OpenShell: launch agents under\n\
capture / network / filesystem controls (from the capture TOML).\n\n\
`execute`  — one Run via pVisor (`PVisor::run`; starts capture when `-c` is set)\n\
`bexecute` — batch of Runs (internally uses pPilot; not exposed as its own CLI verb)"
)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub command: AgentCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum AgentCommand {
    /// Run one Agent under pVisor controls (OpenShell single-session analogue).
    Execute(ExecuteArgs),
    /// Run many Agents / tasks as a batch (OpenShell + batch; pPilot internal).
    Bexecute(BexecuteArgs),
}

#[derive(Debug, Clone, Args)]
pub struct ExecuteArgs {
    /// Logical agent name recorded on the RunSpec (overridden by config `agent_id` when `-c` is set).
    #[arg(long, default_value = "agent")]
    pub agent: String,
    /// Optional run id (default: generated UUID).
    #[arg(long)]
    pub run_id: Option<String>,
    /// Wall-clock deadline for the Attempt (milliseconds).
    #[arg(long)]
    pub timeout_ms: Option<u64>,
    /// Capture proxy URL injected as HTTPS_PROXY when not using `-c` (legacy).
    #[arg(long, env = "PERSISTING_CAPTURE_HTTPS_PROXY")]
    pub https_proxy: Option<String>,
    /// Capture proxy URL injected as HTTP_PROXY when not using `-c` (legacy).
    #[arg(long, env = "PERSISTING_CAPTURE_HTTP_PROXY")]
    pub http_proxy: Option<String>,
    /// Capture config TOML — starts in-process capture and applies `[network]` / `[overlay]`.
    #[arg(long, short = 'c', value_name = "FILE")]
    pub capture_config: Option<PathBuf>,
    /// Capture storage directory (default: `.persisting/capture`).
    #[arg(
        long,
        short = 'o',
        value_name = "DIR",
        env = "PERSISTING_CAPTURE_STORAGE",
        default_value = ".persisting/capture"
    )]
    pub output_dir: String,
    /// Stream dialogue Markdown while capturing (when `-c` is set).
    #[arg(long, default_value_t = true)]
    pub stream_markdown: bool,
    /// Working directory for the child (overrides overlay merged root when set).
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    /// Optional overlay merged mount point (combined with `[overlay]` in the capture TOML).
    #[arg(long, value_name = "DIR")]
    pub overlay_merged: Option<PathBuf>,
    /// Overlay target filesystem (RO base + apply destination). Enables staging overlay.
    #[arg(long, value_name = "DIR")]
    pub overlay_target: Option<PathBuf>,
    /// Enforce capability policy (network via capture proxy when `-c` is set).
    #[arg(long)]
    pub enforce: bool,
    /// Capture child stdout/stderr into the RunResult.
    #[arg(long)]
    pub capture_stdio: bool,
    /// Program and args after `--`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Args)]
#[command(
    about = "Batch-execute many Runs (pPilot is internal)",
    long_about = "Batch entry for many independent Runs under the same agent/runtime\n\
contracts as `execute`. Orchestration is implemented by the internal pPilot\n\
library — there is no `persisting ppilot` command."
)]
pub struct BexecuteArgs {
    /// Batch plan script (`plan()` / `execute(item)`), or a manifest path (future).
    #[arg(value_name = "PLAN")]
    pub plan: Option<PathBuf>,
    /// Durable sink directory for results / checkpoints.
    #[arg(long, value_name = "DIR")]
    pub sink: Option<PathBuf>,
    /// Parallelism hint (forwarded to internal orchestrator when wired).
    #[arg(short = 'w', long, default_value_t = 4)]
    pub workers: usize,
    /// Resume from `--sink` when supported.
    #[arg(long)]
    pub resume: bool,
    /// Extra args after `--` passed through to the plan.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub plan_args: Vec<String>,
}

pub async fn run_agent(args: AgentArgs) -> Result<i32> {
    match args.command {
        AgentCommand::Execute(exec) => run_execute(exec).await,
        AgentCommand::Bexecute(batch) => run_bexecute(batch).await,
    }
}

async fn run_bexecute(args: BexecuteArgs) -> Result<i32> {
    let plan = args
        .plan
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<missing PLAN>".into());
    bail!(
        "`persisting agent bexecute` is the product batch entry (OpenShell + batch). \
         Internal engine: persisting-ppilot (not a CLI verb). \
         Not wired yet — planned: plan={}, workers={}, sink={:?}, resume={}, args={:?}",
        plan,
        args.workers,
        args.sink,
        args.resume,
        args.plan_args,
    );
}

async fn run_execute(args: ExecuteArgs) -> Result<i32> {
    let (program, program_args) = args
        .command
        .split_first()
        .context("execute requires a program after `--`")?;

    let run_id = args
        .run_id
        .unwrap_or_else(|| format!("run-{}", Uuid::new_v4()));

    let mut builder = PVisor::builder();
    if let Some(url) = &args.https_proxy {
        builder = builder.capture_https_proxy(url);
    }
    if let Some(url) = &args.http_proxy {
        builder = builder.capture_http_proxy(url);
    }
    if let Some(path) = &args.capture_config {
        builder = builder
            .capture_config(path)
            .capture_output_dir(&args.output_dir)
            .stream_markdown(args.stream_markdown);
    } else if args.output_dir != ".persisting/capture" {
        builder = builder.capture_output_dir(&args.output_dir);
    }
    if args.overlay_merged.is_some() || args.overlay_target.is_some() {
        builder = builder.overlay(OverlayHint {
            merged_dir: args.overlay_merged.clone(),
            lower_dirs: args.overlay_target.clone().into_iter().collect(),
            ..OverlayHint::default()
        });
    }

    let pvisor = builder.build();

    let agent_name = if let Some(path) = &args.capture_config {
        persisting_capture::config::ProxyConfig::from_file(path)
            .map(|c| c.agent_id)
            .unwrap_or_else(|_| args.agent.clone())
    } else {
        args.agent.clone()
    };

    let mut spec = RunSpec::process(run_id, agent_name, program.to_string());
    {
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = program_args.iter().map(|s| s.to_string()).collect();
        if let Some(cwd) = &args.cwd {
            process.cwd = Some(cwd.display().to_string());
        }
        if args.capture_stdio {
            process.stdout = StdioMode::Capture;
            process.stderr = StdioMode::Capture;
        } else {
            process.stdout = StdioMode::Inherit;
            process.stderr = StdioMode::Inherit;
            process.stdin = StdioMode::Inherit;
        }
    }
    spec.runtime.timeout_ms = args.timeout_ms;
    if args.enforce {
        spec.runtime.policy_mode = PolicyMode::Enforce;
    }

    let handle = pvisor.run(spec).await.context("pVisor run")?;
    eprintln!(
        "[persisting agent execute] run_id={} attempt_id={}",
        handle.run_id(),
        handle.attempt_id()
    );

    let result = handle.wait().await.context("wait for Run")?;
    if args.capture_stdio {
        if let Some(stdout) = &result.output.stdout {
            print!("{stdout}");
        }
        if let Some(stderr) = &result.output.stderr {
            eprint!("{stderr}");
        }
    }
    for warning in &result.warnings {
        eprintln!("[persisting agent execute] warning: {warning}");
    }

    match result.state {
        RunState::Completed => Ok(result.exit_code.unwrap_or(0)),
        RunState::Cancelled => {
            eprintln!("[persisting agent execute] cancelled");
            Ok(130)
        }
        other => {
            if let Some(failure) = &result.failure {
                eprintln!(
                    "[persisting agent execute] {:?}: {}",
                    other, failure.message
                );
            } else {
                eprintln!("[persisting agent execute] ended in state {other:?}");
            }
            Ok(result.exit_code.unwrap_or(1))
        }
    }
}
