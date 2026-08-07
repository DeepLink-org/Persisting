//! Two-level pChronicle aggregation over a Pulsing worker fleet.
//!
//! Version 1 deliberately supports only typed distributive count metrics. Each
//! worker normalizes one or more ATIF shards with pChronicle, returns a typed
//! partial count, and the pPilot coordinator validates and merges those
//! partials. Arbitrary SQL is not rewritten as a distributed plan.

use crate::batch::{
    balanced_shards, load_analysis_trajectories, write_bytes_atomic, write_json_atomic,
};
use crate::digest::sha256_hex;
use crate::dist::DistEnv;
use crate::pulsing_ext::{ask_timeout, resolve_actor};
use anyhow::{bail, Context};
use futures::{stream, StreamExt, TryStreamExt};
use persisting_pchronicle::{AtifDataSource, AtifTrajectory, ChronicleQueryEngine};
use pulsing_actor::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

pub const FEDERATED_COUNT_SCHEMA_VERSION: u32 = 1;
pub const PROCESS_SCRIPT_SCHEMA_VERSION: u32 = 1;
const FEDERATED_ASK_TIMEOUT: Duration = Duration::from_secs(120);
const PYTHON_STAGE_TIMEOUT: Duration = Duration::from_secs(110);
const MAX_SCRIPT_BYTES: usize = 1024 * 1024;

const MAP_REDUCE_BOOTSTRAP: &str = r#"
import asyncio, contextlib, importlib.util, inspect, json, sys
from pathlib import Path

stage, script = sys.argv[1], Path(sys.argv[2]).resolve()
payload = json.load(sys.stdin)

async def main():
    spec = importlib.util.spec_from_file_location("ppilot_map_reduce_job", script)
    module = importlib.util.module_from_spec(spec)
    with contextlib.redirect_stdout(sys.stderr):
        spec.loader.exec_module(module)
        names = ("map", "mapper") if stage == "map" else ("reduce", "reducer")
        fn = next((getattr(module, name) for name in names if hasattr(module, name)), None)
        if fn is None:
            raise RuntimeError(f"process script must define {names[0]}() or {names[1]}()")
        data = payload["records"] if stage == "map" else payload["partials"]
        context = payload["context"]
        try:
            parameter_count = len(inspect.signature(fn).parameters)
        except (TypeError, ValueError):
            parameter_count = 2
        result = fn(data) if parameter_count <= 1 else fn(data, context)
        if inspect.isawaitable(result):
            result = await result
    print(json.dumps(result, ensure_ascii=False, separators=(",", ":")))

asyncio.run(main())
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CountTable {
    /// Number of trajectory/run documents.
    Runs,
    /// Number of normalized interaction steps.
    Steps,
    /// Number of tool invocations.
    ToolCalls,
    /// Sum of `steps.llm_call_count`, which can exceed the number of steps.
    LlmCalls,
    /// Number of steps explicitly marked as copied context.
    CopiedContextSteps,
}

impl CountTable {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Runs => "runs",
            Self::Steps => "steps",
            Self::ToolCalls => "tool_calls",
            Self::LlmCalls => "llm_calls",
            Self::CopiedContextSteps => "copied_context_steps",
        }
    }

    const fn count_sql(self) -> &'static str {
        match self {
            Self::Runs => "SELECT COUNT(*) AS count FROM runs",
            Self::Steps => "SELECT COUNT(*) AS count FROM steps",
            Self::ToolCalls => "SELECT COUNT(*) AS count FROM tool_calls",
            Self::LlmCalls => "SELECT COALESCE(SUM(llm_call_count), 0) AS count FROM steps",
            Self::CopiedContextSteps => {
                "SELECT COUNT(*) AS count FROM steps WHERE is_copied_context = true"
            }
        }
    }
}

impl std::fmt::Display for CountTable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct FederatedCountOptions {
    pub input: PathBuf,
    pub output_dir: PathBuf,
    pub parallelism: usize,
    pub table: CountTable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederatedCountReport {
    pub schema_version: u32,
    pub mode: String,
    pub table: CountTable,
    pub aggregation_levels: usize,
    pub requested_parallelism: usize,
    pub worker_count: usize,
    pub shard_count: usize,
    pub trajectories: usize,
    pub count: u64,
    pub output: PathBuf,
    pub partials: Vec<FederatedCountPartialReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederatedCountPartialReport {
    pub shard_id: usize,
    pub worker_rank: usize,
    pub trajectory_ids: Vec<String>,
    pub count: u64,
    pub payload_bytes: usize,
    pub output: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ProcessScriptOptions {
    pub input: PathBuf,
    pub script: PathBuf,
    pub output_dir: Option<PathBuf>,
    pub mappers: usize,
    pub python: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessScriptReport {
    pub schema_version: u32,
    pub mode: String,
    pub script_name: String,
    pub script_sha256: String,
    pub requested_mappers: usize,
    pub worker_count: usize,
    pub shard_count: usize,
    pub trajectories: usize,
    pub result: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<PathBuf>,
    pub partials: Vec<ProcessMapperReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessMapperReport {
    pub shard_id: usize,
    pub worker_rank: usize,
    pub trajectory_ids: Vec<String>,
    pub result: serde_json::Value,
    pub input_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FederatedAnalysisCommand {
    Count {
        shard_id: usize,
        table: CountTable,
        trajectories_json: Vec<u8>,
    },
    Map {
        shard_id: usize,
        python: PathBuf,
        script_name: String,
        script_bytes: Vec<u8>,
        trajectories_json: Vec<u8>,
    },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FederatedAnalysisReply {
    Partial {
        shard_id: usize,
        worker_rank: usize,
        trajectories: usize,
        count: u64,
        payload_bytes: usize,
    },
    Mapped {
        shard_id: usize,
        worker_rank: usize,
        trajectories: usize,
        input_bytes: usize,
        result_json: Vec<u8>,
    },
    Failed {
        shard_id: usize,
        worker_rank: usize,
        message: String,
    },
    Stopped,
}

/// Pulsing actor that owns the node-local pChronicle analysis stage.
pub struct FederatedAnalysisWorker {
    rank: usize,
    shutdown: CancellationToken,
    completed_shards: usize,
}

impl FederatedAnalysisWorker {
    pub fn new(rank: usize, shutdown: CancellationToken) -> Self {
        Self {
            rank,
            shutdown,
            completed_shards: 0,
        }
    }

    async fn count_shard(
        &self,
        shard_id: usize,
        table: CountTable,
        trajectories_json: &[u8],
    ) -> anyhow::Result<FederatedAnalysisReply> {
        let trajectories: Vec<AtifTrajectory> = serde_json::from_slice(trajectories_json)
            .with_context(|| format!("decode federated shard {shard_id}"))?;
        let source = AtifDataSource::from_trajectories(&trajectories)
            .with_context(|| format!("normalize federated shard {shard_id}"))?;
        let engine = ChronicleQueryEngine::from_atif_source(source)
            .with_context(|| format!("build pChronicle engine for shard {shard_id}"))?;
        let jsonl = engine
            .query_jsonl(table.count_sql())
            .await
            .with_context(|| format!("query pChronicle shard {shard_id}"))?;
        let mut rows = jsonl.lines();
        let row: serde_json::Value = serde_json::from_str(
            rows.next()
                .context("pChronicle COUNT returned no partial row")?,
        )?;
        anyhow::ensure!(
            rows.next().is_none(),
            "pChronicle COUNT returned multiple rows"
        );
        let count = row
            .get("count")
            .and_then(serde_json::Value::as_u64)
            .context("pChronicle COUNT row has no unsigned count")?;
        Ok(FederatedAnalysisReply::Partial {
            shard_id,
            worker_rank: self.rank,
            trajectories: trajectories.len(),
            count,
            payload_bytes: trajectories_json.len(),
        })
    }

    async fn map_shard(
        &self,
        shard_id: usize,
        python: &Path,
        script_name: &str,
        script_bytes: &[u8],
        trajectories_json: &[u8],
    ) -> anyhow::Result<FederatedAnalysisReply> {
        let trajectories: Vec<AtifTrajectory> = serde_json::from_slice(trajectories_json)
            .with_context(|| format!("decode process shard {shard_id}"))?;
        for trajectory in &trajectories {
            trajectory.validate().map_err(anyhow::Error::from)?;
        }
        let trajectory_ids = trajectories
            .iter()
            .map(|trajectory| trajectory.effective_session_id().map(str::to_owned))
            .collect::<persisting_pchronicle::Result<Vec<_>>>()?;
        let payload = serde_json::json!({
            "records": trajectories,
            "context": {
                "shard_id": shard_id,
                "worker_rank": self.rank,
                "trajectory_ids": trajectory_ids,
            }
        });
        let result = run_python_stage(python, script_name, script_bytes, "map", &payload)
            .await
            .with_context(|| format!("execute mapper for shard {shard_id}"))?;
        Ok(FederatedAnalysisReply::Mapped {
            shard_id,
            worker_rank: self.rank,
            trajectories: trajectory_ids.len(),
            input_bytes: trajectories_json.len(),
            result_json: serde_json::to_vec(&result)?,
        })
    }
}

#[async_trait]
impl Actor for FederatedAnalysisWorker {
    fn metadata(&self) -> HashMap<String, String> {
        HashMap::from([
            ("role".into(), "ppilot-federated-analysis".into()),
            ("rank".into(), self.rank.to_string()),
            ("completed_shards".into(), self.completed_shards.to_string()),
        ])
    }

    async fn receive(
        &mut self,
        message: Message,
        _context: &mut ActorContext,
    ) -> pulsing_actor::error::Result<Message> {
        let command: FederatedAnalysisCommand = message.unpack()?;
        let reply = match command {
            FederatedAnalysisCommand::Shutdown => {
                self.shutdown.cancel();
                FederatedAnalysisReply::Stopped
            }
            FederatedAnalysisCommand::Count {
                shard_id,
                table,
                trajectories_json,
            } => match self.count_shard(shard_id, table, &trajectories_json).await {
                Ok(reply) => {
                    self.completed_shards = self.completed_shards.saturating_add(1);
                    reply
                }
                Err(error) => FederatedAnalysisReply::Failed {
                    shard_id,
                    worker_rank: self.rank,
                    message: format!("{error:#}"),
                },
            },
            FederatedAnalysisCommand::Map {
                shard_id,
                python,
                script_name,
                script_bytes,
                trajectories_json,
            } => match self
                .map_shard(
                    shard_id,
                    &python,
                    &script_name,
                    &script_bytes,
                    &trajectories_json,
                )
                .await
            {
                Ok(reply) => {
                    self.completed_shards = self.completed_shards.saturating_add(1);
                    reply
                }
                Err(error) => FederatedAnalysisReply::Failed {
                    shard_id,
                    worker_rank: self.rank,
                    message: format!("{error:#}"),
                },
            },
        };
        Message::pack(&reply)
    }
}

async fn run_python_stage(
    python: &Path,
    script_name: &str,
    script_bytes: &[u8],
    stage: &str,
    payload: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    anyhow::ensure!(
        script_bytes.len() <= MAX_SCRIPT_BYTES,
        "process script exceeds {} byte limit",
        MAX_SCRIPT_BYTES
    );
    let suffix = Path::new(script_name)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_else(|| ".py".into());
    let mut script = tempfile::Builder::new()
        .prefix("ppilot-process-")
        .suffix(&suffix)
        .tempfile()
        .context("create transferred process script")?;
    use std::io::Write as _;
    script
        .write_all(script_bytes)
        .context("write transferred process script")?;
    script.flush()?;

    let mut child = Command::new(python)
        .arg("-c")
        .arg(MAP_REDUCE_BOOTSTRAP)
        .arg(stage)
        .arg(script.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawn process {stage} with {}", python.display()))?;
    let mut stdin = child.stdin.take().context("process Python stdin missing")?;
    stdin.write_all(&serde_json::to_vec(payload)?).await?;
    drop(stdin);
    let output = tokio::time::timeout(PYTHON_STAGE_TIMEOUT, child.wait_with_output())
        .await
        .with_context(|| format!("process {stage} timed out after {PYTHON_STAGE_TIMEOUT:?}"))??;
    if !output.status.success() {
        anyhow::bail!(
            "process {stage} exited {}: {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "process {stage} returned invalid JSON: {}",
            String::from_utf8_lossy(&output.stdout).trim()
        )
    })
}

/// Detect torchrun placement, execute on a local Pulsing fleet otherwise, and
/// return a report only on the coordinator rank.
pub async fn process_federated_count(
    options: FederatedCountOptions,
) -> anyhow::Result<Option<FederatedCountReport>> {
    match DistEnv::from_env()? {
        Some(dist) if dist.is_driver() => run_distributed_driver(dist, options).await.map(Some),
        Some(dist) => {
            run_distributed_worker(dist).await?;
            Ok(None)
        }
        None => run_local(options).await.map(Some),
    }
}

/// Execute a transferred Python map/reduce script locally or across torchrun
/// Pulsing ranks. Only the driver reads input/script paths and returns output.
pub async fn process_script(
    options: ProcessScriptOptions,
) -> anyhow::Result<Option<ProcessScriptReport>> {
    match DistEnv::from_env()? {
        Some(dist) if dist.is_driver() => {
            run_distributed_script_driver(dist, options).await.map(Some)
        }
        Some(dist) => {
            run_distributed_worker(dist).await?;
            Ok(None)
        }
        None => run_local_script(options).await.map(Some),
    }
}

/// Coordinator seam for script-processing integration tests and embedding.
pub async fn process_script_with_workers(
    options: ProcessScriptOptions,
    workers: Vec<ActorRef>,
) -> anyhow::Result<ProcessScriptReport> {
    if workers.is_empty() {
        bail!("script processing requires at least one Pulsing mapper worker");
    }
    let script_bytes = tokio::fs::read(&options.script)
        .await
        .with_context(|| format!("read process script {}", options.script.display()))?;
    anyhow::ensure!(!script_bytes.is_empty(), "process script must not be empty");
    anyhow::ensure!(
        script_bytes.len() <= MAX_SCRIPT_BYTES,
        "process script exceeds {MAX_SCRIPT_BYTES} byte limit"
    );
    let script_name = options
        .script
        .file_name()
        .and_then(|name| name.to_str())
        .context("process script requires a UTF-8 filename")?
        .to_owned();
    let script_sha256 = sha256_hex(&script_bytes);
    let trajectories = load_analysis_trajectories(&options.input).await?;
    let shard_indices = balanced_shards(trajectories.len(), options.mappers);
    let worker_count = workers.len();

    let mut partials = stream::iter(shard_indices.into_iter().enumerate().map(
        |(shard_id, indices)| {
            let worker = workers[shard_id % worker_count].clone();
            let documents = indices
                .into_iter()
                .map(|index| trajectories[index].clone())
                .collect::<Vec<_>>();
            let python = options.python.clone();
            let script_name = script_name.clone();
            let script_bytes = script_bytes.clone();
            async move {
                let trajectory_ids = documents
                    .iter()
                    .map(|document| document.effective_session_id().map(str::to_owned))
                    .collect::<persisting_pchronicle::Result<Vec<_>>>()?;
                let trajectories_json = serde_json::to_vec(&documents)?;
                let reply = ask_timeout::<_, FederatedAnalysisReply>(
                    &worker,
                    FederatedAnalysisCommand::Map {
                        shard_id,
                        python,
                        script_name,
                        script_bytes,
                        trajectories_json,
                    },
                    FEDERATED_ASK_TIMEOUT,
                )
                .await?;
                match reply {
                    FederatedAnalysisReply::Mapped {
                        shard_id: reply_shard,
                        worker_rank,
                        trajectories: reply_trajectories,
                        input_bytes,
                        result_json,
                    } => {
                        anyhow::ensure!(reply_shard == shard_id, "mapper returned wrong shard");
                        anyhow::ensure!(
                            reply_trajectories == trajectory_ids.len(),
                            "mapper processed {reply_trajectories} trajectories for shard {shard_id}, expected {}",
                            trajectory_ids.len()
                        );
                        Ok(ProcessMapperReport {
                            shard_id,
                            worker_rank,
                            trajectory_ids,
                            result: serde_json::from_slice(&result_json)?,
                            input_bytes,
                        })
                    }
                    FederatedAnalysisReply::Failed {
                        shard_id,
                        worker_rank,
                        message,
                    } => bail!("process mapper {worker_rank} failed shard {shard_id}: {message}"),
                    other => bail!("unexpected process mapper reply: {other:?}"),
                }
            }
        },
    ))
    .buffer_unordered(worker_count)
    .try_collect::<Vec<_>>()
    .await?;
    partials.sort_by_key(|partial| partial.shard_id);
    validate_process_partials(&partials)?;

    let reduce_payload = serde_json::json!({
        "partials": partials.iter().map(|partial| &partial.result).collect::<Vec<_>>(),
        "context": {
            "mapper_count": partials.len(),
            "worker_count": worker_count,
            "trajectories": trajectories.len(),
        }
    });
    let result = run_python_stage(
        &options.python,
        &script_name,
        &script_bytes,
        "reduce",
        &reduce_payload,
    )
    .await
    .context("execute process reducer on driver")?;

    let output = if let Some(output_dir) = &options.output_dir {
        tokio::fs::create_dir_all(output_dir)
            .await
            .with_context(|| format!("create process output {}", output_dir.display()))?;
        let output = output_dir.join("results.json");
        write_json_atomic(&output, &result).await?;
        Some(output)
    } else {
        None
    };
    let report = ProcessScriptReport {
        schema_version: PROCESS_SCRIPT_SCHEMA_VERSION,
        mode: "python_map_reduce".into(),
        script_name,
        script_sha256,
        requested_mappers: options.mappers.max(1),
        worker_count,
        shard_count: partials.len(),
        trajectories: trajectories.len(),
        result,
        output,
        partials,
    };
    if let Some(output_dir) = &options.output_dir {
        write_json_atomic(&output_dir.join("process-report.json"), &report).await?;
    }
    Ok(report)
}

fn validate_process_partials(partials: &[ProcessMapperReport]) -> anyhow::Result<()> {
    let mut shard_ids = BTreeSet::new();
    let mut trajectory_ids = BTreeSet::new();
    for partial in partials {
        anyhow::ensure!(
            shard_ids.insert(partial.shard_id),
            "duplicate mapper partial for shard {}",
            partial.shard_id
        );
        for trajectory_id in &partial.trajectory_ids {
            anyhow::ensure!(
                trajectory_ids.insert(trajectory_id),
                "trajectory {trajectory_id:?} appeared in multiple mapper partials"
            );
        }
    }
    for (expected, actual) in shard_ids.into_iter().enumerate() {
        anyhow::ensure!(
            expected == actual,
            "missing mapper partial for shard {expected}"
        );
    }
    Ok(())
}

/// Coordinator seam used by embedding hosts and cross-node integration tests.
pub async fn federated_count_with_workers(
    options: FederatedCountOptions,
    workers: Vec<ActorRef>,
) -> anyhow::Result<FederatedCountReport> {
    if workers.is_empty() {
        bail!("federated count requires at least one Pulsing worker");
    }
    let trajectories = load_analysis_trajectories(&options.input).await?;
    let shard_indices = balanced_shards(trajectories.len(), options.parallelism);
    tokio::fs::create_dir_all(&options.output_dir)
        .await
        .with_context(|| format!("create analysis output {}", options.output_dir.display()))?;

    let table = options.table;
    let worker_count = workers.len();
    let parallelism = options.parallelism.max(1);
    let mut partials = stream::iter(shard_indices.into_iter().enumerate().map(
        |(shard_id, indices)| {
            let worker = workers[shard_id % worker_count].clone();
            let documents = indices
                .into_iter()
                .map(|index| trajectories[index].clone())
                .collect::<Vec<_>>();
            async move {
                let trajectory_ids = documents
                    .iter()
                    .map(|document| document.effective_session_id().map(str::to_owned))
                    .collect::<persisting_pchronicle::Result<Vec<_>>>()?;
                let trajectories_json = serde_json::to_vec(&documents)?;
                let reply = ask_timeout::<_, FederatedAnalysisReply>(
                    &worker,
                    FederatedAnalysisCommand::Count {
                        shard_id,
                        table,
                        trajectories_json,
                    },
                    FEDERATED_ASK_TIMEOUT,
                )
                .await?;
                match reply {
                    FederatedAnalysisReply::Partial {
                        shard_id: reply_shard,
                        worker_rank,
                        trajectories: reply_trajectories,
                        count,
                        payload_bytes,
                    } => {
                        anyhow::ensure!(
                            reply_shard == shard_id,
                            "worker returned shard {reply_shard}, expected {shard_id}"
                        );
                        anyhow::ensure!(
                            reply_trajectories == trajectory_ids.len(),
                            "worker counted {reply_trajectories} trajectories for shard {shard_id}, expected {}",
                            trajectory_ids.len()
                        );
                        Ok(FederatedCountPartialReport {
                            shard_id,
                            worker_rank,
                            trajectory_ids,
                            count,
                            payload_bytes,
                            output: PathBuf::new(),
                        })
                    }
                    FederatedAnalysisReply::Failed {
                        shard_id,
                        worker_rank,
                        message,
                    } => bail!(
                        "federated worker {worker_rank} failed shard {shard_id}: {message}"
                    ),
                    FederatedAnalysisReply::Stopped => {
                        bail!("federated worker stopped while processing shard {shard_id}")
                    }
                    FederatedAnalysisReply::Mapped { .. } => {
                        bail!("federated count worker returned a mapper result for shard {shard_id}")
                    }
                }
            }
        },
    ))
    .buffer_unordered(worker_count)
    .try_collect::<Vec<_>>()
    .await?;
    partials.sort_by_key(|partial| partial.shard_id);
    validate_partials(&partials)?;

    let mut total = 0u64;
    for partial in &mut partials {
        total = total
            .checked_add(partial.count)
            .context("federated count overflow")?;
        partial.output = options
            .output_dir
            .join(format!("part-{:05}.jsonl", partial.shard_id));
        let mut row = serde_json::to_vec(&serde_json::json!({
            "table": table,
            "count": partial.count,
            "shard_id": partial.shard_id,
            "worker_rank": partial.worker_rank,
        }))?;
        row.push(b'\n');
        write_bytes_atomic(&partial.output, &row).await?;
    }

    let output = options.output_dir.join("results.jsonl");
    let mut result_row = serde_json::to_vec(&serde_json::json!({
        "table": table,
        "count": total,
    }))?;
    result_row.push(b'\n');
    write_bytes_atomic(&output, &result_row).await?;

    let report = FederatedCountReport {
        schema_version: FEDERATED_COUNT_SCHEMA_VERSION,
        mode: "federated_count".into(),
        table,
        aggregation_levels: 2,
        requested_parallelism: parallelism,
        worker_count,
        shard_count: partials.len(),
        trajectories: trajectories.len(),
        count: total,
        output,
        partials,
    };
    write_json_atomic(&options.output_dir.join("analysis-report.json"), &report).await?;
    Ok(report)
}

fn validate_partials(partials: &[FederatedCountPartialReport]) -> anyhow::Result<()> {
    let mut shard_ids = BTreeSet::new();
    for partial in partials {
        if !shard_ids.insert(partial.shard_id) {
            bail!("duplicate federated partial for shard {}", partial.shard_id);
        }
    }
    for (expected, actual) in shard_ids.into_iter().enumerate() {
        if expected != actual {
            bail!("missing federated partial for shard {expected}");
        }
    }
    Ok(())
}

async fn run_local(options: FederatedCountOptions) -> anyhow::Result<FederatedCountReport> {
    let system: Arc<ActorSystem> = ActorSystem::builder()
        .mailbox_capacity(64)
        .build()
        .await
        .context("build local federated ActorSystem")?;
    let worker_count = options.parallelism.max(1);
    let mut workers = Vec::with_capacity(worker_count);
    for rank in 0..worker_count {
        workers.push(
            system
                .spawn_named(
                    DistEnv::analysis_worker_name(rank),
                    FederatedAnalysisWorker::new(rank, CancellationToken::new()),
                )
                .await
                .with_context(|| format!("spawn local analysis worker {rank}"))?,
        );
    }
    let result = federated_count_with_workers(options, workers.clone()).await;
    stop_workers(&workers).await;
    system
        .shutdown()
        .await
        .map_err(|error| anyhow::anyhow!("shutdown local federated ActorSystem: {error}"))?;
    result
}

async fn run_local_script(options: ProcessScriptOptions) -> anyhow::Result<ProcessScriptReport> {
    let system: Arc<ActorSystem> = ActorSystem::builder()
        .mailbox_capacity(64)
        .build()
        .await
        .context("build local process ActorSystem")?;
    let worker_count = options.mappers.max(1);
    let mut workers = Vec::with_capacity(worker_count);
    for rank in 0..worker_count {
        workers.push(
            system
                .spawn_named(
                    DistEnv::analysis_worker_name(rank),
                    FederatedAnalysisWorker::new(rank, CancellationToken::new()),
                )
                .await
                .with_context(|| format!("spawn local process mapper {rank}"))?,
        );
    }
    let result = process_script_with_workers(options, workers.clone()).await;
    stop_workers(&workers).await;
    system
        .shutdown()
        .await
        .map_err(|error| anyhow::anyhow!("shutdown local process ActorSystem: {error}"))?;
    result
}

async fn run_distributed_driver(
    dist: DistEnv,
    options: FederatedCountOptions,
) -> anyhow::Result<FederatedCountReport> {
    let bind = format!("0.0.0.0:{}", dist.pulsing_seed.port());
    let system: Arc<ActorSystem> = ActorSystem::builder()
        .mailbox_capacity(64)
        .addr(bind.as_str())
        .build()
        .await
        .context("build federated driver ActorSystem")?;
    system
        .spawn_named(
            DistEnv::analysis_worker_name(0),
            FederatedAnalysisWorker::new(0, CancellationToken::new()),
        )
        .await
        .context("spawn driver analysis worker")?;

    let workers = resolve_workers(&system, dist.world_size, Duration::from_secs(120)).await?;
    let result = federated_count_with_workers(options, workers.clone()).await;
    stop_workers(&workers).await;
    system
        .shutdown()
        .await
        .map_err(|error| anyhow::anyhow!("shutdown federated driver ActorSystem: {error}"))?;
    result
}

async fn run_distributed_script_driver(
    dist: DistEnv,
    options: ProcessScriptOptions,
) -> anyhow::Result<ProcessScriptReport> {
    let bind = format!("0.0.0.0:{}", dist.pulsing_seed.port());
    let system: Arc<ActorSystem> = ActorSystem::builder()
        .mailbox_capacity(64)
        .addr(bind.as_str())
        .build()
        .await
        .context("build distributed process ActorSystem")?;
    system
        .spawn_named(
            DistEnv::analysis_worker_name(0),
            FederatedAnalysisWorker::new(0, CancellationToken::new()),
        )
        .await
        .context("spawn driver process mapper")?;
    let workers = resolve_workers(&system, dist.world_size, Duration::from_secs(120)).await?;
    let result = process_script_with_workers(options, workers.clone()).await;
    stop_workers(&workers).await;
    system
        .shutdown()
        .await
        .map_err(|error| anyhow::anyhow!("shutdown distributed process ActorSystem: {error}"))?;
    result
}

async fn run_distributed_worker(dist: DistEnv) -> anyhow::Result<()> {
    let seed = dist.pulsing_seed.to_string();
    let system = join_cluster(&seed).await?;
    let shutdown = CancellationToken::new();
    system
        .spawn_named(
            DistEnv::analysis_worker_name(dist.rank),
            FederatedAnalysisWorker::new(dist.rank, shutdown.clone()),
        )
        .await
        .with_context(|| format!("spawn analysis worker {}", dist.rank))?;
    shutdown.cancelled().await;
    system
        .shutdown()
        .await
        .map_err(|error| anyhow::anyhow!("shutdown federated worker ActorSystem: {error}"))
}

async fn join_cluster(seed: &str) -> anyhow::Result<Arc<ActorSystem>> {
    let mut last_error = None;
    for _ in 0..60 {
        match ActorSystem::builder()
            .mailbox_capacity(64)
            .addr("0.0.0.0:0")
            .seeds([seed])
            .build()
            .await
        {
            Ok(system) => return Ok(system),
            Err(error) => {
                last_error = Some(error.to_string());
                sleep(Duration::from_millis(250)).await;
            }
        }
    }
    bail!("failed to join federated Pulsing cluster at {seed}: {last_error:?}")
}

async fn resolve_workers(
    system: &Arc<ActorSystem>,
    world_size: usize,
    timeout: Duration,
) -> anyhow::Result<Vec<ActorRef>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let mut workers = Vec::with_capacity(world_size);
        for rank in 0..world_size {
            match resolve_actor(system.as_ref(), &DistEnv::analysis_worker_name(rank)).await {
                Ok(worker) => workers.push(worker),
                Err(_) => break,
            }
        }
        if workers.len() == world_size {
            return Ok(workers);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "timed out resolving federated analysis workers: found {}/{}",
                workers.len(),
                world_size
            );
        }
        sleep(Duration::from_millis(100)).await;
    }
}

async fn stop_workers(workers: &[ActorRef]) {
    for worker in workers {
        if let Err(error) = ask_timeout::<_, FederatedAnalysisReply>(
            worker,
            FederatedAnalysisCommand::Shutdown,
            Duration::from_secs(5),
        )
        .await
        {
            tracing::warn!(%error, "failed to stop federated analysis worker");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_contiguous_unique_partials() {
        let partial = |shard_id| FederatedCountPartialReport {
            shard_id,
            worker_rank: 0,
            trajectory_ids: vec![],
            count: 1,
            payload_bytes: 0,
            output: PathBuf::new(),
        };
        assert!(validate_partials(&[partial(0), partial(1)]).is_ok());
        assert!(validate_partials(&[partial(0), partial(0)]).is_err());
        assert!(validate_partials(&[partial(1)]).is_err());
    }

    #[tokio::test]
    async fn worker_executes_all_typed_agent_analysis_metrics() {
        let input = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../persisting-pchronicle/tests/fixtures/atif");
        let documents = persisting_pchronicle::load_atif_trajectories(input).unwrap();
        let payload = serde_json::to_vec(&documents).unwrap();
        let worker = FederatedAnalysisWorker::new(7, CancellationToken::new());
        for (table, expected) in [
            (CountTable::Runs, 8),
            (CountTable::Steps, 118),
            (CountTable::ToolCalls, 23),
            (CountTable::LlmCalls, 52),
            (CountTable::CopiedContextSteps, 19),
        ] {
            let reply = worker.count_shard(0, table, &payload).await.unwrap();
            assert!(matches!(
                reply,
                FederatedAnalysisReply::Partial {
                    worker_rank: 7,
                    count,
                    ..
                } if count == expected
            ));
        }
    }
}
