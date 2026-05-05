//! CLI loads `libpersisting_engine` lazily and calls **`persisting_engine_submit`** / **`job_poll`** /
//! **`job_take_result`**（异步 job + 进度；见 `persisting_proto::invoke_abi`）。

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use libloading::{Library, Symbol};
use persisting_proto::{
    RequestBody, ResponseBody, RpcRequest, RpcResponse, SearchAddRequest, SearchImportLanceRequest,
    SearchIndexDeleteRequest, SearchIndexListRequest, SearchIndexReorderRequest,
    SearchIndexRebuildRequest, SearchIndexRequest, SearchQueryRequest, TrajectoryAppendRequest,
    TrajectoryReplayRequest, TrajectoryStatsRequest,
    PROTOCOL_VERSION, RON_ABI_VERSION,
};
use serde::{Deserialize, Serialize};

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
        Self {
            cli,
            engine: None,
        }
    }

    fn engine_mut(&mut self) -> Result<&Engine> {
        if self.engine.is_none() {
            let path = resolve_engine_path(self.cli)?;
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
            self.lib.get(b"persisting_engine_submit\0").with_context(|| {
                "missing engine export persisting_engine_submit; rebuild persisting-engine"
            })?
        };
        let poll: Symbol<persisting_proto::PersistingEngineJobPollFn> = unsafe {
            self.lib.get(b"persisting_engine_job_poll\0").with_context(|| {
                "missing engine export persisting_engine_job_poll; rebuild persisting-engine"
            })?
        };
        let take: Symbol<persisting_proto::PersistingEngineJobTakeResultFn> = unsafe {
            self.lib.get(b"persisting_engine_job_take_result\0").with_context(|| {
                "missing engine export persisting_engine_job_take_result; rebuild persisting-engine"
            })?
        };
        let release: Symbol<persisting_proto::PersistingEngineJobReleaseFn> = unsafe {
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
        let raw = unsafe { persisting_proto::invoke_ron_utf8_via_jobs_sync(syms, payload.as_bytes())? };
        persisting_proto::response_utf8_to_string(&raw).context("engine response UTF-8")
    }
}

fn print_engine_ron_response(raw: &str) -> Result<()> {
    if let Ok(w) = ron::from_str::<WireError>(raw) {
        anyhow::bail!("{}", w.error);
    }
    let resp: RpcResponse = ron::from_str(raw).context("engine returned invalid RON RpcResponse")?;
    if let ResponseBody::Error { message, .. } = &resp.body {
        anyhow::bail!("{}", message);
    }
    println!(
        "{}",
        ron::ser::to_string_pretty(
            &resp.body,
            ron::ser::PrettyConfig::new().indentor("    ".to_string()),
        )?
    );
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
    Trajectory(TrajectoryArgs),
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
}

#[derive(Debug, Args)]
struct TrajectoryAddArgs {
    /// Root directory for trajectory datasets (parent of per-name Lance dirs).
    #[arg(value_name = "STORAGE")]
    storage: String,
    /// Logical trajectory name (namespace under `STORAGE`).
    #[arg(value_name = "NAME")]
    name: String,
    /// Input encoding: **jsonl** = one JSON object per line; **toml** = root `records` array (`[[records]]` or `records = [...]`); **ronl** = one RON value per line (passed through).
    #[arg(long, value_enum, default_value_t = TrajectoryAddFormat::Jsonl)]
    format: TrajectoryAddFormat,
    #[arg(long, default_value = "-")]
    input: String,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum TrajectoryAddFormat {
    /// One JSON object per non-empty line; converted to RON lines for the engine.
    #[default]
    Jsonl,
    /// Single TOML document with a `records` array (array of tables or inline tables).
    Toml,
    /// One RON value per non-empty line (unchanged on the wire).
    Ronl,
}

#[derive(Debug, Args)]
struct TrajectoryReplayArgs {
    /// Root directory for trajectory datasets.
    #[arg(value_name = "STORAGE")]
    storage: String,
    /// Logical trajectory name.
    #[arg(value_name = "NAME")]
    name: String,
    #[arg(long)]
    trajectory_id: Option<String>,
    #[arg(long, default_value_t = 0)]
    offset: usize,
    #[arg(long)]
    limit: Option<usize>,
}

#[derive(Debug, Args)]
struct TrajectoryStatsArgs {
    /// Root directory for trajectory datasets.
    #[arg(value_name = "STORAGE")]
    storage: String,
    /// Logical trajectory name.
    #[arg(value_name = "NAME")]
    name: String,
}

fn resolve_engine_path(cli: &Cli) -> Result<PathBuf> {
    if let Some(ref p) = cli.core_lib {
        return Ok(p.clone());
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
        ["libpersisting_engine.dylib", "libpersisting_engine.so", "persisting_engine.dll"]
    }
    #[cfg(target_os = "linux")]
    {
        ["libpersisting_engine.so", "libpersisting_engine.dylib", "persisting_engine.dll"]
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "linux")))]
    {
        ["persisting_engine.dll", "libpersisting_engine.so", "libpersisting_engine.dylib"]
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut lazy = LazyEngine::new(&cli);
    match &cli.command {
        Command::Search(args) => run_search(&mut lazy, args)?,
        Command::Trajectory(args) => run_trajectory(&mut lazy, args)?,
    }
    Ok(())
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
                    lazy.invoke_engine_ron(&payload)?;
                }
                ImportFormat::Jsonl | ImportFormat::Csv => {
                    let content = read_input(&args.input)?;
                    let rows = match fmt {
                        ImportFormat::Jsonl => {
                            parse_jsonl_import(&content, &args.dataset, args.embedding_dim)?
                        }
                        ImportFormat::Csv => {
                            parse_csv_import(&content, &args.dataset, args.embedding_dim)?
                        }
                        ImportFormat::Auto | ImportFormat::Lance => unreachable!(),
                    };
                    for row in rows {
                        let payload = rpc_request_pretty(RequestBody::SearchAdd(row))
                            .context("encode SearchAdd RpcRequest RON")?;
                        lazy.invoke_engine_ron(&payload)?;
                    }
                }
                ImportFormat::Auto => unreachable!(),
            }
        }
        SearchCommand::Index(idx) => match &idx.command {
            SearchIndexCommand::List(args) => {
                let payload = rpc_request_pretty(RequestBody::SearchIndexList(
                    SearchIndexListRequest {
                        dataset: args.dataset.clone(),
                    },
                ))
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
                lazy.invoke_engine_ron(&payload)?;
            }
            SearchIndexCommand::Delete(args) => {
                let payload = rpc_request_pretty(RequestBody::SearchIndexDelete(
                    SearchIndexDeleteRequest {
                        dataset: args.dataset.clone(),
                        index_name: args.index_name.clone(),
                    },
                ))
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
                lazy.invoke_engine_ron(&payload)?;
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
                lazy.invoke_engine_ron(&payload)?;
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
        let mut value: serde_json::Value = serde_json::from_str(line).with_context(|| {
            format!(
                "invalid JSON on line {} of JSONL import",
                line_no + 1
            )
        })?;
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
        .ok_or_else(|| anyhow::anyhow!("CSV must include a column named 'text' (case-insensitive)"))?;
    let pos_id = headers
        .iter()
        .position(|h| h.eq_ignore_ascii_case("id"));
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
                meta.insert(col_name.to_string(), serde_json::Value::String(val.to_string()));
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
    }
    if out.is_empty() {
        anyhow::bail!("CSV contained no data rows");
    }
    Ok(out)
}

fn run_trajectory(lazy: &mut LazyEngine<'_>, args: &TrajectoryArgs) -> Result<()> {
    match &args.command {
        TrajectoryCommand::Add(args) => {
            let raw = read_input(&args.input)?;
            let records_ronl =
                trajectory_add_input_to_ronl(args.format, &raw).context("normalize trajectory add input")?;
            let payload = rpc_request_pretty(RequestBody::TrajectoryAppend(
                TrajectoryAppendRequest {
                    storage: args.storage.clone(),
                    name: args.name.clone(),
                    records_ronl,
                },
            ))
            .context("encode TrajectoryAppend RpcRequest RON")?;
            lazy.invoke_engine_ron(&payload)?;
        }
        TrajectoryCommand::Replay(args) => {
            let payload = rpc_request_pretty(RequestBody::TrajectoryReplay(
                TrajectoryReplayRequest {
                    storage: args.storage.clone(),
                    name: args.name.clone(),
                    trajectory_id: args.trajectory_id.clone(),
                    offset: args.offset,
                    limit: args.limit,
                },
            ))
            .context("encode TrajectoryReplay RpcRequest RON")?;
            lazy.invoke_engine_ron(&payload)?;
        }
        TrajectoryCommand::Stats(args) => {
            let payload = rpc_request_pretty(RequestBody::TrajectoryStats(
                TrajectoryStatsRequest {
                    storage: args.storage.clone(),
                    name: args.name.clone(),
                },
            ))
            .context("encode TrajectoryStats RpcRequest RON")?;
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

/// CLI accepts JSONL / TOML / RONL; the engine contract remains newline-separated RON values.
fn trajectory_add_input_to_ronl(format: TrajectoryAddFormat, raw: &str) -> Result<String> {
    match format {
        TrajectoryAddFormat::Ronl => Ok(raw.to_string()),
        TrajectoryAddFormat::Jsonl => jsonl_to_ronl(raw),
        TrajectoryAddFormat::Toml => toml_records_to_ronl(raw),
    }
}

fn jsonl_to_ronl(src: &str) -> Result<String> {
    let mut lines = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line)
            .with_context(|| format!("invalid JSON on trajectory jsonl line {}", i + 1))?;
        lines.push(
            ron::to_string(&v).with_context(|| format!("encode trajectory jsonl line {} as RON", i + 1))?,
        );
    }
    Ok(lines.join("\n"))
}

fn toml_records_to_ronl(src: &str) -> Result<String> {
    let root: toml::Value =
        toml::from_str(src).context("parse trajectory TOML (expect root table with `records` array)")?;
    let records = root.get("records").ok_or_else(|| {
        anyhow::anyhow!(
            "trajectory TOML must define `records`: e.g. `[[records]]` per row or `records = [{{ ... }}, ...]`"
        )
    })?;
    let arr = records
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("trajectory TOML `records` must be an array"))?;
    let mut lines = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        lines.push(
            ron::to_string(item).with_context(|| format!("encode trajectory TOML records[{i}] as RON"))?,
        );
    }
    Ok(lines.join("\n"))
}
