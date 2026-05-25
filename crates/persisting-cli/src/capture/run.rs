//! `persisting capture run` — in-process proxy + execute command.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use persisting_capture::run_env::strip_capture_proxy_env;
use persisting_capture::{
    proxy_environment, snapshot_daemon_env, snapshot_run_proxy_config, write_run_child_info,
    write_run_session, ProxyConfig,
};

use super::debug_setup::{enable_if_requested, CaptureDebugContext};
use super::in_process::InProcessCapture;

pub struct RunOptions {
    pub output_dir: PathBuf,
    pub config: PathBuf,
    pub command: Vec<String>,
    pub debug: bool,
    pub format: super::CaptureFormat,
    pub sink: std::sync::Arc<dyn persisting_capture::CaptureSink>,
}

pub fn cmd_run(opts: RunOptions) -> Result<i32> {
    if opts.command.is_empty() {
        anyhow::bail!(
            "capture run requires a command after `--`, e.g. \
             `persisting capture run -c proxy.yaml -o ./store -- curl …`"
        );
    }

    let storage = opts.output_dir.canonicalize().unwrap_or(opts.output_dir);

    let run_cfg = ProxyConfig::from_yaml_file(&opts.config)
        .with_context(|| format!("load proxy config {}", opts.config.display()))?;

    strip_capture_proxy_env();
    snapshot_daemon_env(&storage, &run_cfg)
        .with_context(|| format!("snapshot daemon env for {}", storage.display()))?;
    let applied = persisting_capture::apply_daemon_env(&storage)?;

    enable_if_requested(
        &CaptureDebugContext {
            storage: &storage,
            applied_env_keys: &applied,
        },
        opts.debug,
    )?;

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .try_init();

    let server = InProcessCapture::start(
        run_cfg.clone(),
        storage.clone(),
        opts.sink.clone(),
        opts.format.stream_markdown_in_engine(),
    )?;

    let root_session = format!("run-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S-%f"));
    let run_started_at = chrono::Utc::now();

    write_run_session(&storage, &root_session)?;
    snapshot_run_proxy_config(&storage, &root_session, &opts.config)?;

    persisting_capture::append_lifecycle(
        opts.sink.as_ref(),
        &persisting_capture::root_session_route(&root_session),
        &run_cfg.agent_id,
        persisting_capture::session_started_record(
            Some(root_session.clone()),
            Some(run_cfg.agent_id.clone()),
            persisting_capture::CaptureMode::Run,
            Some(&server.listen),
            opts.command.first().map(String::as_str),
        ),
    )
    .context("append session.started to trajectory")?;

    if opts.debug {
        log_run_debug(&storage, &root_session);
    }

    let proxy_env = proxy_environment(&server.listen, &root_session);
    let (program, args) = opts
        .command
        .split_first()
        .context("command program after validation")?;
    let gateway_args = persisting_capture::client_gateway_config_args(program, &server.listen);
    let mut child_argv = gateway_args.clone();
    child_argv.extend_from_slice(args);
    let child_display = if gateway_args.is_empty() {
        opts.command.join(" ")
    } else {
        format!("{program} {} {}", gateway_args.join(" "), args.join(" "))
    };
    eprintln!(
        "[persisting-cli] capture run: dir={} format={} session={root_session} proxy=http://{} cmd={child_display}",
        storage.display(),
        opts.format.as_str(),
        server.listen,
    );
    if !gateway_args.is_empty() {
        eprintln!(
            "[persisting-cli] capture run: injected client gateway config for `{program}` \
             (see persisting-capture `client_gateway_config_args`)"
        );
    }

    let code = run_child(
        storage.as_path(),
        program,
        &child_argv,
        &opts.command,
        proxy_env,
    )
    .context("run command")?;

    if opts.debug {
        eprintln!(
            "[persisting-cli] capture run finished (exit {code}); sessions: `persisting capture list`"
        );
    }

    let duration_ms = (chrono::Utc::now() - run_started_at)
        .num_milliseconds()
        .max(0) as u64;
    persisting_capture::append_lifecycle(
        opts.sink.as_ref(),
        &persisting_capture::root_session_route(&root_session),
        &run_cfg.agent_id,
        persisting_capture::session_ended_record(
            Some(root_session.clone()),
            Some(run_cfg.agent_id.clone()),
            persisting_capture::CaptureMode::Run,
            "child_exit",
            Some(code),
            Some(duration_ms),
        ),
    )
    .context("append session.ended to trajectory")?;

    server.shutdown()?;
    Ok(code)
}

fn log_run_debug(storage: &Path, root_session: &str) {
    if let Ok(Some(snap)) = persisting_capture::load_daemon_env_snapshot(storage) {
        eprintln!(
            "[persisting-cli] capture run daemon.env: {} ({} keys)",
            storage
                .join(persisting_capture::DAEMON_ENV_FILENAME)
                .display(),
            snap.vars.len()
        );
    }
    eprintln!(
        "[persisting-cli] capture run config snapshot: {}",
        persisting_capture::session_proxy_config_path(storage, root_session).display()
    );
}

fn run_child(
    storage: &Path,
    program: &str,
    argv: &[String],
    logged_command: &[String],
    proxy_env: std::collections::HashMap<String, String>,
) -> Result<i32> {
    let mut cmd = Command::new(program);
    cmd.args(argv);
    cmd.stdin(Stdio::inherit());
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());

    for (k, v) in std::env::vars_os() {
        cmd.env(k, v);
    }
    for (k, v) in proxy_env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().context("spawn child process")?;
    write_run_child_info(storage, child.id(), logged_command)?;
    let status = child.wait()?;
    Ok(status.code().unwrap_or(1))
}
