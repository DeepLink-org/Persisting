//! Product-facing review and logical checkpoint commands.

use crate::runtime::{resolve_run, RunRecord};
use crate::{create_logical_checkpoint, RunBundle};
use anyhow::Context;
use clap::Args;
use std::path::{Path, PathBuf};

const DEFAULT_STORAGE: &str = ".persisting/capture";

#[derive(Debug, Clone, Args)]
pub struct ReviewArgs {
    /// Run id, project workspace, run.json, or a path inside the Run filesystem.
    pub selector: Option<PathBuf>,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
    /// Emit the complete versioned Run Bundle.
    #[arg(long)]
    pub json: bool,
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
            Some(persisting_control::IsolationKind::RootlessProcess) => {
                "rootless user namespace + Landlock"
            }
            Some(persisting_control::IsolationKind::SandboxedProcess) => {
                "macOS Seatbelt process sandbox"
            }
            Some(persisting_control::IsolationKind::Container) => {
                "OCI container with injected pVisor"
            }
            Some(persisting_control::IsolationKind::VirtualMachine) => {
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
        for path in &filesystem.sample_paths {
            println!("  - {path}");
        }
    } else {
        println!("  host filesystem; no transactional change set");
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
    Ok(())
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
            "fork: pvisor fork {} --checkpoint {} [--workspace <PROJECT>] -- <agent>",
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
