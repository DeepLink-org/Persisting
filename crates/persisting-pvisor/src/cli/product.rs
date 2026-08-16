//! Product-facing review and logical checkpoint commands.

use crate::runtime::{resolve_run, RunRecord};
use crate::{create_logical_checkpoint, ChangeEntryType, ChangeKind, RunBundle};
use anyhow::Context;
use clap::Args;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_STORAGE: &str = ".persisting/capture";
const DEFAULT_DIFF_BYTES: usize = 256 * 1024;
const DEFAULT_DIFF_FILE_BYTES: u64 = 1024 * 1024;
const REVIEW_PATH_LIMIT: usize = 200;

#[derive(Debug, Clone, Args)]
pub struct ReviewArgs {
    /// Run id, project workspace, run.json, or a path inside the Run filesystem.
    pub selector: Option<PathBuf>,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
    /// Emit the complete versioned Run Bundle.
    #[arg(long)]
    pub json: bool,
    /// Show bounded unified text diffs after the classified change list.
    #[arg(long, conflicts_with = "json")]
    pub diff: bool,
    /// Maximum total bytes emitted by --diff.
    #[arg(long, default_value_t = DEFAULT_DIFF_BYTES, requires = "diff")]
    pub max_diff_bytes: usize,
    /// Skip content diff for any file larger than this many bytes.
    #[arg(long, default_value_t = DEFAULT_DIFF_FILE_BYTES, requires = "diff")]
    pub max_diff_file_bytes: u64,
}

#[derive(Debug, Clone, Args)]
pub struct CheckpointArgs {
    /// Stopped Run id, project workspace, or run.json.
    pub selector: Option<PathBuf>,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
    /// Stable checkpoint name; generated when omitted.
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    #[arg(long)]
    pub json: bool,
}

pub fn review(args: ReviewArgs) -> anyhow::Result<()> {
    let record = selected(args.selector.as_deref(), &args.output_dir)?;
    let bundle = RunBundle::read(&record.stage_dir()).with_context(|| {
        format!(
            "Run {} has no readable Run Bundle; re-run it with this pVisor version",
            record.run_id
        )
    })?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&bundle)?);
        return Ok(());
    }

    println!("pVisor review — {}", bundle.run.run_id);
    println!(
        "outcome: {:?} (exit {:?})",
        bundle.run.state, bundle.run.exit_code
    );
    println!("agent: {}", bundle.run.agent);
    println!("duration: {} ms", bundle.run.duration_ms);
    if bundle.run.parent_run_id.is_some() || bundle.run.task_id.is_some() {
        println!(
            "orchestration: parent={} task={}",
            bundle.run.parent_run_id.as_deref().unwrap_or("-"),
            bundle.run.task_id.as_deref().unwrap_or("-")
        );
    }
    for (key, value) in &bundle.orchestration {
        println!("  {key}: {value}");
    }
    if let Some(lineage) = &bundle.lineage {
        println!(
            "lineage: {} @ {}",
            lineage.parent_run_id, lineage.checkpoint_id
        );
    }

    println!("\nSafety boundary");
    println!(
        "  filesystem: {}",
        if bundle.safety.filesystem_non_bypassable {
            "kernel-enforced read/write roots with staged workspace"
        } else if bundle.safety.filesystem_write_non_bypassable {
            "kernel-enforced staged writes; reads remain ambient"
        } else if bundle.safety.filesystem_changes_staged {
            "changes staged for review"
        } else {
            "no staged change set"
        }
    );
    println!(
        "  network: {}",
        if bundle.safety.network_non_bypassable {
            "non-bypassable enforcement"
        } else if bundle.network.interception.is_some() {
            "cooperative proxy coverage"
        } else {
            "host network"
        }
    );
    println!(
        "  process: {}",
        match bundle
            .run
            .executor
            .as_ref()
            .map(|executor| executor.isolation)
        {
            Some(persisting_agentctl::IsolationKind::RootlessProcess) => {
                "rootless user namespace + Landlock"
            }
            Some(persisting_agentctl::IsolationKind::SandboxedProcess) => {
                "macOS Seatbelt process sandbox"
            }
            Some(persisting_agentctl::IsolationKind::Container) => {
                "OCI container with injected pVisor"
            }
            Some(persisting_agentctl::IsolationKind::VirtualMachine) => {
                "libkrun/KVM guest over the pVisor root OverlayFS"
            }
            _ => "host process (not a host-isolation boundary)",
        }
    );
    for warning in &bundle.safety.warnings {
        println!("  warning: {warning}");
    }
    if let Some(metrics) = &bundle.network.intercepted {
        println!(
            "  intercepted: {} requests ({} allowed, {} denied, {} failures)",
            metrics.requests_seen, metrics.policy_allowed, metrics.policy_denied, metrics.failures
        );
    }

    println!("\nChanges");
    if let Some(filesystem) = &bundle.filesystem {
        println!(
            "  {} changed paths, {} deletions/whiteouts",
            filesystem.changed_files, filesystem.whiteouts
        );
        println!("  target: {}", filesystem.target.display());
        let mut counts = BTreeMap::new();
        for change in &filesystem.changes {
            *counts.entry(change.kind).or_insert(0usize) += 1;
        }
        if !counts.is_empty() {
            println!(
                "  classified: {} added, {} modified, {} deleted, {} type-changed, {} opaque",
                counts.get(&ChangeKind::Added).copied().unwrap_or(0),
                counts.get(&ChangeKind::Modified).copied().unwrap_or(0),
                counts.get(&ChangeKind::Deleted).copied().unwrap_or(0),
                counts.get(&ChangeKind::TypeChanged).copied().unwrap_or(0),
                counts.get(&ChangeKind::Opaque).copied().unwrap_or(0),
            );
        }
        for change in filesystem.changes.iter().take(REVIEW_PATH_LIMIT) {
            let code = match change.kind {
                ChangeKind::Added => "A",
                ChangeKind::Modified => "M",
                ChangeKind::Deleted => "D",
                ChangeKind::TypeChanged => "T",
                ChangeKind::Opaque => "O",
            };
            let mode = change
                .mode
                .map(|mode| format!(" mode={mode:04o}"))
                .unwrap_or_default();
            println!("  {code} {}{mode}", change.path);
        }
        if filesystem.changes.len() > REVIEW_PATH_LIMIT {
            println!(
                "  … {} more paths; use --json for the complete manifest",
                filesystem.changes.len() - REVIEW_PATH_LIMIT
            );
        } else if filesystem.changes.is_empty() {
            for path in &filesystem.sample_paths {
                println!("  - {path}");
            }
        }
    } else {
        println!("  host filesystem; no transactional change set");
    }

    println!("\nObserved Agent state");
    println!("  AgentCtl clients: {}", bundle.agentctl.clients.len());
    println!(
        "  registered processes: {}",
        bundle.agentctl.processes.len()
    );
    let open_operations = bundle
        .agentctl
        .operations
        .iter()
        .filter(|operation| operation.completion.is_none())
        .count();
    println!(
        "  declared operations: {} total, {} open",
        bundle.agentctl.operations.len(),
        open_operations
    );
    println!("\nEnvironment and resources");
    println!(
        "  host environment inherited: {}",
        bundle.environment.inherits_host
    );
    println!(
        "  projected env keys: {}",
        if bundle.environment.projected_keys.is_empty() {
            "-".into()
        } else {
            bundle.environment.projected_keys.join(", ")
        }
    );
    println!(
        "  runtime-injected env keys: {}",
        if bundle.environment.runtime_injected_keys.is_empty() {
            "-".into()
        } else {
            bundle.environment.runtime_injected_keys.join(", ")
        }
    );
    println!(
        "  requested limits: {}",
        serde_json::to_string(&bundle.resources.requested)?
    );
    println!(
        "  effective limits: {}",
        serde_json::to_string(&bundle.resources.effective)?
    );
    if !bundle.resources.mechanisms.is_empty() {
        println!("  mechanisms: {}", bundle.resources.mechanisms.join(", "));
    }
    for limitation in &bundle.resources.limitations {
        println!("  limitation: {limitation}");
    }
    if let Some(failure) = &bundle.run.failure {
        println!("\nFailure\n  {:?}: {}", failure.kind, failure.message);
    }
    println!(
        "\nBundle: {}",
        RunBundle::path(&record.stage_dir()).display()
    );
    if bundle.filesystem.is_some() {
        println!("Next:");
        println!("  pvisor inspect {}", record.stage_dir().display());
        println!("  pvisor checkpoint {}", record.stage_dir().display());
        println!("  pvisor apply {}", record.stage_dir().display());
        println!("  pvisor drop {}", record.stage_dir().display());
    }
    if args.diff {
        print_diffs(
            &record,
            &bundle,
            args.max_diff_bytes,
            args.max_diff_file_bytes,
        )?;
    }
    Ok(())
}

fn print_diffs(
    record: &RunRecord,
    bundle: &RunBundle,
    max_total_bytes: usize,
    max_file_bytes: u64,
) -> anyhow::Result<()> {
    let Some(filesystem) = &bundle.filesystem else {
        return Ok(());
    };
    let overlay = record
        .overlay
        .as_ref()
        .context("Run Bundle has a changeset but Run overlay metadata is missing")?;
    let lowers = if record.overlay_lowers.is_empty() {
        vec![overlay.target.clone()]
    } else {
        record.overlay_lowers.clone()
    };
    let mut remaining = max_total_bytes;
    println!("\nDiff");
    for change in &filesystem.changes {
        if remaining == 0 {
            println!("  … diff output truncated at {max_total_bytes} bytes");
            break;
        }
        let relative = safe_change_path(&change.path)?;
        let old = lowers
            .iter()
            .map(|lower| lower.join(&relative))
            .find(|path| fs::symlink_metadata(path).is_ok());
        let new = overlay.upper.path().join(&relative);
        if change.kind == ChangeKind::Opaque {
            println!("opaque directory: {}", change.path);
            continue;
        }
        if change.old_type == Some(ChangeEntryType::Symlink)
            || change.new_type == Some(ChangeEntryType::Symlink)
        {
            println!(
                "symlink {}: {} -> {}",
                change.path,
                old.as_deref()
                    .and_then(read_link_label)
                    .unwrap_or_else(|| "-".into()),
                read_link_label(&new).unwrap_or_else(|| "-".into())
            );
            continue;
        }
        let old_file = old.as_deref().filter(|path| path.is_file());
        let new_file = new.is_file().then_some(new.as_path());
        if old_file.is_none() && new_file.is_none() {
            continue;
        }
        if [old_file, new_file]
            .into_iter()
            .flatten()
            .any(|path| fs::metadata(path).is_ok_and(|metadata| metadata.len() > max_file_bytes))
        {
            println!("binary/large {} (content diff skipped)", change.path);
            continue;
        }
        if [old_file, new_file]
            .into_iter()
            .flatten()
            .any(is_binary_file)
        {
            println!("binary {} (content diff skipped)", change.path);
            continue;
        }
        let old_arg = old_file.unwrap_or_else(|| Path::new("/dev/null"));
        let new_arg = new_file.unwrap_or_else(|| Path::new("/dev/null"));
        let output = Command::new("diff")
            .args(["-u", "--label"])
            .arg(format!("a/{}", change.path))
            .arg("--label")
            .arg(format!("b/{}", change.path))
            .arg("--")
            .arg(old_arg)
            .arg(new_arg)
            .output()
            .with_context(|| format!("render diff for {}", change.path))?;
        anyhow::ensure!(
            matches!(output.status.code(), Some(0 | 1)),
            "diff failed for {}: {}",
            change.path,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let keep = remaining.min(output.stdout.len());
        print!("{}", String::from_utf8_lossy(&output.stdout[..keep]));
        remaining -= keep;
    }
    Ok(())
}

fn safe_change_path(path: &str) -> anyhow::Result<PathBuf> {
    use std::path::Component;
    let path = Path::new(path);
    anyhow::ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir)),
        "unsafe change path in Run Bundle: {}",
        path.display()
    );
    Ok(path.to_path_buf())
}

fn read_link_label(path: &Path) -> Option<String> {
    fs::read_link(path)
        .ok()
        .map(|target| target.display().to_string())
}

fn is_binary_file(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = fs::File::open(path) else {
        return true;
    };
    let mut prefix = [0_u8; 8192];
    let read = file.read(&mut prefix).unwrap_or(0);
    prefix[..read].contains(&0)
}

pub fn checkpoint(args: CheckpointArgs) -> anyhow::Result<()> {
    let record = selected(args.selector.as_deref(), &args.output_dir)?;
    let checkpoint = create_logical_checkpoint(&record, args.name.as_deref())?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&checkpoint)?);
    } else {
        println!(
            "checkpointed {} @ {} ({:?})",
            checkpoint.run_id, checkpoint.checkpoint_id, checkpoint.consistency
        );
        println!("manifest: {}", checkpoint.manifest_path().display());
        println!(
            "fork: pvisor fork {} --checkpoint {} -- <agent>",
            record.stage_dir().display(),
            checkpoint.checkpoint_id
        );
    }
    Ok(())
}

fn selected(selector: Option<&Path>, output_dir: &Path) -> anyhow::Result<RunRecord> {
    let storage = output_dir
        .canonicalize()
        .unwrap_or_else(|_| output_dir.to_path_buf());
    resolve_run(selector, &storage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_paths_cannot_escape_the_overlay_roots() {
        assert!(safe_change_path("src/lib.rs").is_ok());
        assert!(safe_change_path("../host-secret").is_err());
        assert!(safe_change_path("/etc/passwd").is_err());
    }

    #[test]
    fn binary_probe_detects_nul_bytes() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), b"text\0binary").unwrap();
        assert!(is_binary_file(temp.path()));
        fs::write(temp.path(), b"plain text\n").unwrap();
        assert!(!is_binary_file(temp.path()));
    }
}
