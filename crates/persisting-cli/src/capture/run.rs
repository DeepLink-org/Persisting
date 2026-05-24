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

    let server = InProcessCapture::start(run_cfg.clone(), storage.clone(), opts.sink)?;

    let root_session = format!("run-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S-%f"));

    write_run_session(&storage, &root_session)?;
    snapshot_run_proxy_config(&storage, &root_session, &opts.config)?;
    if opts.debug {
        log_run_debug(&storage, &root_session);
    }

    let proxy_env = proxy_environment(&server.listen, &root_session);
    eprintln!(
        "[persisting-cli] capture run: dir={} format={} session={root_session} proxy=http://{} cmd={}",
        storage.display(),
        opts.format.as_str(),
        server.listen,
        opts.command.join(" ")
    );

    let code = run_child(&storage, &opts.command, proxy_env).context("run command")?;

    if opts.debug {
        eprintln!(
            "[persisting-cli] capture run finished (exit {code}); sessions: `persisting capture list`"
        );
    }

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
    command: &[String],
    proxy_env: std::collections::HashMap<String, String>,
) -> Result<i32> {
    let (program, args) = command.split_first().context("command program")?;
    let mut cmd = Command::new(program);
    cmd.args(args);
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
    write_run_child_info(storage, child.id(), command)?;
    let status = child.wait()?;
    Ok(status.code().unwrap_or(1))
}
