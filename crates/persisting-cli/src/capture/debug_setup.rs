//! Shared capture debug setup for `run` / `serve`.

use std::path::Path;

use anyhow::Result;

pub struct CaptureDebugContext<'a> {
    pub storage: &'a Path,
    pub applied_env_keys: &'a [String],
}

/// Write `{storage}/.capture/debug.enabled` and print log path when `--debug` is set.
pub fn enable_if_requested(ctx: &CaptureDebugContext<'_>, requested: bool) -> Result<()> {
    if !requested {
        return Ok(());
    }
    persisting_capture::debug::enable_debug(ctx.storage)?;
    if !ctx.applied_env_keys.is_empty() {
        persisting_capture::debug::log_daemon_env_applied(ctx.storage, ctx.applied_env_keys);
    }
    eprintln!(
        "[persisting-cli] capture debug → {} (mirror stderr: set {}=1)",
        persisting_capture::debug::debug_log_path(ctx.storage).display(),
        persisting_capture::debug::ENV_CAPTURE_DEBUG_STDERR,
    );
    Ok(())
}
