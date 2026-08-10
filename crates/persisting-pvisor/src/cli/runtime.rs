use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};
use clap::Args;

use crate::runtime::{
    apply_overlay, control_mount_inspect, control_overlay_status, control_ping,
    control_unmount_inspect, discard_overlay, is_live, mount_overlay_record_read_only,
    overlay_status, resolve_run, OverlayState, ReadOnlyOverlayMount, RunRecord,
};

const DEFAULT_STORAGE: &str = ".persisting/capture";

#[derive(Debug, Clone, Args)]
pub struct StatusArgs {
    /// Run id, stage directory, upper directory, or workspace path.
    pub selector: Option<PathBuf>,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct InspectArgs {
    /// Run id, stage directory, upper directory, or workspace path.
    pub selector: Option<PathBuf>,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
    /// Command to run in the read-only view; defaults to $SHELL or /bin/bash.
    #[arg(last = true, allow_hyphen_values = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub struct SelectArgs {
    /// Run id, stage directory, upper directory, or workspace path.
    pub selector: Option<PathBuf>,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct ApplyArgs {
    /// Run id, stage directory, upper directory, or workspace path.
    pub selector: Option<PathBuf>,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
    /// Apply staged changes here instead of the target recorded by the Run.
    #[arg(long, value_name = "PATH")]
    pub target: Option<PathBuf>,
}

pub fn status(args: StatusArgs) -> anyhow::Result<()> {
    let record = selected(args.selector.as_deref(), &args.output_dir)?;
    let live = control_ping(&record.stage_dir()) || is_live(&record.stage_dir())?;
    let fs = record
        .overlay
        .as_ref()
        .map(|overlay| {
            if control_ping(&record.stage_dir()) {
                control_overlay_status(&record.stage_dir()).map(|status| FsSummary {
                    changed_files: status.changed_files,
                    whiteouts: status.whiteouts,
                    sample_paths: status.sample_paths,
                })
            } else {
                overlay_status(overlay)
                    .map(|status| FsSummary {
                        changed_files: status.changed_files,
                        whiteouts: status.whiteouts,
                        sample_paths: status.sample_paths,
                    })
                    .map_err(Into::into)
            }
        })
        .transpose()?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "run": record,
                "live": live,
                "filesystem": fs.as_ref().map(|status| serde_json::json!({
                    "state": record.overlay.as_ref().map(|overlay| overlay.state),
                    "changed_files": status.changed_files,
                    "whiteouts": status.whiteouts,
                    "sample_paths": status.sample_paths,
                })),
            }))?
        );
        return Ok(());
    }

    println!("run: {}", record.run_id);
    println!("session: {}", record.session_id);
    let state = if live {
        "running"
    } else if record.state == "running" {
        "stale"
    } else {
        &record.state
    };
    println!("state: {state}");
    println!(
        "pid: {}{}",
        record.pid,
        if live { " (live)" } else { " (offline)" }
    );
    println!("agent: {}", record.agent);
    println!("command: {}", shell_join(&record.command));
    println!("stage: {}", record.stage_dir().display());
    println!("net: {}", serde_json::to_string(&record.network)?);
    println!(
        "overlaynet: {}",
        record.overlaynet_listen.as_deref().unwrap_or("disabled")
    );
    if let Some(interception) = &record.network_interception {
        println!(
            "network interception: {:?} ({:?}, enforcing={})",
            interception.driver,
            interception.strength,
            interception.is_enforcing()
        );
    }
    println!(
        "gateway: {}",
        record.gateway_listen.as_deref().unwrap_or("disabled")
    );
    if let Some(overlay) = &record.overlay {
        let fs = fs.context("OverlayFS status missing")?;
        println!("fs: {:?} (read-only inspect available)", overlay.state);
        println!("target: {}", overlay.target.display());
        println!("upper: {}", overlay.upper.path().display());
        println!(
            "changes: {} paths, {} whiteouts",
            fs.changed_files, fs.whiteouts
        );
    } else {
        println!("fs: host view (no OverlayFS workspace)");
    }
    Ok(())
}

struct FsSummary {
    changed_files: usize,
    whiteouts: usize,
    sample_paths: Vec<String>,
}

pub fn inspect(args: InspectArgs) -> anyhow::Result<i32> {
    let record = selected(args.selector.as_deref(), &args.output_dir)?;
    let overlay = record
        .overlay
        .as_ref()
        .context("this Run has no OverlayFS workspace to inspect")?;
    let lowers = if record.overlay_lowers.is_empty() {
        vec![overlay.target.clone()]
    } else {
        record.overlay_lowers.clone()
    };
    let stage = record.stage_dir();
    let mount = if control_ping(&stage) {
        let (id, mountpoint) = control_mount_inspect(&stage)?;
        InspectMount::Remote {
            stage,
            id,
            mountpoint,
        }
    } else {
        let inspect_root = overlay.stage_dir.join("inspect").join(format!(
            "{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mountpoint = inspect_root.join("merged");
        let session = mount_overlay_record_read_only(overlay, &lowers, &mountpoint)
            .with_context(|| format!("mount read-only Run view at {}", mountpoint.display()))?;
        InspectMount::Local {
            inspect_root,
            session,
        }
    };

    let command = if args.command.is_empty() {
        vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())]
    } else {
        args.command
    };
    let (program, command_args) = command.split_first().context("missing inspect command")?;
    let status = Command::new(program)
        .args(command_args)
        .current_dir(mount.mountpoint())
        .env("PERSISTING_INSPECT", "1")
        .env("PERSISTING_RUN_ID", &record.run_id)
        .env("PERSISTING_OVERLAY_STAGE", &overlay.stage_dir)
        .status()
        .with_context(|| format!("execute inspect command `{program}`"));
    mount.close()?;
    let status = status?;
    Ok(status.code().unwrap_or(1))
}

enum InspectMount {
    Remote {
        stage: PathBuf,
        id: String,
        mountpoint: PathBuf,
    },
    Local {
        inspect_root: PathBuf,
        session: ReadOnlyOverlayMount,
    },
}

impl InspectMount {
    fn mountpoint(&self) -> &Path {
        match self {
            Self::Remote { mountpoint, .. } => mountpoint,
            Self::Local { session, .. } => session.mountpoint(),
        }
    }

    fn close(self) -> anyhow::Result<()> {
        match self {
            Self::Remote { stage, id, .. } => control_unmount_inspect(&stage, id),
            Self::Local {
                inspect_root,
                session,
            } => {
                session.unmount()?;
                let _ = std::fs::remove_dir(inspect_root);
                Ok(())
            }
        }
    }
}

pub fn apply(args: ApplyArgs) -> anyhow::Result<()> {
    let select = SelectArgs {
        selector: args.selector,
        output_dir: args.output_dir,
    };
    mutate(select, true, args.target.as_deref())
}

pub fn drop_overlay(args: SelectArgs) -> anyhow::Result<()> {
    mutate(args, false, None)
}

fn mutate(args: SelectArgs, apply: bool, target: Option<&Path>) -> anyhow::Result<()> {
    let mut record = selected(args.selector.as_deref(), &args.output_dir)?;
    if is_live(&record.stage_dir())? {
        bail!(
            "Run {} is still running; its upper cannot be {}",
            record.run_id,
            if apply { "applied" } else { "dropped" }
        );
    }
    let mut overlay = record
        .overlay
        .take()
        .context("this Run has no OverlayFS workspace")?;
    if apply
        && record
            .overlay_lowers
            .iter()
            .any(|lower| lower != &overlay.target)
    {
        bail!(
            "Run {} composes read-only layers above its base; apply is disabled until pVisor can materialize the complete merged diff",
            record.run_id
        );
    }
    if apply && overlay.state == OverlayState::Applied {
        println!(
            "already applied {} → {}",
            record.run_id,
            overlay.target.display()
        );
        return Ok(());
    }
    if !apply && overlay.state == OverlayState::Discarded {
        println!("already dropped {} (target untouched)", record.run_id);
        return Ok(());
    }
    if apply {
        if overlay.target == Path::new("/") {
            bail!(
                "Run {} is a full-root libkrun changeset; checkpoint/fork it or drop it instead of applying it to the host root",
                record.run_id
            );
        }
        if let Some(target) = target {
            let target = resolve_apply_target(target, &record.stage_dir())?;
            overlay.target = target.clone();
            if let Some(primary_lower) = record.overlay_lowers.first_mut() {
                *primary_lower = target;
            } else {
                record.overlay_lowers.push(target);
            }
        }
        apply_overlay(&mut overlay)?;
        println!("applied {} → {}", record.run_id, overlay.target.display());
    } else {
        discard_overlay(&mut overlay)?;
        println!("dropped {} (target untouched)", record.run_id);
    }
    record.overlay = Some(overlay);
    record.write()?;
    Ok(())
}

fn resolve_apply_target(target: &Path, stage: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(target)
        .with_context(|| format!("create apply target {}", target.display()))?;
    let target = target
        .canonicalize()
        .with_context(|| format!("resolve apply target {}", target.display()))?;
    let stage = stage.canonicalize().unwrap_or_else(|_| stage.to_path_buf());
    if target.starts_with(&stage) || stage.starts_with(&target) {
        bail!(
            "apply target must not overlap the pVisor stage: target={}, stage={}",
            target.display(),
            stage.display()
        );
    }
    Ok(target)
}

fn selected(selector: Option<&Path>, output_dir: &Path) -> anyhow::Result<RunRecord> {
    let storage = output_dir
        .canonicalize()
        .unwrap_or_else(|_| output_dir.to_path_buf());
    resolve_run(selector, &storage)
}

fn shell_join(parts: &[String]) -> String {
    parts
        .iter()
        .map(|part| {
            if part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || "-._/".contains(ch))
            {
                part.clone()
            } else {
                format!("{:?}", part)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_join_quotes_only_when_needed() {
        assert_eq!(
            shell_join(&["rg".into(), "hello world".into()]),
            "rg \"hello world\""
        );
    }
}
