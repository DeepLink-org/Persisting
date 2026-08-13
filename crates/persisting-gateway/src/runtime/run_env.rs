//! Run-scoped capture environment files.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::runtime::private_fs::{ensure_private_dir, write_private_file};

pub const ENV_SESSION_ID: &str = "PERSISTING_CAPTURE_SESSION_ID";
pub use crate::injection::env::CAPTURE_PROXY_ENV_KEYS;

pub fn run_session_file(storage: &Path) -> PathBuf {
    storage.join(".capture").join("run_session")
}

pub fn write_run_session(storage: &Path, session_id: &str) -> Result<()> {
    let path = run_session_file(storage);
    ensure_private_capture_dir(storage)?;
    write_private_file(&path, session_id.trim().as_bytes()).context("write run_session")?;
    Ok(())
}

pub fn read_run_session(storage: &Path) -> Option<String> {
    let path = run_session_file(storage);
    let s = fs::read_to_string(&path).ok()?;
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

pub const RUN_CHILD_FILENAME: &str = "run_child.yaml";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunChildInfo {
    pub pid: u32,
    pub command: String,
}

pub fn run_child_file(storage: &Path) -> PathBuf {
    storage.join(".capture").join(RUN_CHILD_FILENAME)
}

/// Written at `capture run` child spawn — authoritative client command (not the proxy).
pub fn write_run_child_info(storage: &Path, pid: u32, command: &[String]) -> Result<()> {
    let path = run_child_file(storage);
    ensure_private_capture_dir(storage)?;
    let info = RunChildInfo {
        pid,
        command: command.join(" "),
    };
    let yaml = serde_yaml::to_string(&info).context("serialize run_child.yaml")?;
    write_private_file(&path, yaml.as_bytes()).context("write run_child.yaml")
}

pub(crate) fn read_run_child_info(storage: &Path) -> Option<RunChildInfo> {
    let path = run_child_file(storage);
    let text = fs::read_to_string(&path).ok()?;
    serde_yaml::from_str(&text).ok()
}

pub(crate) fn ensure_private_capture_dir(storage: &Path) -> Result<PathBuf> {
    let capture_dir = storage.join(".capture");
    ensure_private_dir(&capture_dir)?;
    Ok(capture_dir)
}

pub use crate::injection::env::{
    capture_openai_v1_base, client_gateway_config_args, proxy_environment,
};
