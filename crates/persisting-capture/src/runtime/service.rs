//! Capture daemon state (pid, listen addresses).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureDaemonState {
    pub pid: u32,
    pub storage: String,
    pub config_path: String,
    pub listen: String,
    pub admin_listen: String,
    pub started_at: String,
}

impl CaptureDaemonState {
    pub fn state_path(storage: &Path) -> PathBuf {
        storage.join(".capture").join("daemon.json")
    }

    pub fn write(&self, storage: &Path) -> Result<()> {
        let path = Self::state_path(storage);
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn read(storage: &Path) -> Result<Option<Self>> {
        let path = Self::state_path(storage);
        if !path.exists() {
            return Ok(None);
        }
        let s = fs::read_to_string(&path)?;
        Ok(Some(serde_json::from_str(&s)?))
    }

    pub fn remove(storage: &Path) -> Result<()> {
        let path = Self::state_path(storage);
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        #[cfg(unix)]
        {
            use std::process::Command;
            Command::new("kill")
                .args(["-0", &self.pid.to_string()])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            let _ = self;
            false
        }
    }
}

/// `~/.persisting/capture/current.json` — last successful `traj proxy start`.
pub fn global_registry_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("home directory")?;
    Ok(home
        .join(".persisting")
        .join("capture")
        .join("current.json"))
}

pub fn write_current(state: &CaptureDaemonState) -> Result<()> {
    let path = global_registry_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

pub(crate) fn read_current() -> Result<Option<CaptureDaemonState>> {
    let path = global_registry_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let s = fs::read_to_string(&path)?;
    Ok(Some(serde_json::from_str(&s)?))
}

pub(crate) fn clear_current() -> Result<()> {
    let path = global_registry_path()?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

/// Resolve trajectory root for `list` / `status` / `stop`.
///
/// Priority: CLI argument → **first running capture from process list** →
/// `PERSISTING_CAPTURE_STORAGE` → `~/.persisting/capture/current.json`.
pub fn resolve_storage_detailed(
    explicit: Option<&Path>,
) -> Result<crate::discover_daemon::StorageResolution> {
    use crate::discover_daemon::{discover_running_captures, StorageSource};

    let running = discover_running_captures().unwrap_or_default();

    if let Some(p) = explicit {
        return Ok(crate::discover_daemon::StorageResolution {
            storage: p.to_path_buf(),
            source: StorageSource::Cli,
            running,
        });
    }

    if let Some(first) = running.first() {
        return Ok(crate::discover_daemon::StorageResolution {
            storage: PathBuf::from(&first.storage),
            source: StorageSource::ProcessList,
            running,
        });
    }

    if let Ok(s) = std::env::var("PERSISTING_CAPTURE_STORAGE") {
        let s = s.trim();
        if !s.is_empty() {
            return Ok(crate::discover_daemon::StorageResolution {
                storage: PathBuf::from(s),
                source: StorageSource::Env,
                running,
            });
        }
    }

    if let Some(state) = read_current()? {
        return Ok(crate::discover_daemon::StorageResolution {
            storage: PathBuf::from(state.storage),
            source: StorageSource::CurrentRegistry,
            running,
        });
    }

    anyhow::bail!(
        "no capture instance found: start one with `persisting traj proxy start -o <DIR> -c <config.toml>`, \
         or pass `-o <DIR>`, or set PERSISTING_CAPTURE_STORAGE"
    )
}

pub fn stop_daemon(storage: &Path) -> Result<()> {
    let Some(state) = CaptureDaemonState::read(storage)? else {
        anyhow::bail!(
            "capture is not running (no daemon.json under {})",
            storage.display()
        );
    };
    #[cfg(unix)]
    {
        use std::process::Command;
        let status = Command::new("kill")
            .arg(state.pid.to_string())
            .status()
            .context("kill capture daemon")?;
        if !status.success() {
            anyhow::bail!("failed to stop pid {}", state.pid);
        }
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("capture stop is only supported on Unix");
    }
    CaptureDaemonState::remove(storage)?;
    let _ = clear_current();
    Ok(())
}
