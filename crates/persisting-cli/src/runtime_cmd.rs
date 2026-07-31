//! `persisting runtime` / `persisting run` — pVisor ops.
//!
//! Inspect the library runtime and manage overlay staging (apply / discard).

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use persisting_pvisor::{
    apply_overlay, discard_overlay, load_overlay_by_id, load_overlay_record, overlay_status,
    PVisor, ProcessExecutor, RunExecutor,
};
use std::path::PathBuf;

#[derive(Debug, Clone, Args)]
#[command(
    name = "runtime",
    about = "pVisor runtime ops (alias: run)",
    long_about = "Inspect pVisor capabilities and manage overlay staging.\n\n\
Agent launch stays under `persisting agent execute|bexecute`."
)]
pub struct RuntimeArgs {
    #[command(subcommand)]
    pub command: RuntimeCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum RuntimeCommand {
    /// Show pVisor capability summary.
    Status,
    /// Dump implant plan for a dry-run process RunSpec.
    Inspect {
        /// Program name used only for dry-run enrichment.
        #[arg(long, default_value = "true")]
        program: String,
    },
    /// List pVisor executors and providers.
    Providers,
    /// Overlay staging: status / apply / discard.
    Overlay(OverlayArgs),
}

#[derive(Debug, Clone, Args)]
pub struct OverlayArgs {
    #[command(subcommand)]
    pub command: OverlayCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum OverlayCommand {
    /// Show staging status for an overlay id (or `--stage` directory).
    Status(OverlaySelectArgs),
    /// Apply staged upper onto the target filesystem.
    Apply(OverlaySelectArgs),
    /// Discard staged upper (do not touch target).
    Discard(OverlaySelectArgs),
}

#[derive(Debug, Clone, Args)]
pub struct OverlaySelectArgs {
    /// Capture / pVisor storage root (default `.persisting/capture`).
    #[arg(
        long,
        short = 'o',
        value_name = "DIR",
        env = "PERSISTING_CAPTURE_STORAGE",
        default_value = ".persisting/capture"
    )]
    pub output_dir: PathBuf,
    /// Overlay id (`run-…` session id). Looks up `{output_dir}/.overlay/{id}`.
    #[arg(long)]
    pub id: Option<String>,
    /// Explicit stage directory containing `overlay.json`.
    #[arg(long, value_name = "DIR")]
    pub stage: Option<PathBuf>,
}

pub fn run_runtime(args: RuntimeArgs) -> Result<()> {
    let pvisor = PVisor::new();
    match args.command {
        RuntimeCommand::Status => {
            let caps = pvisor.capabilities();
            println!("pVisor: ready");
            println!("api: PVisor::run(spec) → RunHandle");
            println!(
                "capabilities: capture={} network={} filesystem={}",
                caps.capture, caps.network, caps.filesystem
            );
            println!("providers:");
            for provider in caps.providers {
                println!("  - {provider}");
            }
        }
        RuntimeCommand::Inspect { program } => {
            let spec = persisting_proto::RunSpec::process("inspect-run", "inspect", program);
            let plan = pvisor.plan_for(&spec);
            println!("pVisor inspect run_id={}", spec.run_id);
            println!("implant notes:");
            for note in &plan.notes {
                println!("  - {note}");
            }
            println!("implant env:");
            for (key, value) in &plan.env {
                println!("  {key}={value}");
            }
        }
        RuntimeCommand::Providers => {
            let exec = ProcessExecutor;
            let desc = RunExecutor::descriptor(&exec);
            println!("pVisor executors:");
            println!(
                "  - {} kind={:?} isolation={:?} enforces_capabilities={}",
                desc.name, desc.kind, desc.isolation, desc.enforces_capabilities
            );
            println!("pVisor providers:");
            for provider in pvisor.capabilities().providers {
                println!("  - {provider}");
            }
            println!("filesystem overlay: fuse-overlayfs + staging apply/discard");
        }
        RuntimeCommand::Overlay(args) => run_overlay(args)?,
    }
    Ok(())
}

fn run_overlay(args: OverlayArgs) -> Result<()> {
    match args.command {
        OverlayCommand::Status(sel) => {
            let mut record = load_selected(&sel)?;
            let status = overlay_status(&record).context("overlay status")?;
            println!("id: {}", status.record.id);
            println!("state: {:?}", status.record.state);
            println!("target: {}", status.record.target.display());
            println!("upper: {}", status.record.upper.path().display());
            println!("stage: {}", status.record.stage_dir.display());
            println!(
                "changes: {} files, {} whiteouts",
                status.changed_files, status.whiteouts
            );
            for path in &status.sample_paths {
                println!("  - {path}");
            }
            let _ = &mut record;
        }
        OverlayCommand::Apply(sel) => {
            let mut record = load_selected(&sel)?;
            apply_overlay(&mut record).context("overlay apply")?;
            println!(
                "applied overlay {} → {}",
                record.id,
                record.target.display()
            );
        }
        OverlayCommand::Discard(sel) => {
            let mut record = load_selected(&sel)?;
            discard_overlay(&mut record).context("overlay discard")?;
            println!("discarded overlay {} (target untouched)", record.id);
        }
    }
    Ok(())
}

fn load_selected(sel: &OverlaySelectArgs) -> Result<persisting_pvisor::OverlayRecord> {
    if let Some(stage) = &sel.stage {
        return load_overlay_record(stage)
            .with_context(|| format!("load overlay meta in {}", stage.display()));
    }
    let Some(id) = &sel.id else {
        bail!("specify `--id <run-…>` or `--stage <dir>`");
    };
    let storage = sel
        .output_dir
        .canonicalize()
        .unwrap_or_else(|_| sel.output_dir.clone());
    load_overlay_by_id(&storage, id)
        .with_context(|| format!("load overlay id={id} under {}", storage.display()))
}
