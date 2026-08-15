//! Durable reusable execution environments backed by pVisor OverlayFS stages.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use persisting_overlayfs::jujutsu_upper_dir;

use crate::runtime::{
    all_runs, apply_overlay_selected, discard_overlay, is_live, load_overlay_record,
    mount_overlay_record, resolve_run, write_overlay_record, ApplySelection, OverlayRecord,
    OverlayState, OverlayUpper, RunLease, RunRecord,
};

#[derive(Debug, Args)]
pub struct EnvArgs {
    #[command(subcommand)]
    command: EnvCommand,
}

#[derive(Debug, Subcommand)]
enum EnvCommand {
    /// Create a durable environment in the ready state.
    Create(CreateArgs),
    /// Allow new exec/shell sessions in a stopped environment.
    Start(SelectArgs),
    /// Prevent new exec/shell sessions; the environment must be idle.
    Stop(SelectArgs),
    /// Execute one command with persistent filesystem changes.
    Exec(ExecArgs),
    /// Open an interactive shell with persistent filesystem changes.
    Shell(SelectArgs),
    /// List environments.
    List(ListArgs),
    /// Show environment state and filesystem summary.
    Status(StatusArgs),
    /// Open a read-only view or run a command in it.
    Inspect(InspectArgs),
    /// Apply staged filesystem changes to the target.
    Apply(ApplyArgs),
    /// Discard staged filesystem changes.
    Drop(SelectArgs),
    /// Permanently remove environment metadata and staged changes.
    Delete(DeleteArgs),
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum EnvBackend {
    #[default]
    Directory,
    Jujutsu,
}

#[derive(Debug, Args)]
struct CreateArgs {
    /// Stable environment name.
    name: String,
    /// Read-only filesystem base and default apply destination.
    #[arg(long, value_name = "DIR", default_value = ".")]
    target: PathBuf,
    /// Root containing all pVisor environments.
    #[arg(long, value_name = "DIR", env = "PERSISTING_ENV_HOME")]
    root: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = EnvBackend::Directory)]
    backend: EnvBackend,
    /// Shared Jujutsu store (defaults to `<env-root>/.jujutsu`).
    #[arg(long, value_name = "DIR")]
    jujutsu_store: Option<PathBuf>,
    #[arg(long, default_value = "agent")]
    agent: String,
}

#[derive(Debug, Clone, Args)]
struct SelectArgs {
    /// Environment name or workspace path.
    selector: PathBuf,
    #[arg(long, value_name = "DIR", env = "PERSISTING_ENV_HOME")]
    root: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ExecArgs {
    #[command(flatten)]
    select: SelectArgs,
    /// Command to execute.
    #[arg(last = true, required = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

#[derive(Debug, Args)]
struct ListArgs {
    #[arg(long, value_name = "DIR", env = "PERSISTING_ENV_HOME")]
    root: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct StatusArgs {
    #[command(flatten)]
    select: SelectArgs,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct InspectArgs {
    #[command(flatten)]
    select: SelectArgs,
    /// Command for the read-only view; defaults to $SHELL or /bin/bash.
    #[arg(last = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

#[derive(Debug, Args)]
struct ApplyArgs {
    #[command(flatten)]
    select: SelectArgs,
    #[arg(long, value_name = "DIR")]
    target: Option<PathBuf>,
    /// Apply this relative path and its descendants. Repeatable.
    #[arg(long = "path", value_name = "RELATIVE_PATH")]
    paths: Vec<PathBuf>,
    /// Include staged paths matching this glob. Repeatable.
    #[arg(long, value_name = "GLOB")]
    include: Vec<String>,
    /// Exclude staged paths matching this glob. Repeatable.
    #[arg(long, value_name = "GLOB")]
    exclude: Vec<String>,
    /// Explicitly apply every remaining staged change.
    #[arg(long)]
    all: bool,
}

#[derive(Debug, Args)]
struct DeleteArgs {
    #[command(flatten)]
    select: SelectArgs,
    /// Confirm permanent deletion of the staged upper and metadata.
    #[arg(long)]
    force: bool,
}

pub fn run(args: EnvArgs) -> Result<i32> {
    match args.command {
        EnvCommand::Create(args) => create(args),
        EnvCommand::Start(args) => set_accepting(args, true),
        EnvCommand::Stop(args) => set_accepting(args, false),
        EnvCommand::Exec(args) => exec(args.select, args.command),
        EnvCommand::Shell(args) => {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
            exec(args, vec![shell])
        }
        EnvCommand::List(args) => list(args),
        EnvCommand::Status(args) => status(args),
        EnvCommand::Inspect(args) => inspect(args),
        EnvCommand::Apply(args) => apply(args),
        EnvCommand::Drop(args) => drop_changes(args),
        EnvCommand::Delete(args) => delete(args),
    }
}

fn create(args: CreateArgs) -> Result<i32> {
    validate_name(&args.name)?;
    let root = resolve_root(args.root)?;
    let target = args
        .target
        .canonicalize()
        .with_context(|| format!("resolve environment target {}", args.target.display()))?;
    anyhow::ensure!(target.is_dir(), "environment target must be a directory");
    let stage = root.join(&args.name);
    anyhow::ensure!(
        !stage.exists(),
        "environment '{}' already exists at {}",
        args.name,
        stage.display()
    );
    anyhow::ensure!(
        !stage.starts_with(&target) && !target.starts_with(&stage),
        "environment root and target must not overlap: root={}, target={}",
        root.display(),
        target.display()
    );
    fs::create_dir_all(&stage)?;
    let jujutsu_store = args.jujutsu_store.unwrap_or_else(|| root.join(".jujutsu"));
    let overlay = OverlayRecord {
        id: args.name.clone(),
        target: target.clone(),
        upper: match args.backend {
            EnvBackend::Directory => OverlayUpper::Directory {
                upper_dir: stage.join("upper"),
                work_dir: stage.join("work"),
            },
            EnvBackend::Jujutsu => OverlayUpper::Jujutsu {
                upper_dir: jujutsu_upper_dir(&jujutsu_store, &args.name)?,
                store_path: jujutsu_store,
                workspace: args.name.clone(),
            },
        },
        merged_dir: stage.join("merged"),
        stage_dir: stage.clone(),
        excluded_paths: Vec::new(),
        auto_apply: false,
        auto_discard: false,
        protect_target: false,
        state: OverlayState::Staged,
    };
    let now = crate::unix_now_ms();
    RunRecord {
        schema_version: 1,
        run_id: args.name.clone(),
        parent_run_id: None,
        task_id: None,
        session_id: args.name.clone(),
        agent: args.agent,
        pid: 0,
        command: Vec::new(),
        executor: None,
        state: "ready".into(),
        started_at_unix_ms: now,
        finished_at_unix_ms: Some(now),
        storage: root,
        workspace: Some(target.clone()),
        overlaynet_listen: None,
        network_interception: None,
        network_interception_metrics: None,
        gateway_listen: None,
        network: serde_json::json!({"mode": "host"}),
        network_policy: None,
        environment: Default::default(),
        resource_limits: Default::default(),
        overlay: Some(overlay),
        overlay_lowers: vec![target],
        lineage: None,
        orchestration: Default::default(),
    }
    .write()?;
    println!("created environment '{}' at {}", args.name, stage.display());
    Ok(0)
}

fn set_accepting(args: SelectArgs, accepting: bool) -> Result<i32> {
    let mut record = selected(&args)?;
    ensure_idle(&record)?;
    let _lease = RunLease::acquire(&record.stage_dir())?;
    let overlay = record
        .overlay
        .as_ref()
        .context("environment has no OverlayFS stage")?;
    anyhow::ensure!(
        matches!(overlay.state, OverlayState::Staged),
        "environment filesystem is {:?}; it cannot be started or stopped",
        overlay.state
    );
    record.state = if accepting { "ready" } else { "stopped" }.into();
    record.write()?;
    println!(
        "environment '{}' is {}",
        record.run_id,
        if accepting { "ready" } else { "stopped" }
    );
    Ok(0)
}

fn exec(args: SelectArgs, command: Vec<String>) -> Result<i32> {
    let mut record = selected(&args)?;
    ensure_idle(&record)?;
    anyhow::ensure!(
        record.state == "ready",
        "environment '{}' is {}; run `pvisor env start {}` first",
        record.run_id,
        record.state,
        record.run_id
    );
    let overlay = record
        .overlay
        .clone()
        .context("environment has no OverlayFS stage")?;
    anyhow::ensure!(
        overlay.state == OverlayState::Staged,
        "environment filesystem is {:?}; exec requires staged changes",
        overlay.state
    );
    let lowers = if record.overlay_lowers.is_empty() {
        vec![overlay.target.clone()]
    } else {
        record.overlay_lowers.clone()
    };
    let lease = RunLease::acquire(&record.stage_dir())?;
    let mount = mount_overlay_record(&overlay, &lowers)?;
    let (program, program_args) = command
        .split_first()
        .context("missing environment command")?;
    let mut child = Command::new(program)
        .args(program_args)
        .current_dir(mount.mountpoint())
        .env("PERSISTING_ENV", &record.run_id)
        .env("PERSISTING_RUN_ID", &record.run_id)
        .spawn()
        .with_context(|| format!("execute environment command `{program}`"))?;
    record.pid = child.id();
    record.command = command;
    record.state = "running".into();
    record.started_at_unix_ms = crate::unix_now_ms();
    record.finished_at_unix_ms = None;
    record.overlay = Some(mount.record().clone());
    record.write()?;

    let status = child.wait().context("wait for environment command");
    let staged = mount.unmount()?;
    record.pid = 0;
    record.state = "ready".into();
    record.finished_at_unix_ms = Some(crate::unix_now_ms());
    record.overlay = Some(staged);
    record.write()?;
    drop(lease);
    let status = status?;
    Ok(status.code().unwrap_or(1))
}

fn list(args: ListArgs) -> Result<i32> {
    let root = resolve_root(args.root)?;
    let records = all_runs(&root)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&records)?);
    } else if records.is_empty() {
        println!("no environments under {}", root.display());
    } else {
        println!("NAME\tSTATE\tTARGET");
        for record in records {
            let target = record
                .overlay
                .as_ref()
                .map(|overlay| overlay.target.display().to_string())
                .unwrap_or_else(|| "-".into());
            println!("{}\t{}\t{}", record.run_id, record.state, target);
        }
    }
    Ok(0)
}

fn status(args: StatusArgs) -> Result<i32> {
    let root = resolve_root(args.select.root)?;
    super::runtime::status(super::runtime::StatusArgs {
        selector: Some(args.select.selector),
        output_dir: root,
        json: args.json,
    })?;
    Ok(0)
}

fn inspect(args: InspectArgs) -> Result<i32> {
    let root = resolve_root(args.select.root)?;
    let record = resolve_run(Some(&args.select.selector), &root)?;
    ensure_idle(&record)?;
    let _lease = RunLease::acquire(&record.stage_dir())?;
    super::runtime::inspect(super::runtime::InspectArgs {
        selector: Some(args.select.selector),
        output_dir: root,
        command: args.command,
    })
}

fn apply(args: ApplyArgs) -> Result<i32> {
    if args.all && (!args.paths.is_empty() || !args.include.is_empty() || !args.exclude.is_empty())
    {
        bail!("--all cannot be combined with --path, --include, or --exclude");
    }
    let mut record = selected(&args.select)?;
    ensure_idle(&record)?;
    let _lease = RunLease::acquire(&record.stage_dir())?;
    let mut overlay = current_overlay(&record)?;
    if let Some(target) = args.target {
        fs::create_dir_all(&target)
            .with_context(|| format!("create apply target {}", target.display()))?;
        let target = target
            .canonicalize()
            .with_context(|| format!("resolve apply target {}", target.display()))?;
        let stage = record
            .stage_dir()
            .canonicalize()
            .unwrap_or_else(|_| record.stage_dir());
        anyhow::ensure!(
            !target.starts_with(&stage) && !stage.starts_with(&target),
            "apply target must not overlap environment stage"
        );
        overlay.target = target.clone();
        record.overlay_lowers = vec![target];
    }
    let selection = ApplySelection {
        paths: args.paths,
        includes: args.include,
        excludes: args.exclude,
    };
    let lowers = if record.overlay_lowers.is_empty() {
        vec![overlay.target.clone()]
    } else {
        record.overlay_lowers.clone()
    };
    let outcome = apply_overlay_selected(&mut overlay, &lowers, &selection)?;
    overlay.state = OverlayState::Staged;
    write_overlay_record(&overlay)?;
    record.overlay = Some(overlay);
    record.state = "ready".into();
    record.write()?;
    println!(
        "applied {} changes from environment '{}' (apply_id={}, remaining={})",
        outcome.applied.len(),
        record.run_id,
        outcome.apply_id,
        outcome.remaining.len()
    );
    Ok(0)
}

fn drop_changes(args: SelectArgs) -> Result<i32> {
    let mut record = selected(&args)?;
    ensure_idle(&record)?;
    let _lease = RunLease::acquire(&record.stage_dir())?;
    let mut overlay = current_overlay(&record)?;
    discard_overlay(&mut overlay)?;
    overlay.state = OverlayState::Staged;
    write_overlay_record(&overlay)?;
    record.overlay = Some(overlay);
    record.state = "ready".into();
    record.write()?;
    println!(
        "dropped changes for environment '{}' and reset its stage",
        record.run_id
    );
    Ok(0)
}

fn delete(args: DeleteArgs) -> Result<i32> {
    anyhow::ensure!(args.force, "environment delete requires --force");
    let root = resolve_root(args.select.root)?;
    let record = resolve_run(Some(&args.select.selector), &root)?;
    ensure_idle(&record)?;
    let _lease = RunLease::acquire(&record.stage_dir())?;
    let stage = record.stage_dir();
    let root = root.canonicalize().unwrap_or(root);
    let stage = stage.canonicalize().unwrap_or(stage);
    anyhow::ensure!(
        stage.starts_with(&root) && stage != root,
        "refusing to delete environment outside root: {}",
        stage.display()
    );
    fs::remove_dir_all(&stage)
        .with_context(|| format!("delete environment stage {}", stage.display()))?;
    record.remove_index()?;
    println!("deleted environment '{}'", record.run_id);
    Ok(0)
}

fn selected(args: &SelectArgs) -> Result<RunRecord> {
    let root = resolve_root(args.root.clone())?;
    let mut record = resolve_run(Some(&args.selector), &root)?;
    if record.state == "running" && !is_live(&record.stage_dir())? {
        record.pid = 0;
        record.state = "ready".into();
        record.finished_at_unix_ms = Some(crate::unix_now_ms());
        record.overlay = Some(current_overlay(&record)?);
        record.write()?;
    }
    Ok(record)
}

fn current_overlay(record: &RunRecord) -> Result<OverlayRecord> {
    match load_overlay_record(&record.stage_dir()) {
        Ok(overlay) => Ok(overlay),
        Err(_) => record
            .overlay
            .clone()
            .context("environment has no OverlayFS stage"),
    }
}

fn ensure_idle(record: &RunRecord) -> Result<()> {
    anyhow::ensure!(
        !is_live(&record.stage_dir())?,
        "environment '{}' has a running command",
        record.run_id
    );
    Ok(())
}

fn resolve_root(root: Option<PathBuf>) -> Result<PathBuf> {
    let root = root
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".persisting/envs"))
        })
        .context("cannot determine environment root; pass --root or set PERSISTING_ENV_HOME")?;
    fs::create_dir_all(&root)?;
    Ok(root.canonicalize().unwrap_or(root))
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        bail!("environment name must be one non-empty path segment");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_list_stop_start_and_delete_metadata() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("envs");
        let target = temp.path().join("target");
        fs::create_dir(&target)?;
        create(CreateArgs {
            name: "demo".into(),
            target,
            root: Some(root.clone()),
            backend: EnvBackend::Directory,
            jujutsu_store: None,
            agent: "test".into(),
        })?;
        let select = SelectArgs {
            selector: PathBuf::from("demo"),
            root: Some(root.clone()),
        };
        assert_eq!(selected(&select)?.state, "ready");
        set_accepting(select.clone(), false)?;
        assert_eq!(selected(&select)?.state, "stopped");
        set_accepting(select.clone(), true)?;
        assert_eq!(selected(&select)?.state, "ready");
        list(ListArgs {
            root: Some(root.clone()),
            json: false,
        })?;
        delete(DeleteArgs {
            select,
            force: true,
        })?;
        assert!(!root.join("demo").exists());
        assert!(!root.join(".pvisor/runs/64656d6f.json").exists());
        Ok(())
    }

    #[test]
    fn apply_and_drop_reset_environment_for_reuse() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("envs");
        let target = temp.path().join("target");
        fs::create_dir(&target)?;
        create(CreateArgs {
            name: "demo".into(),
            target: target.clone(),
            root: Some(root.clone()),
            backend: EnvBackend::Directory,
            jujutsu_store: None,
            agent: "test".into(),
        })?;
        let select = SelectArgs {
            selector: PathBuf::from("demo"),
            root: Some(root),
        };
        let record = selected(&select)?;
        let overlay = record.overlay.context("overlay")?;
        let OverlayUpper::Directory { upper_dir, .. } = &overlay.upper else {
            unreachable!("directory fixture")
        };
        fs::create_dir_all(upper_dir)?;
        fs::write(upper_dir.join("committed.txt"), b"value")?;
        fs::write(upper_dir.join("later.txt"), b"later")?;
        write_overlay_record(&overlay)?;

        apply(ApplyArgs {
            select: select.clone(),
            target: None,
            paths: vec!["committed.txt".into()],
            include: Vec::new(),
            exclude: Vec::new(),
            all: false,
        })?;
        assert_eq!(fs::read(target.join("committed.txt"))?, b"value");
        assert!(!target.join("later.txt").exists());
        assert!(upper_dir.join("later.txt").exists());
        assert_eq!(
            selected(&select)?.overlay.context("overlay")?.state,
            OverlayState::Staged
        );

        apply(ApplyArgs {
            select: select.clone(),
            target: None,
            paths: Vec::new(),
            include: Vec::new(),
            exclude: Vec::new(),
            all: true,
        })?;
        assert_eq!(fs::read(target.join("later.txt"))?, b"later");

        fs::create_dir_all(upper_dir)?;
        fs::write(upper_dir.join("discarded.txt"), b"value")?;
        drop_changes(select.clone())?;
        assert!(!target.join("discarded.txt").exists());
        assert_eq!(
            selected(&select)?.overlay.context("overlay")?.state,
            OverlayState::Staged
        );
        Ok(())
    }

    #[test]
    fn names_cannot_escape_root() {
        assert!(validate_name("../escape").is_err());
        assert!(validate_name("nested/name").is_err());
        assert!(validate_name("valid-name").is_ok());
    }
}
