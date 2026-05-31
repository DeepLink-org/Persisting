//! `persisting traj capture` — in-process proxy + execute command.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use persisting_capture::config::ProxyConfig;
use persisting_capture::frontmatter::{format_run_summary_line, refresh_run_markdown_frontmatter};
use persisting_capture::injection::{client_gateway_config_args, proxy_environment};
use persisting_capture::lifecycle::{
    append_lifecycle, root_session_route, session_ended_record, session_started_record, CaptureMode,
};
use persisting_capture::runtime::run_config::{
    session_proxy_config_path, snapshot_run_proxy_config,
};
use persisting_capture::runtime::run_env::{
    apply_daemon_env, load_daemon_env_snapshot, snapshot_daemon_env, strip_capture_proxy_env,
    write_run_child_info, write_run_session, DAEMON_ENV_FILENAME,
};
use persisting_capture::sink::CaptureSink;

use super::debug_setup::{enable_if_requested, CaptureDebugContext};
use super::in_process::InProcessCapture;

pub struct RunOptions {
    pub output_dir: PathBuf,
    pub config: PathBuf,
    pub command: Vec<String>,
    pub debug: bool,
    pub format: super::CaptureFormat,
    pub sink: std::sync::Arc<dyn CaptureSink>,
}

pub fn cmd_run(opts: RunOptions) -> Result<i32> {
    if opts.command.is_empty() {
        anyhow::bail!(
            "traj capture requires a command after `--`, e.g. \
             `persisting traj capture -c proxy.toml -o ./store -- curl …`"
        );
    }

    let storage = opts.output_dir.canonicalize().unwrap_or(opts.output_dir);

    let run_cfg = ProxyConfig::from_file(&opts.config)
        .with_context(|| format!("load proxy config {}", opts.config.display()))?;

    strip_capture_proxy_env();
    snapshot_daemon_env(&storage, &run_cfg)
        .with_context(|| format!("snapshot daemon env for {}", storage.display()))?;
    let applied = apply_daemon_env(&storage)?;

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

    append_lifecycle(
        opts.sink.as_ref(),
        &root_session_route(&root_session),
        &run_cfg.agent_id,
        session_started_record(
            Some(root_session.clone()),
            Some(run_cfg.agent_id.clone()),
            CaptureMode::Run,
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
    let gateway_args = client_gateway_config_args(program, &server.listen);
    let mut child_argv = gateway_args.clone();
    child_argv.extend_from_slice(args);
    let child_display = if gateway_args.is_empty() {
        opts.command.join(" ")
    } else {
        format!("{program} {} {}", gateway_args.join(" "), args.join(" "))
    };
    eprintln!(
        "[persisting-cli] traj capture: dir={} format={} session={root_session} proxy=http://{} cmd={child_display}",
        storage.display(),
        opts.format.as_str(),
        server.listen,
    );
    if !gateway_args.is_empty() {
        eprintln!(
            "[persisting-cli] traj capture: injected client gateway config for `{program}` \
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
            "[persisting-cli] traj capture finished (exit {code}); sessions: `persisting traj proxy list`"
        );
    }

    let duration_ms = (chrono::Utc::now() - run_started_at)
        .num_milliseconds()
        .max(0) as u64;
    append_lifecycle(
        opts.sink.as_ref(),
        &root_session_route(&root_session),
        &run_cfg.agent_id,
        session_ended_record(
            Some(root_session.clone()),
            Some(run_cfg.agent_id.clone()),
            CaptureMode::Run,
            "child_exit",
            Some(code),
            Some(duration_ms),
        ),
    )
    .context("append session.ended to trajectory")?;

    server.shutdown()?;
    if opts.format.stream_markdown_in_engine() {
        print_run_markdown_summary(&storage, &run_cfg.agent_id, &root_session);
    }
    eprintln!(
        "[persisting-cli] traj capture done (exit {code}) — inspect: \
         `persisting traj stats {} --detail` · sessions: `persisting traj proxy list -o {}`",
        storage.display(),
        storage.display(),
    );

    Ok(code)
}

fn print_run_markdown_summary(storage: &Path, agent_id: &str, root_session: &str) {
    match refresh_run_markdown_frontmatter(storage, agent_id, root_session, None) {
        Ok(entries) if entries.is_empty() => {}
        Ok(entries) => {
            let main = entries.iter().find(|(p, _)| {
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s == root_session)
            });
            if let Some((path, summary)) = main {
                eprintln!(
                    "[persisting-cli] {}",
                    format_run_summary_line(path, summary)
                );
            }
            for (path, summary) in &entries {
                if summary.subagents.is_empty() && summary.turns == 0 {
                    continue;
                }
                if main.is_some_and(|(p, _)| p == path) {
                    continue;
                }
                eprintln!(
                    "[persisting-cli]   {}",
                    format_run_summary_line(path, summary)
                );
            }
        }
        Err(e) => {
            eprintln!("[persisting-cli] traj capture frontmatter refresh: {e:#}");
        }
    }
}

fn log_run_debug(storage: &Path, root_session: &str) {
    if let Ok(Some(snap)) = load_daemon_env_snapshot(storage) {
        eprintln!(
            "[persisting-cli] traj capture daemon.env: {} ({} keys)",
            storage.join(DAEMON_ENV_FILENAME).display(),
            snap.vars.len()
        );
    }
    eprintln!(
        "[persisting-cli] traj capture config snapshot: {}",
        session_proxy_config_path(storage, root_session).display()
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
