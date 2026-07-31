//! `persisting traj capture` — thin wrapper over pVisor Attempt (capture + network + overlay).

use std::path::PathBuf;

use anyhow::{Context, Result};
use persisting_capture::config::ProxyConfig;
use persisting_capture::sink::CaptureSink;
use persisting_proto::{RunInvocation, RunSpec, RunState, StdioMode};
use persisting_pvisor::{OverlayHint, PVisor};
use std::sync::Arc;

use super::CaptureFormat;

pub struct RunOptions {
    pub output_dir: PathBuf,
    pub config: PathBuf,
    pub command: Vec<String>,
    pub debug: bool,
    pub format: CaptureFormat,
    pub sink: Arc<dyn CaptureSink>,
}

pub fn cmd_run(opts: RunOptions) -> Result<i32> {
    if opts.command.is_empty() {
        anyhow::bail!(
            "traj capture requires a command after `--`, e.g. \
             `persisting traj capture -c proxy.toml -o ./store -- curl …`"
        );
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("tokio runtime for traj capture")?;
    rt.block_on(cmd_run_async(opts))
}

async fn cmd_run_async(opts: RunOptions) -> Result<i32> {
    let storage = opts.output_dir.canonicalize().unwrap_or(opts.output_dir);
    let run_cfg = ProxyConfig::from_file(&opts.config)
        .with_context(|| format!("load proxy config {}", opts.config.display()))?;

    if opts.debug {
        eprintln!(
            "[persisting-cli] traj capture via pVisor: dir={} format={} config={}",
            storage.display(),
            opts.format.as_str(),
            opts.config.display(),
        );
    }

    let (program, program_args) = opts
        .command
        .split_first()
        .context("command program after validation")?;

    let pvisor = PVisor::builder()
        .capture_config(&opts.config)
        .capture_output_dir(&storage)
        .capture_sink(opts.sink)
        .stream_markdown(opts.format.stream_markdown_in_engine())
        .overlay(OverlayHint::default())
        .build();
    let run_id = format!("run-{}", uuid::Uuid::new_v4());
    let mut spec = RunSpec::process(run_id, run_cfg.agent_id.clone(), program.to_string());
    {
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = program_args.iter().map(|s| s.to_string()).collect();
        process.stdout = StdioMode::Inherit;
        process.stderr = StdioMode::Inherit;
        process.stdin = StdioMode::Inherit;
    }

    let handle = pvisor.run(spec).await.context("pVisor run")?;
    eprintln!(
        "[persisting-cli] traj capture: session via pVisor run_id={} proxy config listen={}",
        handle.run_id(),
        run_cfg.listen,
    );

    let result = handle.wait().await.context("wait for Run")?;
    let code = match result.state {
        RunState::Completed => result.exit_code.unwrap_or(0),
        RunState::Cancelled => 130,
        _ => result.exit_code.unwrap_or(1),
    };

    eprintln!(
        "[persisting-cli] traj capture done (exit {code}) — inspect: \
         `persisting traj stats {} --detail` · sessions: `persisting traj proxy list -o {}`",
        storage.display(),
        storage.display(),
    );
    Ok(code)
}
