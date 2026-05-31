//! `traj proxy start` / `stop` / `list` / `status`.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use persisting_capture::config::ProxyConfig;
use persisting_capture::runtime::discover::{StorageResolution, StorageSource};
use persisting_capture::runtime::run_env::{load_daemon_env_snapshot, snapshot_daemon_env};
use persisting_capture::runtime::service::{
    resolve_storage_detailed, stop_daemon, write_current, CaptureDaemonState,
};
use persisting_capture::session_index::{discover_sessions, SessionSummary};

use super::usage;

pub struct StartOptions {
    pub output_dir: PathBuf,
    pub config: PathBuf,
    pub debug: bool,
    pub format: super::CaptureFormat,
}

pub fn cmd_start(opts: StartOptions) -> Result<()> {
    if let Some(state) = CaptureDaemonState::read(&opts.output_dir)? {
        if state.is_running() {
            anyhow::bail!(
                "traj proxy already running (pid {}) for {}",
                state.pid,
                opts.output_dir.display()
            );
        }
        CaptureDaemonState::remove(&opts.output_dir)?;
    }

    let exe = std::env::current_exe().context("current_exe")?;
    let config = ProxyConfig::from_file(&opts.config)
        .with_context(|| format!("load proxy config {}", opts.config.display()))?;
    let env_snap = snapshot_daemon_env(&opts.output_dir, &config)
        .with_context(|| format!("snapshot daemon env for {}", opts.output_dir.display()))?;
    eprintln!(
        "[persisting-cli] traj proxy daemon env snapshot: {} ({} keys)",
        env_snap.display(),
        load_daemon_env_snapshot(&opts.output_dir)
            .ok()
            .flatten()
            .map(|s| s.vars.len())
            .unwrap_or(0)
    );

    let mut cmd = Command::new(&exe);
    let args = vec![
        "traj".to_string(),
        "proxy".to_string(),
        "-o".to_string(),
        opts.output_dir.to_string_lossy().to_string(),
        "-c".to_string(),
        opts.config.to_string_lossy().to_string(),
        "-f".to_string(),
        opts.format.as_str().to_string(),
    ];
    cmd.args(args);
    if opts.debug {
        cmd.env(persisting_capture::debug::ENV_CAPTURE_DEBUG, "1");
    }
    let stderr = if opts.debug {
        let log_path = opts.output_dir.join(".capture").join("daemon.log");
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map(Stdio::from)
            .unwrap_or(Stdio::null())
    } else {
        Stdio::null()
    };
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .context("spawn traj proxy")?;

    let state = CaptureDaemonState {
        pid: child.id(),
        storage: opts.output_dir.display().to_string(),
        config_path: opts.config.display().to_string(),
        listen: config.listen.clone(),
        admin_listen: config.admin_listen.clone(),
        started_at: chrono::Utc::now().to_rfc3339(),
    };
    state.write(&opts.output_dir)?;
    write_current(&state)?;
    if opts.debug {
        eprintln!(
            "[persisting-cli] traj proxy debug enabled (daemon env {}=1)",
            persisting_capture::debug::ENV_CAPTURE_DEBUG
        );
    }
    usage::eprint_serve_banner(&usage::ServeBanner {
        listen: &state.listen,
        admin_listen: &state.admin_listen,
        output_dir: &opts.output_dir,
        agent_id: &config.agent_id,
        format: opts.format,
        background: true,
        pid: Some(state.pid),
    });
    Ok(())
}

pub(super) fn log_storage_resolution(res: &StorageResolution) {
    eprintln!(
        "[persisting-cli] capture target: {} (via {}, pid {} running)",
        res.storage.display(),
        res.source.as_str(),
        res.running.len()
    );
    if res.running.len() > 1 && res.source == StorageSource::ProcessList {
        eprintln!(
            "[persisting-cli] capture: multiple instances; using pid {} — specify -o <DIR> to override",
            res.running[0].pid
        );
        for s in res.running.iter().skip(1) {
            eprintln!(
                "  - pid {} storage {} proxy=http://{}",
                s.pid, s.storage, s.listen
            );
        }
    }
}

pub fn cmd_stop(storage: Option<&Path>) -> Result<()> {
    let res = resolve_storage_detailed(storage)?;
    log_storage_resolution(&res);
    stop_daemon(&res.storage)?;
    eprintln!(
        "[persisting-cli] traj proxy stopped ({})",
        res.storage.display()
    );
    Ok(())
}

pub fn cmd_list(storage: Option<&Path>) -> Result<Vec<SessionSummary>> {
    let res = resolve_storage_detailed(storage)?;
    log_storage_resolution(&res);
    discover_sessions(&res.storage).context("discover sessions")
}

pub fn cmd_status(storage: Option<&Path>) -> Result<()> {
    let res = resolve_storage_detailed(storage)?;
    log_storage_resolution(&res);
    let storage = &res.storage;
    let state = CaptureDaemonState::read(storage)?
        .ok_or_else(|| anyhow::anyhow!("capture not running for {}", storage.display()))?;
    if !state.is_running() {
        anyhow::bail!("stale daemon.json: pid {} not running", state.pid);
    }
    let url = format!("http://{}/admin/status", state.admin_listen);
    let resp = reqwest::blocking::Client::builder()
        .build()?
        .get(&url)
        .send()
        .with_context(|| format!("GET {url}"))?;
    let text = resp.text()?;
    println!("{text}");
    Ok(())
}

pub fn print_list_table(sessions: &[SessionSummary]) {
    if sessions.is_empty() {
        println!("No capture sessions found.");
        return;
    }
    println!(
        "{:<20} {:<36} {:<10} {:>8} {:>10} {:>10} {:>10} {:>6}",
        "agent_id", "session_id", "active", "requests", "in_tok", "out_tok", "cost_usd", "model"
    );
    for s in sessions {
        let model = if s.model.len() > 12 {
            format!("{}…", &s.model[..12])
        } else {
            s.model.clone()
        };
        println!(
            "{:<20} {:<36} {:<10} {:>8} {:>10} {:>10} {:>10.4} {:>6}",
            trunc(&s.agent_id, 20),
            trunc(&s.session_id, 36),
            if s.active { "yes" } else { "no" },
            s.request_count,
            s.usage.input_tokens,
            s.usage.output_tokens,
            s.estimated_cost_usd,
            model,
        );
    }
}

fn trunc(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}
