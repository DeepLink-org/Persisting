//! Product batch scenarios built from pPilot's pVisor and pChronicle seams.

use anyhow::{bail, Context};
use futures::{stream, stream::FuturesUnordered, Stream, StreamExt};
use persisting_agentctl::{RunId, RunInvocation, RunSpec, RunState, StdioMode};
use persisting_gateway::config::{CaptureLevel, NetworkConfig, OverlayConfig, ProxyConfig};
use persisting_pvisor::{GatewayDriverConfig, PVisor};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::pin::Pin;

pub const BATCH_PRODUCTION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchProductionManifest {
    #[serde(default = "production_schema_version")]
    pub schema_version: u32,
    pub batch_id: String,
    pub runs: Vec<TrajectoryProductionRun>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryProductionRun {
    pub id: String,
    #[serde(default = "default_agent")]
    pub agent: String,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct BatchProductionOptions {
    pub output_dir: PathBuf,
    pub parallelism: usize,
    pub capture_gateway: bool,
    pub supervisor_network_limit_bytes_per_second: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchProductionReport {
    pub schema_version: u32,
    pub batch_id: String,
    pub requested_parallelism: usize,
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub runs: Vec<ProductionRunOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductionRunOutcome {
    pub run_id: String,
    pub task_id: String,
    pub workspace: PathBuf,
    pub state: RunState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

impl BatchProductionManifest {
    pub fn from_path(path: &Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("read production manifest {}", path.display()))?;
        let manifest: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse production manifest {}", path.display()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.schema_version != BATCH_PRODUCTION_SCHEMA_VERSION {
            bail!(
                "unsupported production manifest schema {}; expected {}",
                self.schema_version,
                BATCH_PRODUCTION_SCHEMA_VERSION
            );
        }
        validate_id("batch_id", &self.batch_id)?;
        if self.runs.is_empty() {
            bail!("production manifest must contain at least one run");
        }
        let mut ids = BTreeSet::new();
        for run in &self.runs {
            run.validate()?;
            if !ids.insert(&run.id) {
                bail!("duplicate production run id {:?}", run.id);
            }
        }
        Ok(())
    }
}

impl TrajectoryProductionRun {
    fn validate(&self) -> anyhow::Result<()> {
        validate_id("run id", &self.id)?;
        if self.agent.trim().is_empty() {
            bail!("production run {} has an empty agent", self.id);
        }
        if self
            .command
            .first()
            .is_none_or(|program| program.trim().is_empty())
        {
            bail!("production run {} has an empty command", self.id);
        }
        Ok(())
    }

    fn from_plan_value(value: serde_json::Value) -> anyhow::Result<Self> {
        let run: Self = serde_json::from_value(value)
            .context("planner item is not a valid trajectory production Run")?;
        run.validate()?;
        Ok(run)
    }
}

/// Run each manifest entry in an independent pVisor workspace with bounded
/// concurrency. A durable report is written even when individual Runs fail.
pub async fn produce_trajectories(
    manifest: BatchProductionManifest,
    options: BatchProductionOptions,
) -> anyhow::Result<BatchProductionReport> {
    manifest.validate()?;
    let runs = stream::iter(manifest.runs.into_iter().map(Ok));
    produce_trajectory_stream(manifest.batch_id, Box::pin(runs), options).await
}

/// Run trajectory descriptions emitted incrementally by a Python planner.
/// The planner is back-pressured by the bounded execution window, so large
/// batches do not need to be materialized in pPilot memory.
pub async fn produce_from_planner(
    planner: PathBuf,
    python: PathBuf,
    planner_args: Vec<String>,
    batch_id: String,
    options: BatchProductionOptions,
) -> anyhow::Result<BatchProductionReport> {
    validate_id("batch_id", &batch_id)?;
    let runs = crate::plan::stream_plan_values(planner, python, planner_args)
        .map(|value| value.and_then(TrajectoryProductionRun::from_plan_value));
    produce_trajectory_stream(batch_id, Box::pin(runs), options).await
}

async fn produce_trajectory_stream(
    batch_id: String,
    mut source: Pin<Box<dyn Stream<Item = anyhow::Result<TrajectoryProductionRun>> + Send>>,
    options: BatchProductionOptions,
) -> anyhow::Result<BatchProductionReport> {
    if options.supervisor_network_limit_bytes_per_second.is_some() && !options.capture_gateway {
        bail!("Supervisor network limit requires the pVisor capture Gateway");
    }
    tokio::fs::create_dir_all(&options.output_dir)
        .await
        .with_context(|| format!("create batch output {}", options.output_dir.display()))?;
    let parallelism = options.parallelism.max(1);
    let output_dir = options.output_dir.clone();
    let capture_gateway = options.capture_gateway;
    let parent_run_id = format!("ppilot-batch-{batch_id}");
    let supervisor =
        crate::supervisor::EmbeddedSupervisor::start(crate::supervisor::EmbeddedSupervisorConfig {
            network_limit_bytes_per_second: options.supervisor_network_limit_bytes_per_second,
            quota_slots: parallelism,
        })
        .await
        .context("start embedded pPilot Supervisor")?;
    let supervisor_bootstrap = supervisor.bootstrap();

    let execution = async {
        let mut seen_ids = BTreeSet::new();
        let mut in_flight = FuturesUnordered::new();
        let mut runs = Vec::new();
        let mut source_finished = false;

        loop {
            while !source_finished && in_flight.len() < parallelism {
                match source.next().await {
                    Some(run) => {
                        let run = run?;
                        run.validate()?;
                        if !seen_ids.insert(run.id.clone()) {
                            bail!("duplicate production run id {:?}", run.id);
                        }
                        let output_dir = output_dir.clone();
                        let parent_run_id = parent_run_id.clone();
                        let batch_id = batch_id.clone();
                        let supervisor_bootstrap = supervisor_bootstrap.clone();
                        in_flight.push(async move {
                            run_production_entry(
                                run,
                                &batch_id,
                                &parent_run_id,
                                &output_dir,
                                capture_gateway,
                                supervisor_bootstrap,
                            )
                            .await
                        });
                    }
                    None => source_finished = true,
                }
            }

            match in_flight.next().await {
                Some(outcome) => runs.push(outcome?),
                None if source_finished => break,
                None => continue,
            }
        }

        if runs.is_empty() {
            bail!("production planner must emit at least one Run");
        }
        anyhow::Ok(runs)
    }
    .await;
    let shutdown = supervisor.shutdown().await;
    let mut runs = execution?;
    shutdown.context("shut down embedded pPilot Supervisor")?;
    runs.sort_by(|left, right| left.run_id.cmp(&right.run_id));
    let completed = runs
        .iter()
        .filter(|outcome| outcome.state == RunState::Completed)
        .count();
    let report = BatchProductionReport {
        schema_version: BATCH_PRODUCTION_SCHEMA_VERSION,
        batch_id,
        requested_parallelism: parallelism,
        total: runs.len(),
        completed,
        failed: runs.len().saturating_sub(completed),
        runs,
    };
    write_json_atomic(&output_dir.join("production-report.json"), &report).await?;
    Ok(report)
}

async fn run_production_entry(
    run: TrajectoryProductionRun,
    batch_id: &str,
    parent_run_id: &str,
    output_dir: &Path,
    capture_gateway: bool,
    supervisor: persisting_agentctl::SupervisorBootstrap,
) -> anyhow::Result<ProductionRunOutcome> {
    let workspace = output_dir.join(&run.id);
    if workspace.exists() {
        bail!(
            "production workspace already exists for {}: {}",
            run.id,
            workspace.display()
        );
    }
    let mut builder = PVisor::builder().storage(&workspace);
    if capture_gateway {
        let proxy = ProxyConfig {
            listen: free_loopback_address()?,
            admin_listen: free_loopback_address()?,
            agent_id: run.agent.clone(),
            session_header: "x-persisting-session-id".into(),
            capture_level: CaptureLevel::Dialogue,
            debug: false,
            network: NetworkConfig::default(),
            overlay: OverlayConfig::default(),
            models: Vec::new(),
        };
        builder = builder.gateway(
            GatewayDriverConfig::new(proxy)
                .output_dir(&workspace)
                .gateway_enabled(true),
        );
    }
    let pvisor = builder.build();
    let (program, args) = run.command.split_first().expect("manifest was validated");
    let mut spec = RunSpec::process(run.id.as_str(), run.agent.as_str(), program);
    spec.supervisor = Some(supervisor);
    spec.parent_run_id = Some(RunId::new(parent_run_id));
    spec.task_id = Some(run.id.clone());
    spec.metadata
        .insert("ppilot.batch_id".into(), serde_json::json!(batch_id));
    spec.metadata.insert(
        "ppilot.scope".into(),
        serde_json::json!("trajectory-production"),
    );
    let RunInvocation::Process(process) = &mut spec.invocation;
    process.args = args.to_vec();
    process.cwd = run.cwd.map(|path| path.display().to_string());
    process.env = run.env;
    process.stdout = StdioMode::Capture;
    process.stderr = StdioMode::Capture;

    let handle = pvisor
        .run(spec)
        .await
        .with_context(|| format!("submit production Run {}", run.id))?;
    let result = handle
        .wait()
        .await
        .with_context(|| format!("wait for production Run {}", run.id))?;
    Ok(ProductionRunOutcome {
        run_id: result.run_id.to_string(),
        task_id: run.id,
        workspace,
        state: result.state,
        exit_code: result.exit_code,
        failure: result.failure.map(|failure| failure.message),
    })
}

pub(crate) async fn write_json_atomic(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    write_bytes_atomic(path, &serde_json::to_vec_pretty(value)?).await
}

pub(crate) async fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("batch output path has no UTF-8 filename")?;
    let temporary = path.with_file_name(format!(".{name}.tmp"));
    tokio::fs::write(&temporary, bytes)
        .await
        .with_context(|| format!("write {}", temporary.display()))?;
    tokio::fs::rename(&temporary, path)
        .await
        .with_context(|| format!("rename {} to {}", temporary.display(), path.display()))?;
    Ok(())
}

fn validate_id(label: &str, id: &str) -> anyhow::Result<()> {
    let id = id.trim();
    if id.is_empty() || id == "." || id == ".." || id.contains('/') || id.contains('\\') {
        bail!("{label} must be one non-empty path-safe segment");
    }
    Ok(())
}

fn free_loopback_address() -> anyhow::Result<String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.to_string())
}

const fn production_schema_version() -> u32 {
    BATCH_PRODUCTION_SCHEMA_VERSION
}

fn default_agent() -> String {
    "agent".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_manifest_rejects_duplicate_and_unsafe_ids() {
        let mut manifest = BatchProductionManifest {
            schema_version: 1,
            batch_id: "batch-1".into(),
            runs: vec![
                TrajectoryProductionRun {
                    id: "run-1".into(),
                    agent: "test".into(),
                    command: vec!["/bin/true".into()],
                    cwd: None,
                    env: Default::default(),
                },
                TrajectoryProductionRun {
                    id: "run-1".into(),
                    agent: "test".into(),
                    command: vec!["/bin/true".into()],
                    cwd: None,
                    env: Default::default(),
                },
            ],
        };
        assert!(manifest
            .validate()
            .unwrap_err()
            .to_string()
            .contains("duplicate"));
        manifest.runs.pop();
        manifest.runs[0].id = "../escape".into();
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn production_plan_items_are_typed_and_validated() {
        let run = TrajectoryProductionRun::from_plan_value(serde_json::json!({
            "id": "run-1",
            "command": ["/bin/true"],
            "env": {"MODE": "test"}
        }))
        .unwrap();
        assert_eq!(run.agent, "agent");
        assert_eq!(run.env["MODE"], "test");

        let error = TrajectoryProductionRun::from_plan_value(serde_json::json!({
            "id": "../escape",
            "command": ["/bin/true"]
        }))
        .unwrap_err();
        assert!(error.to_string().contains("path-safe"));
    }

    #[tokio::test]
    async fn production_planner_rejects_duplicate_ids_incrementally() {
        let dir = tempfile::tempdir().unwrap();
        let planner = dir.path().join("duplicate.py");
        std::fs::write(
            &planner,
            r#"
def plan():
    yield {"id": "same", "command": ["/bin/true"]}
    yield {"id": "same", "command": ["/bin/true"]}
"#,
        )
        .unwrap();
        let error = produce_from_planner(
            planner,
            PathBuf::from("python3"),
            vec![],
            "duplicates".into(),
            BatchProductionOptions {
                output_dir: dir.path().join("runs"),
                parallelism: 2,
                capture_gateway: false,
                supervisor_network_limit_bytes_per_second: None,
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("duplicate production run id"));
    }

    #[tokio::test]
    async fn production_limit_requires_the_intercepting_gateway() {
        let manifest = BatchProductionManifest {
            schema_version: 1,
            batch_id: "batch-limit".into(),
            runs: vec![TrajectoryProductionRun {
                id: "run-1".into(),
                agent: "test".into(),
                command: vec!["/bin/true".into()],
                cwd: None,
                env: Default::default(),
            }],
        };
        let output = tempfile::tempdir().unwrap();
        let error = produce_trajectories(
            manifest,
            BatchProductionOptions {
                output_dir: output.path().join("runs"),
                parallelism: 1,
                capture_gateway: false,
                supervisor_network_limit_bytes_per_second: Some(1024),
            },
        )
        .await
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("requires the pVisor capture Gateway"));
    }
}
