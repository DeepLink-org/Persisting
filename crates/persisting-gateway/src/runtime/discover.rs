//! Discover running `persisting traj proxy` processes.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

use super::service::CaptureDaemonState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageSource {
    Cli,
    ProcessList,
    Env,
    CurrentRegistry,
}

impl StorageSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::ProcessList => "process",
            Self::Env => "env",
            Self::CurrentRegistry => "registry",
        }
    }
}

#[derive(Debug, Clone)]
pub struct StorageResolution {
    pub storage: PathBuf,
    pub source: StorageSource,
    /// All running capture daemons (sorted by pid); `storage` is the selected one.
    pub running: Vec<CaptureDaemonState>,
}

/// Running capture daemons, sorted by ascending pid (first = default target).
pub(crate) fn discover_running_captures() -> Result<Vec<CaptureDaemonState>> {
    let mut seen = Vec::new();
    for storage in discover_storage_paths_from_processes()? {
        let storage = storage.canonicalize().unwrap_or(storage);
        if seen.iter().any(|p: &PathBuf| p == &storage) {
            continue;
        }
        seen.push(storage);
    }

    let mut running = Vec::new();
    for storage in seen {
        if let Some(state) = CaptureDaemonState::read(&storage)? {
            if state.is_running() {
                running.push(state);
            }
        }
    }
    running.sort_by_key(|s| s.pid);
    Ok(running)
}

/// Parse storage from `traj proxy -o <DIR> …` (foreground proxy).
pub fn storage_from_proxy_cmdline(cmdline: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = cmdline.split_whitespace().collect();
    let proxy_idx = parts.iter().position(|&t| t == "proxy")?;
    // `traj proxy start` is the short-lived wrapper; the child is `traj proxy -o …`.
    if parts.get(proxy_idx + 1).is_some_and(|&t| t == "start") {
        return None;
    }
    let tail = &parts[proxy_idx + 1..];
    for (j, &next) in tail.iter().enumerate() {
        if matches!(next, "-o" | "--output-dir" | "--output_dir") {
            if let Some(&path) = tail.get(j + 1) {
                return Some(PathBuf::from(path));
            }
        }
    }
    for &next in tail {
        if next.starts_with('-') {
            continue;
        }
        if next == "traj" || next == "trajectory" || next.ends_with("persisting") {
            continue;
        }
        return Some(PathBuf::from(next));
    }
    None
}

#[cfg(unix)]
fn discover_storage_paths_from_processes() -> Result<Vec<PathBuf>> {
    let output = Command::new("ps")
        .args(["ax", "-o", "args="])
        .output()
        .context("ps ax -o args=")?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut paths = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.contains("persisting") || !line.contains("proxy") {
            continue;
        }
        if line.contains("proxy start") {
            continue;
        }
        if let Some(p) = storage_from_proxy_cmdline(line) {
            paths.push(p);
        }
    }
    Ok(paths)
}

#[cfg(not(unix))]
fn discover_storage_paths_from_processes() -> Result<Vec<PathBuf>> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_proxy_storage_with_output_flag() {
        let p = storage_from_proxy_cmdline(
            "/usr/bin/persisting traj proxy -o ./trajectory-store -c /tmp/c.yaml -f md",
        )
        .unwrap();
        assert_eq!(p, PathBuf::from("./trajectory-store"));
    }

    #[test]
    fn parse_proxy_skips_start_wrapper() {
        assert!(
            storage_from_proxy_cmdline("persisting traj proxy start -o ./store -c cfg.toml")
                .is_none()
        );
    }

    #[test]
    fn parse_proxy_storage_positional() {
        let p = storage_from_proxy_cmdline(
            "persisting trajectory proxy ./data/store --config cfg.yaml",
        )
        .unwrap();
        assert_eq!(p, PathBuf::from("./data/store"));
    }
}
