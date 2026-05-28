//! CLI loads `libpersisting_engine` lazily and calls **`persisting_engine_submit`** / **`job_poll`** /
//! **`job_take_result`**（异步 job + 进度；见 `persisting_proto::invoke_abi`）。

mod capture;
mod trajectory_detail;
mod trajectory_format;
mod trajectory_stdout_toml;

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use libloading::{Library, Symbol};
use persisting_proto::{
    RequestBody, ResponseBody, RpcRequest, RpcResponse, SearchAddBatchRequest, SearchAddRequest,
    SearchImportLanceRequest, SearchIndexDeleteRequest, SearchIndexListRequest,
    SearchIndexRebuildRequest, SearchIndexReorderRequest, SearchIndexRequest, SearchQueryRequest,
    TrajectoryAppendRequest, TrajectoryMaterializeRequest, TrajectoryReplayRequest,
    TrajectoryReplayResponse, TrajectoryStatsRequest, TrajectoryStatsResponse,
    TrajectoryStorageFormat, PROTOCOL_VERSION, RON_ABI_VERSION,
};
use serde::{Deserialize, Serialize};

use persisting_engine::trajectory::{
    merge_traj_location, resolve_traj_read_location, trajectory_run_dir, TrajLocation,
};
use trajectory_detail::{build_detail_node, print_trajectory_stats_detail, SpawnLinkInfo};
use trajectory_format::{TrajectoryAddFormat, TrajectoryFormatManager, TrajectoryStorageCli};
use trajectory_stdout_toml::{
    print_trajectory_append_as_toml, print_trajectory_replay_as_toml,
    print_trajectory_stats_as_toml,
};

type RonAbiVersionFn = unsafe extern "C" fn() -> u32;

fn ron_request_pretty<T: Serialize>(v: &T) -> Result<String> {
    ron::ser::to_string_pretty(
        v,
        ron::ser::PrettyConfig::new().indentor("    ".to_string()),
    )
    .context("encode request RON")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireError {
    error: String,
}

struct Engine {
    lib: Library,
}

/// Resolves path and opens the engine on first call that needs it.
struct LazyEngine<'a> {
    cli: &'a Cli,
    engine: Option<Engine>,
}

impl<'a> LazyEngine<'a> {
    fn new(cli: &'a Cli) -> Self {
        Self { cli, engine: None }
    }

    fn engine_mut(&mut self) -> Result<&Engine> {
        if self.engine.is_none() {
            let path = resolve_engine_path(self.cli.core_lib.as_deref())?;
            self.engine = Some(Engine::load(&path)?);
        }
        Ok(self.engine.as_ref().unwrap())
    }

    /// `payload` must be full `RpcRequest` RON (callers serialize **before** calling so bad rows skip `dlopen`).
    fn invoke_engine_ron(&mut self, payload: &str) -> Result<()> {
        let eng = self.engine_mut()?;
        let out = eng.invoke_engine_ron(payload)?;
        print_engine_ron_response(&out)
    }

    /// 调用引擎并校验响应，不打印（用于大批量 `SearchAdd` 的中间行）。
    fn invoke_engine_ron_silent(&mut self, payload: &str) -> Result<String> {
        let eng = self.engine_mut()?;
        let out = eng.invoke_engine_ron(payload)?;
        parse_engine_ron_response(&out)?;
        Ok(out)
    }
}

impl Engine {
    fn load(path: &Path) -> Result<Self> {
        let lib = unsafe { Library::new(path) }
            .with_context(|| format!("failed to load engine library from {}", path.display()))?;
        let ron_abi: Symbol<RonAbiVersionFn> = unsafe {
            lib.get(b"persisting_engine_ron_abi_version\0")
                .map_err(|e| {
                    anyhow::anyhow!(
                        "engine library missing persisting_engine_ron_abi_version ({}); rebuild persisting-engine",
                        e
                    )
                })?
        };
        let v = unsafe { ron_abi() };
        if v != RON_ABI_VERSION {
            anyhow::bail!(
                "engine RON ABI version {} does not match CLI ({})",
                v,
                RON_ABI_VERSION
            );
        }
        Ok(Self { lib })
    }

    fn invoke_engine_ron(&self, payload: &str) -> Result<String> {
        let submit: Symbol<persisting_proto::PersistingEngineSubmitFn> = unsafe {
            self.lib
                .get(b"persisting_engine_submit\0")
                .with_context(|| {
                    "missing engine export persisting_engine_submit; rebuild persisting-engine"
                })?
        };
        let poll: Symbol<persisting_proto::PersistingEngineJobPollFn> =
            unsafe {
                self.lib.get(b"persisting_engine_job_poll\0").with_context(|| {
                "missing engine export persisting_engine_job_poll; rebuild persisting-engine"
            })?
            };
        let take: Symbol<persisting_proto::PersistingEngineJobTakeResultFn> = unsafe {
            self.lib.get(b"persisting_engine_job_take_result\0").with_context(|| {
                "missing engine export persisting_engine_job_take_result; rebuild persisting-engine"
            })?
        };
        let release: Symbol<persisting_proto::PersistingEngineJobReleaseFn> =
            unsafe {
                self.lib.get(b"persisting_engine_job_release\0").with_context(|| {
                "missing engine export persisting_engine_job_release; rebuild persisting-engine"
            })?
            };
        let syms = persisting_proto::PersistingEngineJobSyms {
            submit: *submit,
            poll: *poll,
            take_result: *take,
            release: *release,
        };
        let raw =
            unsafe { persisting_proto::invoke_ron_utf8_via_jobs_sync(syms, payload.as_bytes())? };
        persisting_proto::response_utf8_to_string(&raw).context("engine response UTF-8")
    }
}

fn parse_engine_ron_response(raw: &str) -> Result<RpcResponse> {
    if let Ok(w) = ron::from_str::<WireError>(raw) {
        anyhow::bail!("{}", w.error);
    }
    let resp: RpcResponse =
        ron::from_str(raw).context("engine returned invalid RON RpcResponse")?;
    if let ResponseBody::Error { message, .. } = &resp.body {
        anyhow::bail!("{}", message);
    }
    Ok(resp)
}

fn print_engine_ron_response(raw: &str) -> Result<()> {
    let resp = parse_engine_ron_response(raw)?;
    match &resp.body {
        // trajectory 成功响应统一用 TOML stdout（与默认写入格式一致）。
        ResponseBody::TrajectoryAppend(tr) => print_trajectory_append_as_toml(tr),
        ResponseBody::TrajectoryStats(tr) => print_trajectory_stats_as_toml(tr),
        ResponseBody::TrajectoryReplay(tr) => print_trajectory_replay_as_toml(tr),
        _ => {
            println!(
                "{}",
                ron::ser::to_string(&resp.body)
                    .map_err(|e| anyhow::anyhow!("RON serialize: {e}"))?
            );
            Ok(())
        }
    }
}

/// 多行 JSONL/CSV：按批 `SearchAddBatch` 写入 Lance（每批一次 `InsertBuilder`，远快于逐行 `SearchAdd`）。
fn search_add_batch(lazy: &mut LazyEngine<'_>, mut rows: Vec<SearchAddRequest>) -> Result<()> {
    const CHUNK: usize = 256;
    let total = rows.len();
    if total == 0 {
        anyhow::bail!("import contained no rows");
    }
    let dataset = rows[0].dataset.clone();
    let mut processed = 0usize;
    while !rows.is_empty() {
        let n = CHUNK.min(rows.len());
        let chunk: Vec<SearchAddRequest> = rows.drain(0..n).collect();
        let payload = rpc_request_pretty(RequestBody::SearchAddBatch(SearchAddBatchRequest {
            rows: chunk,
        }))
        .with_context(|| {
            format!(
                "encode SearchAddBatch RON (rows {}..={})",
                processed + 1,
                processed + n
            )
        })?;
        let is_last = rows.is_empty();
        if is_last {
            lazy.invoke_engine_ron(&payload).with_context(|| {
                format!(
                    "SearchAddBatch final chunk (through row {}/{})",
                    processed + n,
                    total
                )
            })?;
        } else {
            lazy.invoke_engine_ron_silent(&payload).with_context(|| {
                format!(
                    "SearchAddBatch rows {}..={} of {}",
                    processed + 1,
                    processed + n,
                    total
                )
            })?;
        }
        processed += n;
        eprintln!("[persisting-cli] search create: {processed}/{total} rows -> {dataset}");
    }
    eprintln!("[persisting-cli] search create: done {total} rows -> {dataset}");
    Ok(())
}

fn rpc_request_pretty(body: RequestBody) -> Result<String> {
    ron_request_pretty(&RpcRequest {
        version: PROTOCOL_VERSION,
        body,
    })
}

#[derive(Debug, Parser)]
#[command(
    name = "persisting",
    version,
    about = "Agent memory, search, and trajectory storage (engine .so loaded lazily; RON C ABI)"
)]
struct Cli {
    /// Path to `libpersisting_engine` dynamic library (`.dylib`, `.so`, or `.dll`).
    #[arg(long, env = "PERSISTING_ENGINE_LIB")]
    core_lib: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Search(SearchArgs),
    /// Agent trajectory storage (append, replay, stats)
    Trajectory(TrajectoryArgs),
    /// Short alias for `trajectory`
    #[command(name = "traj")]
    Traj(TrajectoryArgs),
    Capture(CaptureArgs),
}

#[derive(Debug, Args)]
struct CaptureArgs {
    #[command(subcommand)]
    command: CaptureCommand,
}

#[derive(Debug, Subcommand)]
enum CaptureCommand {
    /// Merge IDE / agentgateway events into one trajectory session.
    Import(CaptureImportArgs),
    /// Start capture daemon (LLM proxy + trajectory append).
    Start(CaptureStartArgs),
    /// Stop capture daemon for this storage root.
    Stop(CaptureStopArgs),
    /// List recorded sessions with usage and estimated cost.
    List(CaptureListArgs),
    /// Query running daemon (active connections + sessions).
    Status(CaptureStatusArgs),
    /// Run proxy in foreground (same as `start --foreground`).
    Serve(CaptureServeArgs),
    /// Set proxy env vars and run a command (in-process LLM proxy, no forked daemon).
    Run(CaptureRunArgs),
    /// Re-apply events from `{output_dir}/.capture/dead_letter.jsonl`.
    ReplayDeadLetter(CaptureReplayDeadLetterArgs),
}

#[derive(Debug, Args)]
struct CaptureRunArgs {
    /// Trajectory output directory (default: `.persisting/capture`).
    #[arg(
        long,
        short = 'o',
        value_name = "DIR",
        default_value = ".persisting/capture"
    )]
    output_dir: String,
    /// Proxy config TOML (`listen`, `models`, …).
    #[arg(long, short = 'c', value_name = "FILE")]
    config: PathBuf,
    /// Log every proxied / captured HTTP request to stderr and `{output_dir}/.capture/debug.log`.
    #[arg(long)]
    debug: bool,
    /// Storage format: `md` (0001.md) or `bin` (Lance).
    #[arg(long, short = 'f', value_enum, default_value_t = capture::CaptureFormat::Markdown)]
    format: capture::CaptureFormat,
    /// Command and arguments to execute (after `--`).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

#[derive(Debug, Args)]
struct CaptureReplayDeadLetterArgs {
    #[arg(
        long,
        short = 'o',
        value_name = "DIR",
        default_value = ".persisting/capture"
    )]
    output_dir: String,
    #[arg(long, short = 'f', value_enum, default_value_t = capture::CaptureFormat::Markdown)]
    format: capture::CaptureFormat,
}

#[derive(Debug, Args)]
struct CaptureStartArgs {
    #[arg(long, short = 'o', value_name = "DIR")]
    output_dir: String,
    #[arg(long, short = 'c', value_name = "FILE")]
    config: PathBuf,
    #[arg(long)]
    foreground: bool,
    #[arg(long)]
    debug: bool,
    #[arg(long, short = 'f', value_enum, default_value_t = capture::CaptureFormat::Markdown)]
    format: capture::CaptureFormat,
}

#[derive(Debug, Args)]
struct CaptureStopArgs {
    /// Trajectory output directory (default: last `capture start` or `PERSISTING_CAPTURE_STORAGE`).
    #[arg(long, short = 'o', value_name = "DIR")]
    output_dir: Option<String>,
}

#[derive(Debug, Args)]
struct CaptureListArgs {
    #[arg(long, short = 'o', value_name = "DIR")]
    output_dir: Option<String>,
}

#[derive(Debug, Args)]
struct CaptureStatusArgs {
    #[arg(long, short = 'o', value_name = "DIR")]
    output_dir: Option<String>,
}

#[derive(Debug, Args)]
struct CaptureServeArgs {
    #[arg(long, short = 'o', value_name = "DIR")]
    output_dir: String,
    #[arg(long, short = 'c', value_name = "FILE")]
    config: PathBuf,
    #[arg(long)]
    debug: bool,
    #[arg(long, short = 'f', value_enum, default_value_t = capture::CaptureFormat::Markdown)]
    format: capture::CaptureFormat,
}

#[derive(Debug, Args)]
struct CaptureImportArgs {
    /// Trajectory root or session directory (`{storage}/{agent_id}/{session_id}/`).
    #[arg(value_name = "STORAGE")]
    storage: String,
    /// `ide` = Claude + Cursor JSONL; `gateway` = OTLP/envelope JSONL; `all` = both.
    #[arg(long, value_enum, default_value_t = capture::CaptureProvider::Ide)]
    provider: capture::CaptureProvider,
    /// Only include files modified within the last N days.
    #[arg(long, default_value_t = 30)]
    since_days: u64,
    /// Substring match on encoded project dir (default: current working directory).
    #[arg(long)]
    project: Option<String>,
    /// Do not filter by project; scan all projects under `~/.claude` / `~/.cursor`.
    #[arg(long)]
    all_projects: bool,
    /// Import a single session (required when multiple sessions match).
    #[arg(long, value_name = "SEG")]
    session_id: Option<String>,
    /// Trajectory `agent_id` segment (default: `--project` slug or `capture`).
    #[arg(long, value_name = "SEG")]
    agent_id: Option<String>,
    /// Include `subagents/*.jsonl` and merge by timestamp.
    #[arg(long, default_value_t = true)]
    merge_subagents: bool,
    /// agentgateway export JSONL (`-` = stdin). Required for `gateway` / `all`.
    #[arg(long, default_value = "-")]
    gateway_input: String,
    /// Print counts only; do not call the engine.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct SearchArgs {
    #[command(subcommand)]
    command: SearchCommand,
}

#[derive(Debug, Subcommand)]
enum SearchCommand {
    /// Import rows from JSONL, CSV, or an existing Lance dataset (`--input` required; stdin needs `--format jsonl|csv`).
    Create(SearchCreateArgs),
    /// Index maintenance: `list` / `delete` / `rebuild` / `build` (IVF-PQ + FTS), `reorder` (IVF layout via lance-tools).
    Index(SearchIndexMaintenanceArgs),
    Query(SearchQueryArgs),
}

#[derive(Debug, Args)]
struct SearchCreateArgs {
    /// Target Lance dataset path or URI root.
    #[arg(value_name = "DATASET")]
    dataset: String,
    /// File path or `-` for stdin.
    #[arg(long)]
    input: String,
    /// `auto`: infer from path (see help). Stdin `-` must use `jsonl` or `csv` (not `lance`).
    #[arg(long, value_enum, default_value_t = ImportFormat::Auto)]
    format: ImportFormat,
    #[arg(long, default_value_t = 384)]
    embedding_dim: usize,
    /// Text column on the **source** Lance table (`--format lance`; default `text`).
    #[arg(long, default_value = "text")]
    lance_text_column: String,
    /// Optional id column on the source Lance table (must be Utf8 / LargeUtf8 for now).
    #[arg(long)]
    lance_id_column: Option<String>,
    /// Optional cap on rows to import in a future implementation (reported in RPC for now).
    #[arg(long)]
    import_limit: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum ImportFormat {
    #[default]
    Auto,
    Jsonl,
    Csv,
    /// Existing Lance dataset directory (contains `data.lance/`) or path understood by `Dataset::open`.
    Lance,
}

#[derive(Debug, Args)]
struct SearchIndexMaintenanceArgs {
    #[command(subcommand)]
    command: SearchIndexCommand,
}

#[derive(Debug, Subcommand)]
enum SearchIndexCommand {
    /// List index segments (excludes Lance system indices).
    List(SearchIndexListArgs),
    Build(SearchIndexBuildArgs),
    /// Drop an index by logical name (`DatasetIndexExt::drop_index`).
    Delete(SearchIndexDeleteArgs),
    /// Merge / retrain index segments (`DatasetIndexExt::optimize_indices`).
    Rebuild(SearchIndexRebuildArgs),
    Reorder(SearchReorderArgs),
}

#[derive(Debug, Args)]
struct SearchIndexListArgs {
    #[arg(value_name = "DATASET")]
    dataset: String,
}

#[derive(Debug, Args)]
struct SearchIndexDeleteArgs {
    #[arg(value_name = "DATASET")]
    dataset: String,
    #[arg(value_name = "INDEX_NAME")]
    index_name: String,
}

#[derive(Debug, Args)]
struct SearchIndexRebuildArgs {
    #[arg(value_name = "DATASET")]
    dataset: String,
    /// When omitted, all non-system indices are considered (Lance `OptimizeOptions::index_names = None`).
    #[arg(long)]
    index_name: Option<String>,
    /// Use merge-style optimize instead of full retrain (v3 vector `retrain` in Lance).
    #[arg(long, action = clap::ArgAction::SetTrue)]
    no_retrain: bool,
    /// When `--no-retrain` is set: passed to `OptimizeOptions::num_indices_to_merge`.
    #[arg(long)]
    merge_num_indices: Option<usize>,
}

#[derive(Debug, Args)]
struct SearchIndexBuildArgs {
    #[arg(value_name = "DATASET")]
    dataset: String,
    #[arg(long, default_value = "embedding")]
    vector_column: String,
    #[arg(long, default_value = "text")]
    text_column: String,
    #[arg(long, default_value = "cosine")]
    metric: String,
    /// IVF partition count (maps to Lance `IvfBuildParams::num_partitions`).
    #[arg(long, value_name = "N")]
    num_partitions: Option<usize>,
    /// Max k-means iterations for IVF centroid training (Lance `IvfBuildParams::max_iters`, default 50).
    #[arg(long, value_name = "N")]
    ivf_max_iters: Option<usize>,
    /// IVF k-means balance loss weight (`IvfBuildParams::balance_factor`; `lance-tools reorder` uses 0.0).
    #[arg(long)]
    ivf_balance_factor: Option<f32>,
    /// Enable IVF oversized-cluster split postprocess (same idea as `lance-tools index reorder --ivf-balance-postprocess`).
    #[arg(long = "ivf-balance-postprocess", action = clap::ArgAction::SetTrue)]
    ivf_balance_postprocess: bool,
    /// Postprocess cluster size ratio threshold (reorder default 2.5 when postprocess is on).
    #[arg(long)]
    ivf_postprocess_max_cluster_ratio: Option<f32>,
    /// IVF training sample rate (`IvfBuildParams::sample_rate`, Lance default 256).
    #[arg(long)]
    ivf_sample_rate: Option<usize>,
    /// Target rows per IVF partition; if set, partition count is derived (`IvfBuildParams::target_partition_size`).
    #[arg(long)]
    ivf_target_partition_size: Option<usize>,
    /// IVF shuffle rows per batch (`IvfBuildParams::shuffle_partition_batches`, Lance default large).
    #[arg(long)]
    ivf_shuffle_partition_batches: Option<usize>,
    /// IVF shuffle task concurrency (`IvfBuildParams::shuffle_partition_concurrency`).
    #[arg(long)]
    ivf_shuffle_partition_concurrency: Option<usize>,
    /// PQ sub-vector count (`PQBuildParams::num_sub_vectors`, Lance default 16).
    #[arg(long, value_name = "M")]
    pq_num_sub_vectors: Option<usize>,
    /// Bits per PQ code (`PQBuildParams::num_bits`, Lance default 8).
    #[arg(long, value_name = "BITS")]
    pq_num_bits: Option<u8>,
    /// Max k-means iterations for PQ codebook (`PQBuildParams::max_iters`; reorder uses 50).
    #[arg(long, value_name = "N")]
    pq_max_iters: Option<usize>,
    /// PQ k-means restarts (`PQBuildParams::kmeans_redos`, default 1).
    #[arg(long)]
    pq_kmeans_redos: Option<usize>,
    /// PQ codebook training sample rate (`PQBuildParams::sample_rate`, default 256).
    #[arg(long)]
    pq_sample_rate: Option<usize>,
}

#[derive(Debug, Args)]
struct SearchQueryArgs {
    #[arg(value_name = "DATASET")]
    dataset: String,
    #[arg(value_name = "QUERY")]
    query: String,
    #[arg(long, value_enum, default_value_t = SearchMode::Hybrid)]
    mode: SearchMode,
    #[arg(long, default_value_t = 10)]
    k: usize,
    #[arg(long, default_value_t = 384)]
    embedding_dim: usize,
    /// FTS / hybrid：全文检索列名（须已建 inverted / FTS 索引，见 `search index build`）。
    #[arg(long, default_value = "text")]
    text_column: String,
    #[arg(long)]
    filter: Option<String>,
    #[arg(long)]
    nprobes: Option<usize>,
    #[arg(long)]
    minimum_nprobes: Option<usize>,
    #[arg(long)]
    maximum_nprobes: Option<usize>,
    #[arg(long)]
    adaptive_nprobes_margin: Option<f32>,
}

#[derive(Clone, Debug, ValueEnum)]
enum SearchMode {
    Vector,
    Fts,
    Hybrid,
}

impl std::fmt::Display for SearchMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SearchMode::Vector => "vector",
            SearchMode::Fts => "fts",
            SearchMode::Hybrid => "hybrid",
        })
    }
}

#[derive(Debug, Args)]
struct SearchReorderArgs {
    #[arg(value_name = "DATASET")]
    dataset: String,
    #[arg(value_name = "PIVOT_INDEX")]
    pivot_index: String,
    #[arg(long)]
    target: Option<String>,
    #[arg(long)]
    in_place: bool,
}

#[derive(Debug, Args)]
struct TrajectoryArgs {
    #[command(subcommand)]
    command: TrajectoryCommand,
}

#[derive(Debug, Subcommand)]
enum TrajectoryCommand {
    Add(TrajectoryAddArgs),
    Replay(TrajectoryReplayArgs),
    Stats(TrajectoryStatsArgs),
    /// Lance raw log → TLV Markdown (lossy materialized view).
    Materialize(TrajectoryMaterializeArgs),
}

#[derive(Debug, Args)]
struct TrajectoryAddArgs {
    /// Root directory for trajectory datasets (parent of `agent_id/session_id/` Lance dirs).
    #[arg(value_name = "STORAGE")]
    storage: String,
    /// Agent identity（单层路径段；省略则自动生成并在 stderr 打印）。
    #[arg(long, value_name = "SEG")]
    agent_id: Option<String>,
    /// Session / run id（单层路径段；省略则自动生成并在 stderr 打印）。
    #[arg(long, value_name = "SEG")]
    session_id: Option<String>,
    /// 输入格式；`auto` 时按 `--input` 文件名推断（`0001.md` → markdown，`.jsonl` → jsonl，…）。
    #[arg(long, value_enum, default_value_t = TrajectoryAddFormat::Auto)]
    format: TrajectoryAddFormat,
    #[arg(long, default_value = "-")]
    input: String,
    /// 存储后端；`auto` 时按 `--input` 文件名推断（`0001.md` → markdown，`.jsonl`/`.toml` → lance）。
    #[arg(long, value_enum, default_value_t = TrajectoryStorageCli::Auto)]
    storage_format: TrajectoryStorageCli,
}

#[derive(Debug, Args)]
struct TrajectoryReplayArgs {
    /// Storage root or session directory (`{storage}/{agent_id}/{session_id}/`).
    #[arg(value_name = "STORAGE")]
    storage: String,
    /// 须与 `trajectory add` 写入时一致（add 若自动生成，见当时 stderr）。
    #[arg(long, value_name = "SEG")]
    agent_id: Option<String>,
    #[arg(long, value_name = "SEG")]
    session_id: Option<String>,
    /// 嵌套 subagent session 时指定父 session（路径 `{root}/subagents/{session_id}/`）。
    #[arg(long, value_name = "SEG")]
    root_session_id: Option<String>,
    #[arg(long, default_value_t = 0)]
    offset: usize,
    #[arg(long)]
    limit: Option<usize>,
    #[arg(long, value_enum, default_value_t = TrajectoryStorageCli::Auto)]
    storage_format: TrajectoryStorageCli,
}

#[derive(Debug, Args)]
struct TrajectoryStatsArgs {
    /// Storage root or session directory (`{storage}/{agent_id}/{session_id}/`).
    #[arg(value_name = "STORAGE")]
    storage: String,
    #[arg(long, value_name = "SEG")]
    agent_id: Option<String>,
    #[arg(long, value_name = "SEG")]
    session_id: Option<String>,
    /// 嵌套 subagent session 时指定父 session（路径 `{root}/subagents/{session_id}/`）。
    #[arg(long, value_name = "SEG")]
    root_session_id: Option<String>,
    #[arg(long, value_enum, default_value_t = TrajectoryStorageCli::Auto)]
    storage_format: TrajectoryStorageCli,
    /// 逐轮一行摘要：用户/模型字符数、TTFT、TPOT（stdout 纯文本，非 TOML）。
    #[arg(long)]
    detail: bool,
}

#[derive(Debug, Args)]
struct TrajectoryMaterializeArgs {
    #[arg(value_name = "STORAGE")]
    storage: String,
    #[arg(long, value_name = "SEG")]
    agent_id: Option<String>,
    #[arg(long, value_name = "SEG")]
    session_id: Option<String>,
    #[arg(long, value_name = "SEG")]
    root_session_id: Option<String>,
}

static TRAJ_AUTO_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 生成单层路径段（仅小写十六进制与连字符，不含 `/` `\`）。
fn auto_traj_segment() -> String {
    let ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let c = TRAJ_AUTO_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("auto-{ns:x}-{c:x}")
}

fn resolve_traj_ids_for_write(
    agent_id: Option<String>,
    session_id: Option<String>,
) -> Result<(String, String)> {
    let agent = agent_id.unwrap_or_else(auto_traj_segment);
    let session = session_id.unwrap_or_else(auto_traj_segment);
    validate_traj_segment(&agent)?;
    validate_traj_segment(&session)?;
    Ok((agent, session))
}

/// 校验路径段不含分隔符，防止目录穿越。
fn validate_traj_segment(s: &str) -> Result<()> {
    if s.contains('/') || s.contains('\\') || s.contains("..") {
        return Err(anyhow::anyhow!(
            "trajectory id must not contain path separators or '..': got {s:?}"
        ));
    }
    Ok(())
}

fn resolve_traj_ids_for_read(
    op: &str,
    path_arg: String,
    agent_id: Option<String>,
    session_id: Option<String>,
    root_session_id: Option<String>,
) -> Result<TrajLocation> {
    resolve_traj_read_location(op, path_arg, agent_id, session_id, root_session_id)
}

fn resolve_engine_path(core_lib: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = core_lib {
        return Ok(p.to_path_buf());
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let names = engine_lib_names();
    if let Some(dir) = exe_dir {
        for name in &names {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    anyhow::bail!(
        "set --core-lib or PERSISTING_ENGINE_LIB to the path of libpersisting_engine (e.g. target/debug/libpersisting_engine.dylib)"
    )
}

fn engine_lib_names() -> [&'static str; 3] {
    #[cfg(target_os = "macos")]
    {
        [
            "libpersisting_engine.dylib",
            "libpersisting_engine.so",
            "persisting_engine.dll",
        ]
    }
    #[cfg(target_os = "linux")]
    {
        [
            "libpersisting_engine.so",
            "libpersisting_engine.dylib",
            "persisting_engine.dll",
        ]
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "linux")))]
    {
        [
            "persisting_engine.dll",
            "libpersisting_engine.so",
            "libpersisting_engine.dylib",
        ]
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut lazy = LazyEngine::new(&cli);
    match &cli.command {
        Command::Search(args) => run_search(&mut lazy, args)?,
        Command::Trajectory(args) | Command::Traj(args) => run_trajectory(&mut lazy, args)?,
        Command::Capture(args) => run_capture(&mut lazy, args)?,
    }
    Ok(())
}

fn run_capture(lazy: &mut LazyEngine<'_>, args: &CaptureArgs) -> Result<()> {
    match &args.command {
        CaptureCommand::Start(args) => {
            if args.foreground {
                run_capture_serve(
                    lazy,
                    &CaptureServeArgs {
                        output_dir: args.output_dir.clone(),
                        config: args.config.clone(),
                        debug: args.debug,
                        format: args.format,
                    },
                )?;
            } else {
                capture::daemon::cmd_start(capture::daemon::StartOptions {
                    output_dir: PathBuf::from(&args.output_dir),
                    config: args.config.clone(),
                    debug: args.debug,
                    format: args.format,
                })?;
            }
        }
        CaptureCommand::Stop(args) => {
            let storage = args.output_dir.as_deref().map(Path::new);
            capture::daemon::cmd_stop(storage)?;
        }
        CaptureCommand::List(args) => {
            let storage = args.output_dir.as_deref().map(Path::new);
            let sessions = capture::daemon::cmd_list(storage)?;
            capture::daemon::print_list_table(&sessions);
        }
        CaptureCommand::Status(args) => {
            let storage = args.output_dir.as_deref().map(Path::new);
            capture::daemon::cmd_status(storage)?;
        }
        CaptureCommand::Serve(args) => run_capture_serve(lazy, args)?,
        CaptureCommand::Run(args) => {
            let code = run_capture_run(lazy, args)?;
            std::process::exit(code);
        }
        CaptureCommand::ReplayDeadLetter(args) => {
            run_replay_dead_letter(lazy, args)?;
        }
        CaptureCommand::Import(args) => {
            let merged = merge_traj_location(
                args.storage.clone(),
                args.agent_id.clone(),
                args.session_id.clone(),
                None,
            );
            let gateway_input = match args.provider {
                capture::CaptureProvider::Ide => None,
                capture::CaptureProvider::Gateway | capture::CaptureProvider::All => {
                    Some(args.gateway_input.clone())
                }
            };
            let opts = capture::CaptureImportOptions {
                providers: args.provider,
                since_days: args.since_days,
                project_filter: args.project.clone(),
                all_projects: args.all_projects,
                session_id: merged.session_id,
                agent_id: merged.agent_id,
                merge_subagents: args.merge_subagents,
                gateway_input,
                dry_run: args.dry_run,
            };
            let summary = capture::import_to_trajectory_with_engine(
                &merged.storage,
                &opts,
                |storage, agent_id, session_id, records_ronl| {
                    eprintln!(
                        "[persisting-cli] capture import: {record_count} records -> {storage}/{agent_id}/{session_id}",
                        record_count = records_ronl.lines().filter(|l| !l.trim().is_empty()).count(),
                        storage = storage,
                        agent_id = agent_id,
                        session_id = session_id,
                    );
                    let payload = rpc_request_pretty(RequestBody::TrajectoryAppend(
                        TrajectoryAppendRequest {
                            storage: storage.to_string(),
                            agent_id: agent_id.to_string(),
                            session_id: session_id.to_string(),
                            root_session_id: None,
                            records_ronl: records_ronl.to_string(),
                            storage_format: TrajectoryStorageFormat::Auto,
                        },
                    ))
                    .context("encode TrajectoryAppend RpcRequest RON")?;
                    lazy.invoke_engine_ron(&payload)
                },
            )?;
            print_capture_summary(&summary, args.dry_run);
        }
    }
    Ok(())
}

struct TrajectoryAppendJob {
    storage: String,
    agent_id: String,
    session_id: String,
    root_session_id: Option<String>,
    record: persisting_capture::record::CaptureRecord,
}

#[derive(Hash, Eq, PartialEq, Clone)]
struct TrajectoryBatchKey {
    storage: String,
    agent_id: String,
    session_id: String,
    root_session_id: Option<String>,
}

const CAPTURE_TRAJECTORY_BATCH: usize = 32;

fn should_flush_capture_record(record: &persisting_capture::record::CaptureRecord) -> bool {
    matches!(
        record.kind.as_str(),
        "llm.request"
            | "llm.response"
            | "llm.response.stream"
            | "llm.spawn_link"
            | "session.started"
            | "session.ended"
    )
}

fn records_ronl_from_lines(lines: &[String]) -> String {
    if lines.len() == 1 {
        format!("{}\n", lines[0])
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

fn write_lance_dead_letter(key: &TrajectoryBatchKey, lines: &[String], error: &str) {
    let storage_path = std::path::Path::new(&key.storage);
    let records_ronl = records_ronl_from_lines(lines);
    if let Err(dl) = persisting_capture::dead_letter::append_lance_dead_letter(
        storage_path,
        &key.agent_id,
        &key.session_id,
        key.root_session_id.as_deref(),
        &records_ronl,
        error,
    ) {
        eprintln!("[persisting-cli] lance dead letter write failed: {dl:#}");
    }
}

fn flush_capture_trajectory_batch_or_dead_letter(
    engine: &Engine,
    key: &TrajectoryBatchKey,
    lines: &[String],
    stream_markdown: bool,
) {
    if lines.is_empty() {
        return;
    }
    if let Err(e) = flush_capture_trajectory_batch(engine, key, lines, stream_markdown) {
        write_lance_dead_letter(key, lines, &format!("{e:#}"));
        eprintln!("[persisting-cli] capture trajectory append failed: {e:#}");
    }
}

fn flush_capture_trajectory_batch(
    engine: &Engine,
    key: &TrajectoryBatchKey,
    lines: &[String],
    stream_markdown: bool,
) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let records_ronl = records_ronl_from_lines(lines);
    let payload = rpc_request_pretty(RequestBody::TrajectoryAppend(TrajectoryAppendRequest {
        storage: key.storage.clone(),
        agent_id: key.agent_id.clone(),
        session_id: key.session_id.clone(),
        root_session_id: key.root_session_id.clone(),
        records_ronl,
        storage_format: TrajectoryStorageFormat::Lance,
    }))?;
    let raw = engine.invoke_engine_ron(&payload)?;
    parse_engine_ron_response(&raw)?;
    if stream_markdown {
        let run_dir = trajectory_run_dir(
            &key.storage,
            &key.agent_id,
            &key.session_id,
            key.root_session_id.as_deref(),
        )?;
        let _ = persisting_capture::trajectory_convert::stream_engine_lines_to_markdown(
            &run_dir,
            &key.session_id,
            lines,
        )
        .with_context(|| {
            format!(
                "stream markdown to {}",
                persisting_capture::trajectory_convert::materialize_markdown_path(
                    &run_dir,
                    &key.session_id
                )
                .display()
            )
        })?;
    }
    Ok(())
}

fn build_capture_trajectory_sink(
    core_lib: Option<PathBuf>,
    storage: String,
    agent_id: String,
    format: capture::CaptureFormat,
) -> Result<(
    std::sync::Arc<persisting_capture::sink::CallbackSink>,
    TrajectoryAppendWorker,
)> {
    let _ = format.stream_markdown_in_engine();
    let engine_path = resolve_engine_path(core_lib.as_deref())?;
    let storage = std::path::PathBuf::from(&storage)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&storage))
        .display()
        .to_string();
    let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<TrajectoryAppendJob>(256);
    let job_tx = Arc::new(job_tx);
    let tx = Arc::clone(&job_tx);

    let join = std::thread::spawn(move || {
        use std::collections::HashMap;

        let engine = match Engine::load(&engine_path) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("[persisting-cli] capture trajectory engine load failed: {e:#}");
                return;
            }
        };
        let mut batches: HashMap<TrajectoryBatchKey, Vec<String>> = HashMap::new();

        while let Ok(job) = job_rx.recv() {
            let result = (|| -> Result<(), anyhow::Error> {
                let key = TrajectoryBatchKey {
                    storage: job.storage.clone(),
                    agent_id: job.agent_id,
                    session_id: job.session_id,
                    root_session_id: job.root_session_id,
                };
                let line = persisting_capture::record::record_to_engine_line(&job.record)?;
                let flush_now = should_flush_capture_record(&job.record);
                let batch = batches.entry(key.clone()).or_default();
                batch.push(line);
                if batch.len() >= CAPTURE_TRAJECTORY_BATCH || flush_now {
                    let lines = batches.remove(&key).unwrap_or_default();
                    flush_capture_trajectory_batch_or_dead_letter(&engine, &key, &lines, false);
                }
                Ok(())
            })();
            if let Err(e) = result {
                eprintln!("[persisting-cli] capture trajectory append failed: {e:#}");
            }
        }

        for (key, lines) in batches {
            flush_capture_trajectory_batch_or_dead_letter(&engine, &key, &lines, false);
        }
    });

    let sink_storage = storage;
    let sink = std::sync::Arc::new(persisting_capture::sink::CallbackSink::new(
        agent_id,
        move |route, agent_id, record| {
            tx.send(TrajectoryAppendJob {
                storage: sink_storage.clone(),
                agent_id: agent_id.to_string(),
                session_id: route.storage_session_id.clone(),
                root_session_id: route.append_root_session(),
                record,
            })
            .map_err(|e| anyhow::anyhow!("engine append channel closed: {e}"))?;
            Ok(())
        },
    ));
    Ok((
        sink,
        TrajectoryAppendWorker {
            job_tx: Some(job_tx),
            join: Some(join),
        },
    ))
}

fn run_capture_run(lazy: &mut LazyEngine<'_>, args: &CaptureRunArgs) -> Result<i32> {
    let storage = PathBuf::from(&args.output_dir);
    let storage = storage.canonicalize().unwrap_or(storage);
    let config = persisting_capture::config::ProxyConfig::from_file(&args.config)
        .with_context(|| format!("load proxy config {}", args.config.display()))?;
    let agent_id = config.agent_id.clone();
    let (sink, mut worker) = build_capture_trajectory_sink(
        lazy.cli.core_lib.clone(),
        storage.display().to_string(),
        agent_id.clone(),
        args.format,
    )?;
    let code = capture::cmd_run(capture::RunOptions {
        output_dir: storage.clone(),
        config: args.config.clone(),
        command: args.command.clone(),
        debug: args.debug,
        format: args.format,
        sink,
    })?;
    worker.shutdown();
    if args.format.stream_markdown_in_engine() {
        if let Err(e) =
            capture::reconcile::reconcile_run_after_flush(&storage, &agent_id, args.format, |req| {
                invoke_trajectory_replay(lazy, req)
            })
        {
            eprintln!("[persisting-cli] capture reconcile failed: {e:#}");
        }
    }
    Ok(code)
}

fn load_storage_agent_id(storage: &Path) -> String {
    for name in ["proxy.toml", "proxy.yaml"] {
        let path = storage.join(name);
        if path.is_file() {
            if let Ok(cfg) = persisting_capture::config::ProxyConfig::from_file(&path) {
                return cfg.agent_id;
            }
        }
    }
    if let Ok(Some(state)) = persisting_capture::runtime::service::CaptureDaemonState::read(storage)
    {
        if let Ok(cfg) =
            persisting_capture::config::ProxyConfig::from_file(Path::new(&state.config_path))
        {
            return cfg.agent_id;
        }
    }
    "capture".into()
}

fn run_replay_dead_letter(
    lazy: &mut LazyEngine<'_>,
    args: &CaptureReplayDeadLetterArgs,
) -> Result<()> {
    let storage = PathBuf::from(&args.output_dir);
    let storage = storage.canonicalize().unwrap_or(storage);
    let agent_id = load_storage_agent_id(&storage);
    let (sink, mut worker) = build_capture_trajectory_sink(
        lazy.cli.core_lib.clone(),
        storage.display().to_string(),
        agent_id,
        args.format,
    )?;
    capture::replay_dead_letter::cmd_replay_dead_letter(
        capture::replay_dead_letter::ReplayDeadLetterOptions {
            output_dir: storage,
            format: args.format,
            sink,
        },
    )?;
    worker.shutdown();
    Ok(())
}

struct TrajectoryAppendWorker {
    job_tx: Option<Arc<std::sync::mpsc::SyncSender<TrajectoryAppendJob>>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl TrajectoryAppendWorker {
    fn shutdown(&mut self) {
        if let Some(tx) = self.job_tx.take() {
            drop(tx);
        }
        if let Some(j) = self.join.take() {
            if let Err(e) = j.join() {
                eprintln!("[persisting-cli] capture trajectory worker panicked: {e:?}");
            }
        }
    }
}

impl Drop for TrajectoryAppendWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_capture_serve(lazy: &mut LazyEngine<'_>, args: &CaptureServeArgs) -> Result<()> {
    let storage_path = PathBuf::from(&args.output_dir);
    let _run_session =
        persisting_capture::runtime::run_env::ensure_serve_run_session(&storage_path)
            .with_context(|| format!("ensure serve run_session for {}", storage_path.display()))?;
    let applied = persisting_capture::runtime::run_env::apply_daemon_env(&storage_path)
        .with_context(|| format!("apply daemon env snapshot for {}", storage_path.display()))?;
    if !applied.is_empty() {
        eprintln!(
            "[persisting-cli] capture serve: applied daemon env snapshot ({} keys: {})",
            applied.len(),
            applied.join(", ")
        );
    }

    let config = persisting_capture::config::ProxyConfig::from_file(&args.config)
        .with_context(|| format!("load proxy config {}", args.config.display()))?;

    capture::enable_capture_debug(
        &capture::CaptureDebugContext {
            storage: &storage_path,
            applied_env_keys: &applied,
        },
        args.debug,
    )?;

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let (sink, mut worker) = build_capture_trajectory_sink(
        lazy.cli.core_lib.clone(),
        args.output_dir.clone(),
        config.agent_id.clone(),
        args.format,
    )?;

    let rt = tokio::runtime::Runtime::new().context("tokio runtime")?;
    rt.block_on(persisting_capture::proxy::serve(
        config,
        &args.output_dir,
        sink,
        args.format.stream_markdown_in_engine(),
    ))?;
    worker.shutdown();
    Ok(())
}

fn print_capture_summary(summary: &capture::CaptureImportSummary, dry_run: bool) {
    let mode = if dry_run { "dry-run" } else { "imported" };
    eprintln!(
        "[persisting-cli] capture {mode}: {} records @ {} (agent_id={} session_id={})",
        summary.record_count, summary.storage, summary.agent_id, summary.session_id
    );
    for (src, n) in &summary.sources {
        eprintln!("[persisting-cli] capture   source {src}: {n}");
    }
}

fn run_search(lazy: &mut LazyEngine<'_>, args: &SearchArgs) -> Result<()> {
    match &args.command {
        SearchCommand::Create(args) => {
            let fmt = resolve_import_format(&args.input, args.format)?;
            match fmt {
                ImportFormat::Lance => {
                    if args.input.trim() == "-" {
                        anyhow::bail!(
                            "Lance import cannot use stdin; pass a Lance dataset path as --input"
                        );
                    }
                    let payload = rpc_request_pretty(RequestBody::SearchImportLance(
                        SearchImportLanceRequest {
                            target_dataset: args.dataset.clone(),
                            source_lance: args.input.clone(),
                            source_text_column: args.lance_text_column.clone(),
                            source_id_column: args.lance_id_column.clone(),
                            embedding_dim: args.embedding_dim,
                            limit: args.import_limit,
                        },
                    ))
                    .context("encode SearchImportLance RpcRequest RON")?;
                    eprintln!(
                        "[persisting-cli] search create: Lance import from {:?} -> dataset {:?} (engine may take a while)…",
                        args.input, args.dataset
                    );
                    lazy.invoke_engine_ron(&payload)?;
                }
                ImportFormat::Jsonl | ImportFormat::Csv => {
                    let content = read_input(&args.input)?;
                    eprintln!(
                        "[persisting-cli] search create: read {} bytes from {:?}, parsing…",
                        content.len(),
                        args.input
                    );
                    let rows = match fmt {
                        ImportFormat::Jsonl => {
                            parse_jsonl_import(&content, &args.dataset, args.embedding_dim)?
                        }
                        ImportFormat::Csv => {
                            parse_csv_import(&content, &args.dataset, args.embedding_dim)?
                        }
                        ImportFormat::Auto | ImportFormat::Lance => unreachable!(),
                    };
                    eprintln!(
                        "[persisting-cli] search create: parsed {} rows, sending to engine…",
                        rows.len()
                    );
                    search_add_batch(lazy, rows)?;
                }
                ImportFormat::Auto => unreachable!(),
            }
        }
        SearchCommand::Index(idx) => match &idx.command {
            SearchIndexCommand::List(args) => {
                let payload =
                    rpc_request_pretty(RequestBody::SearchIndexList(SearchIndexListRequest {
                        dataset: args.dataset.clone(),
                    }))
                    .context("encode SearchIndexList RpcRequest RON")?;
                lazy.invoke_engine_ron(&payload)?;
            }
            SearchIndexCommand::Build(args) => {
                let ivf_balance_postprocess = if args.ivf_balance_postprocess {
                    Some(true)
                } else {
                    None
                };
                let payload = rpc_request_pretty(RequestBody::SearchIndex(SearchIndexRequest {
                    dataset: args.dataset.clone(),
                    vector_column: args.vector_column.clone(),
                    text_column: args.text_column.clone(),
                    metric: args.metric.clone(),
                    num_partitions: args.num_partitions,
                    ivf_max_iters: args.ivf_max_iters,
                    ivf_balance_factor: args.ivf_balance_factor,
                    ivf_balance_postprocess,
                    ivf_postprocess_max_cluster_ratio: args.ivf_postprocess_max_cluster_ratio,
                    ivf_sample_rate: args.ivf_sample_rate,
                    ivf_target_partition_size: args.ivf_target_partition_size,
                    ivf_shuffle_partition_batches: args.ivf_shuffle_partition_batches,
                    ivf_shuffle_partition_concurrency: args.ivf_shuffle_partition_concurrency,
                    pq_num_sub_vectors: args.pq_num_sub_vectors,
                    pq_num_bits: args.pq_num_bits,
                    pq_max_iters: args.pq_max_iters,
                    pq_kmeans_redos: args.pq_kmeans_redos,
                    pq_sample_rate: args.pq_sample_rate,
                }))
                .context("encode SearchIndex RpcRequest RON")?;
                eprintln!(
                    "[persisting-cli] search index build: dataset {:?} (IVF/PQ 训练可能需数分钟，期间无 stdout 输出)…",
                    args.dataset
                );
                lazy.invoke_engine_ron(&payload)?;
                eprintln!(
                    "[persisting-cli] search index build: finished {:?}",
                    args.dataset
                );
            }
            SearchIndexCommand::Delete(args) => {
                let payload =
                    rpc_request_pretty(RequestBody::SearchIndexDelete(SearchIndexDeleteRequest {
                        dataset: args.dataset.clone(),
                        index_name: args.index_name.clone(),
                    }))
                    .context("encode SearchIndexDelete RpcRequest RON")?;
                lazy.invoke_engine_ron(&payload)?;
            }
            SearchIndexCommand::Rebuild(args) => {
                let payload = rpc_request_pretty(RequestBody::SearchIndexRebuild(
                    SearchIndexRebuildRequest {
                        dataset: args.dataset.clone(),
                        index_name: args.index_name.clone(),
                        retrain: !args.no_retrain,
                        merge_num_indices: args.merge_num_indices,
                    },
                ))
                .context("encode SearchIndexRebuild RpcRequest RON")?;
                eprintln!(
                    "[persisting-cli] search index rebuild: dataset {:?} index {:?} (可能较慢)…",
                    args.dataset, args.index_name
                );
                lazy.invoke_engine_ron(&payload)?;
                eprintln!("[persisting-cli] search index rebuild: finished");
            }
            SearchIndexCommand::Reorder(args) => {
                let payload = rpc_request_pretty(RequestBody::SearchIndexReorder(
                    SearchIndexReorderRequest {
                        dataset: args.dataset.clone(),
                        pivot_index: args.pivot_index.clone(),
                        target: args.target.clone(),
                        in_place: args.in_place,
                    },
                ))
                .context("encode SearchIndexReorder RpcRequest RON")?;
                eprintln!(
                    "[persisting-cli] search index reorder: dataset {:?} (可能较慢)…",
                    args.dataset
                );
                lazy.invoke_engine_ron(&payload)?;
                eprintln!("[persisting-cli] search index reorder: finished");
            }
        },
        SearchCommand::Query(args) => {
            let payload = rpc_request_pretty(RequestBody::SearchQuery(SearchQueryRequest {
                dataset: args.dataset.clone(),
                query: args.query.clone(),
                mode: args.mode.to_string(),
                k: args.k,
                embedding_dim: args.embedding_dim,
                text_column: args.text_column.clone(),
                filter: args.filter.clone(),
                nprobes: args.nprobes,
                minimum_nprobes: args.minimum_nprobes,
                maximum_nprobes: args.maximum_nprobes,
                adaptive_nprobes_margin: args.adaptive_nprobes_margin,
            }))
            .context("encode SearchQuery RpcRequest RON")?;
            eprintln!(
                "[persisting-cli] search query: dataset {:?} mode {:?}…",
                args.dataset, args.mode
            );
            lazy.invoke_engine_ron(&payload)?;
        }
    }
    Ok(())
}

fn resolve_import_format(input_path: &str, explicit: ImportFormat) -> Result<ImportFormat> {
    match explicit {
        ImportFormat::Auto => infer_import_format_from_path(input_path),
        f => Ok(f),
    }
}

fn infer_import_format_from_path(input_path: &str) -> Result<ImportFormat> {
    if input_path == "-" {
        anyhow::bail!("when --input is '-' (stdin), set --format to jsonl or csv");
    }
    let p = Path::new(input_path);
    if p.join("data.lance").exists() {
        return Ok(ImportFormat::Lance);
    }
    let lower = input_path.to_ascii_lowercase();
    if lower.ends_with(".csv") {
        return Ok(ImportFormat::Csv);
    }
    if lower.ends_with(".jsonl") || lower.ends_with(".json") {
        return Ok(ImportFormat::Jsonl);
    }
    anyhow::bail!(
        "cannot infer --format from path '{}'; use --format jsonl, csv, or lance (or point --input at a directory that contains data.lance/)",
        input_path
    )
}

fn parse_jsonl_import(
    content: &str,
    dataset: &str,
    embedding_dim: usize,
) -> Result<Vec<SearchAddRequest>> {
    let mut rows = Vec::new();
    for (line_no, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let mut value: serde_json::Value = serde_json::from_str(line)
            .with_context(|| format!("invalid JSON on line {} of JSONL import", line_no + 1))?;
        let text = value
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "line {}: JSON object must contain string field 'text'",
                    line_no + 1
                )
            })?
            .to_string();
        let id = value
            .get("id")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned);
        let metadata = value
            .as_object_mut()
            .and_then(|object| object.remove("metadata"));
        rows.push(SearchAddRequest {
            dataset: dataset.to_string(),
            id,
            text,
            metadata,
            embedding_dim,
        });
        let n = rows.len();
        if n == 1 || n % 2000 == 0 {
            eprintln!("[persisting-cli] search create: parsed {n} jsonl rows…");
        }
    }
    if rows.is_empty() {
        anyhow::bail!("JSONL import contained no non-empty lines");
    }
    Ok(rows)
}

fn parse_csv_import(
    content: &str,
    dataset: &str,
    embedding_dim: usize,
) -> Result<Vec<SearchAddRequest>> {
    let mut rdr = csv::ReaderBuilder::new()
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(content.as_bytes());
    let headers = rdr.headers().context("CSV headers")?.clone();
    let pos_text = headers
        .iter()
        .position(|h| h.eq_ignore_ascii_case("text"))
        .ok_or_else(|| {
            anyhow::anyhow!("CSV must include a column named 'text' (case-insensitive)")
        })?;
    let pos_id = headers.iter().position(|h| h.eq_ignore_ascii_case("id"));
    let mut out = Vec::new();
    for (row_idx, result) in rdr.records().enumerate() {
        let rec = result.with_context(|| format!("CSV parse error at data row {}", row_idx + 1))?;
        let text = rec
            .get(pos_text)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("row {}: missing or empty 'text'", row_idx + 2))?
            .to_string();
        let id = pos_id
            .and_then(|p| rec.get(p))
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let mut meta = serde_json::Map::new();
        for (i, col_name) in headers.iter().enumerate() {
            if i == pos_text || pos_id == Some(i) {
                continue;
            }
            if let Some(val) = rec.get(i) {
                if val.is_empty() {
                    continue;
                }
                meta.insert(
                    col_name.to_string(),
                    serde_json::Value::String(val.to_string()),
                );
            }
        }
        let metadata = if meta.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(meta))
        };
        out.push(SearchAddRequest {
            dataset: dataset.to_string(),
            id,
            text,
            metadata,
            embedding_dim,
        });
        let n = out.len();
        if n == 1 || n % 2000 == 0 {
            eprintln!("[persisting-cli] search create: parsed {n} csv rows…");
        }
    }
    if out.is_empty() {
        anyhow::bail!("CSV contained no data rows");
    }
    Ok(out)
}

fn invoke_trajectory_stats(
    lazy: &mut LazyEngine<'_>,
    req: TrajectoryStatsRequest,
) -> Result<TrajectoryStatsResponse> {
    let payload =
        rpc_request_pretty(RequestBody::TrajectoryStats(req)).context("encode TrajectoryStats")?;
    let raw = lazy.invoke_engine_ron_silent(&payload)?;
    match parse_engine_ron_response(&raw)?.body {
        ResponseBody::TrajectoryStats(r) => Ok(r),
        other => anyhow::bail!("unexpected engine response: {other:?}"),
    }
}

fn invoke_trajectory_replay(
    lazy: &mut LazyEngine<'_>,
    req: TrajectoryReplayRequest,
) -> Result<TrajectoryReplayResponse> {
    let payload = rpc_request_pretty(RequestBody::TrajectoryReplay(req))
        .context("encode TrajectoryReplay")?;
    let raw = lazy.invoke_engine_ron_silent(&payload)?;
    match parse_engine_ron_response(&raw)?.body {
        ResponseBody::TrajectoryReplay(r) => Ok(r),
        other => anyhow::bail!("unexpected engine response: {other:?}"),
    }
}

fn run_trajectory(lazy: &mut LazyEngine<'_>, args: &TrajectoryArgs) -> Result<()> {
    match &args.command {
        TrajectoryCommand::Add(args) => {
            let auto_agent = args.agent_id.is_none();
            let auto_session = args.session_id.is_none();
            let (agent_id, session_id) =
                resolve_traj_ids_for_write(args.agent_id.clone(), args.session_id.clone())?;
            if auto_agent || auto_session {
                eprintln!(
                    "[persisting-cli] trajectory add: auto agent_id={agent_id} session_id={session_id} (override with --agent-id / --session-id)"
                );
            }
            let raw = read_input(&args.input)?;
            let input_format =
                TrajectoryFormatManager::resolve_add_format(&args.input, args.format)
                    .context("resolve trajectory add input format")?;
            let storage_format =
                TrajectoryFormatManager::resolve_storage_format(&args.input, args.storage_format);
            eprintln!(
                "[persisting-cli] trajectory add: read {} bytes from {:?} (format={input_format:?} storage={:?}), converting…",
                raw.len(),
                args.input,
                storage_format,
            );
            let records_ronl = TrajectoryFormatManager::prepare_append_batch(input_format, &raw)
                .context("normalize trajectory add input")?;
            eprintln!(
                "[persisting-cli] trajectory add: {} bytes internal payload, building RpcRequest…",
                records_ronl.len()
            );
            let payload =
                rpc_request_pretty(RequestBody::TrajectoryAppend(TrajectoryAppendRequest {
                    storage: args.storage.clone(),
                    agent_id,
                    session_id,
                    root_session_id: None,
                    records_ronl,
                    storage_format,
                }))
                .context("encode TrajectoryAppend RpcRequest RON")?;
            eprintln!(
                "[persisting-cli] trajectory add: request {} bytes, calling engine (大轨迹可能较慢)…",
                payload.len()
            );
            lazy.invoke_engine_ron(&payload)?;
            eprintln!("[persisting-cli] trajectory add: engine returned");
        }
        TrajectoryCommand::Replay(args) => {
            let loc = resolve_traj_ids_for_read(
                "trajectory replay",
                args.storage.clone(),
                args.agent_id.clone(),
                args.session_id.clone(),
                args.root_session_id.clone(),
            )?;
            let payload =
                rpc_request_pretty(RequestBody::TrajectoryReplay(TrajectoryReplayRequest {
                    storage: loc.storage,
                    agent_id: loc.agent_id,
                    session_id: loc.session_id,
                    offset: args.offset,
                    limit: args.limit,
                    storage_format: args.storage_format.into(),
                    root_session_id: loc.root_session_id,
                }))
                .context("encode TrajectoryReplay RpcRequest RON")?;
            lazy.invoke_engine_ron(&payload)?;
        }
        TrajectoryCommand::Stats(args) => {
            let loc = resolve_traj_ids_for_read(
                "trajectory stats",
                args.storage.clone(),
                args.agent_id.clone(),
                args.session_id.clone(),
                args.root_session_id.clone(),
            )?;
            let stats_req = TrajectoryStatsRequest {
                storage: loc.storage.clone(),
                agent_id: loc.agent_id.clone(),
                session_id: loc.session_id.clone(),
                storage_format: args.storage_format.into(),
                root_session_id: loc.root_session_id.clone(),
            };
            if args.detail {
                let stats = invoke_trajectory_stats(lazy, stats_req)?;
                if stats.status != "ok" {
                    print_trajectory_stats_as_toml(&stats)?;
                    return Ok(());
                }
                let parent_root = loc
                    .root_session_id
                    .clone()
                    .unwrap_or_else(|| loc.session_id.clone());
                let replay = invoke_trajectory_replay(
                    lazy,
                    TrajectoryReplayRequest {
                        storage: loc.storage.clone(),
                        agent_id: loc.agent_id.clone(),
                        session_id: loc.session_id.clone(),
                        offset: 0,
                        limit: None,
                        storage_format: args.storage_format.into(),
                        root_session_id: loc.root_session_id.clone(),
                    },
                )?;
                let storage = loc.storage.clone();
                let agent_id = loc.agent_id.clone();
                let storage_format = args.storage_format;
                let mut load_subagent = |link: &SpawnLinkInfo| -> Result<Option<Vec<String>>> {
                    let replay = invoke_trajectory_replay(
                        lazy,
                        TrajectoryReplayRequest {
                            storage: storage.clone(),
                            agent_id: agent_id.clone(),
                            session_id: link.storage_session_id(),
                            offset: 0,
                            limit: None,
                            storage_format: storage_format.into(),
                            root_session_id: Some(parent_root.clone()),
                        },
                    );
                    match replay {
                        Ok(r) if r.status == "ok" && !r.records.is_empty() => Ok(Some(r.records)),
                        Ok(_) => Ok(None),
                        Err(_) => Ok(None),
                    }
                };
                let tree = build_detail_node(
                    format!("main ({})", stats.session_id),
                    &replay.records,
                    &mut load_subagent,
                )?;
                print_trajectory_stats_detail(&stats, &tree)?;
            } else {
                let payload = rpc_request_pretty(RequestBody::TrajectoryStats(stats_req))
                    .context("encode TrajectoryStats RpcRequest RON")?;
                lazy.invoke_engine_ron(&payload)?;
            }
        }
        TrajectoryCommand::Materialize(args) => {
            let (agent_id, session_id) =
                resolve_traj_ids_for_write(args.agent_id.clone(), args.session_id.clone())?;
            let payload = rpc_request_pretty(RequestBody::TrajectoryMaterialize(
                TrajectoryMaterializeRequest {
                    storage: args.storage.clone(),
                    agent_id,
                    session_id,
                    root_session_id: args.root_session_id.clone(),
                },
            ))
            .context("encode TrajectoryMaterialize RpcRequest RON")?;
            lazy.invoke_engine_ron(&payload)?;
        }
    }
    Ok(())
}

fn read_input(path: &str) -> Result<String> {
    if path == "-" {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        return Ok(buffer);
    }
    Ok(fs::read_to_string(path)?)
}
