//! Unified CLI backed directly by pChronicle for durable history and evaluation.

mod capture;
mod judge_manual;
mod stats_output;
mod terminal_markdown;
mod trajectory_convert;
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
use persisting_pchronicle::{
    JudgeMethod, JudgeSampleMode, JudgeScope, RequestBody, ResponseBody, TrajectoryAppendRequest,
    TrajectoryExtractRequest, TrajectoryJudgeRequest, TrajectoryJudgeResponse,
    TrajectoryJudgeStatsRequest, TrajectoryJudgeStatsResponse, TrajectoryMaterializeRequest,
    TrajectoryReplayRequest, TrajectoryReplayResponse, TrajectoryStatsRequest,
    TrajectoryStatsResponse, TrajectoryStorageFormat, TrajectoryTruncateRequest,
};

use persisting_gateway::engine::TurnKind;
use persisting_pchronicle::{
    drop_lifecycle_run_partitions, expand_story_locations_blocking, list_traj_read_locations,
    merge_traj_location, resolve_traj_read_location, StoryCoords as TrajLocation,
};
use stats_output::{
    print_stats_section_divider, print_trajectory_stats_detail, print_trajectory_stats_list,
    print_trajectory_stats_summary, supports_detail_tree, ResolvedStatsOutputBackend,
    StatsOutputBackend,
};
use trajectory_detail::{build_detail_node, SpawnLinkInfo};
use trajectory_format::{TrajectoryAddFormat, TrajectoryFormatManager, TrajectoryStorageCli};
use trajectory_stdout_toml::{
    print_trajectory_append_as_toml, print_trajectory_extract_as_toml,
    print_trajectory_judge_as_toml, print_trajectory_judge_stats_as_toml,
    print_trajectory_materialize_as_toml, print_trajectory_replay_as_toml,
    print_trajectory_stats_as_toml, print_trajectory_truncate_as_toml,
};

#[derive(Clone, Copy)]
struct Chronicle;

struct ChronicleClient {
    chronicle: Chronicle,
}

impl ChronicleClient {
    fn new() -> Self {
        Self {
            chronicle: Chronicle,
        }
    }

    fn invoke(&mut self, request: &RequestBody) -> Result<()> {
        let response = self.chronicle.invoke(request)?;
        print_chronicle_response(&response)
    }

    fn invoke_silent(&mut self, request: &RequestBody) -> Result<ResponseBody> {
        self.chronicle.invoke(request)
    }
}

impl Chronicle {
    fn invoke(&self, request: &RequestBody) -> Result<ResponseBody> {
        persisting_pchronicle::invoke_request_body(request.clone()).context("pChronicle request")
    }
}

fn print_chronicle_response(response: &ResponseBody) -> Result<()> {
    match response {
        // trajectory 成功响应统一用 TOML stdout（与默认写入格式一致）。
        ResponseBody::TrajectoryAppend(tr) => print_trajectory_append_as_toml(tr),
        ResponseBody::TrajectoryStats(tr) => print_trajectory_stats_as_toml(tr),
        ResponseBody::TrajectoryReplay(tr) => print_trajectory_replay_as_toml(tr),
        ResponseBody::TrajectoryMaterialize(tr) => print_trajectory_materialize_as_toml(tr),
        ResponseBody::TrajectoryTruncate(tr) => print_trajectory_truncate_as_toml(tr),
        ResponseBody::TrajectoryExtract(tr) => print_trajectory_extract_as_toml(tr),
        ResponseBody::TrajectoryJudge(tr) => print_trajectory_judge_as_toml(tr),
        ResponseBody::TrajectoryJudgeStats(tr) => print_trajectory_judge_stats_as_toml(tr),
        _ => {
            println!(
                "{}",
                ron::ser::to_string(response).map_err(|e| anyhow::anyhow!("RON serialize: {e}"))?
            );
            Ok(())
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "persisting",
    version,
    about = "Unified CLI for Agent execution, environments, orchestration, and durable history"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Execute one Agent or command in a fresh pVisor Run.
    #[command(visible_alias = "exec")]
    Execute(ForwardArgs),
    /// Manage durable reusable pVisor execution environments.
    #[command(visible_alias = "environment")]
    Env(ForwardArgs),
    /// Run a pPilot batch plan.
    Batch(ForwardArgs),
    /// Query Lance or ATIF history with pPilot/DataFusion SQL.
    Query(ForwardArgs),
    /// Import, replay, convert, and maintain trajectory history.
    History(HistoryArgs),
    /// Evaluate trajectory quality.
    Eval(EvalArgs),
    /// Run or manage the long-lived Gateway capture service.
    Gateway(GatewayArgs),
}

#[derive(Debug, Args)]
#[command(disable_help_flag = true)]
struct ForwardArgs {
    /// Arguments forwarded unchanged to the component command.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[derive(Debug, Args)]
struct HistoryArgs {
    #[command(subcommand)]
    command: HistoryCommand,
}

#[derive(Debug, Subcommand)]
enum HistoryCommand {
    /// Merge IDE or Gateway events into one trajectory session.
    Import(CaptureImportArgs),
    /// Retry capture events recorded in the dead-letter log.
    #[command(name = "replay-dead-letter")]
    ReplayDeadLetter(CaptureReplayDeadLetterArgs),
    /// Append normalized events to a trajectory store.
    Add(TrajectoryAddArgs),
    /// Keep only the first N Lance events.
    Truncate(TrajectoryTruncateArgs),
    /// Summarize one or more runs and stories.
    Stats(TrajectoryStatsArgs),
    /// Page through stored events in sequence order.
    Replay(TrajectoryReplayArgs),
    /// Export a Story or Run directory tree.
    Extract(TrajectoryExtractArgs),
    /// Rebuild the human-readable AgenticMD projection from Lance.
    Materialize(TrajectoryMaterializeArgs),
    /// Convert between Storyline, ATIF, OpenAI messages, AgenticMD, and events.
    Convert(trajectory_convert::TrajectoryConvertArgs),
}

#[derive(Debug, Args)]
struct EvalArgs {
    #[command(subcommand)]
    command: EvalCommand,
}

#[derive(Debug, Subcommand)]
enum EvalCommand {
    /// Judge trajectory quality manually or with an LLM.
    Judge(TrajectoryJudgeArgs),
    /// Aggregate persisted judge scores by session and rubric.
    Stats(TrajectoryJudgeStatsArgs),
}

#[derive(Debug, Args)]
struct GatewayArgs {
    #[command(subcommand)]
    command: GatewayCommand,
}

#[derive(Debug, Subcommand)]
enum GatewayCommand {
    /// Run the Gateway in the foreground.
    Serve(GatewayServeArgs),
    /// Start the Gateway daemon.
    Start(GatewayStartArgs),
    /// Stop the Gateway daemon.
    Stop(GatewaySelectArgs),
    /// List captured sessions.
    List(GatewaySelectArgs),
    /// Show Gateway daemon status.
    Status(GatewaySelectArgs),
}

#[derive(Debug, Args)]
struct GatewayServeArgs {
    #[arg(long, value_enum, default_value_t = GatewayBackend::Capture)]
    backend: GatewayBackend,
    #[arg(long, short = 'o', value_name = "DIR")]
    output_dir: Option<String>,
    #[arg(long, short = 'c', value_name = "FILE")]
    config: Option<PathBuf>,
    #[arg(long)]
    debug: bool,
    #[arg(long, short = 'f', value_enum)]
    format: Option<capture::CaptureFormat>,
}

#[derive(Debug, Args)]
struct CaptureReplayDeadLetterArgs {
    #[arg(
        long,
        short = 'o',
        value_name = "DIR",
        env = "PERSISTING_CAPTURE_STORAGE",
        default_value = ".persisting/capture"
    )]
    output_dir: String,
    #[arg(long, short = 'f', value_enum, default_value_t = capture::CaptureFormat::Markdown)]
    format: capture::CaptureFormat,
}

#[derive(Debug)]
struct CaptureServeConfig {
    output_dir: String,
    config: PathBuf,
    debug: bool,
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
    /// Print counts only; do not write through pChronicle.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
enum GatewayBackend {
    #[default]
    Capture,
    Dlcapt,
}

#[derive(Debug, Args)]
struct GatewayStartArgs {
    #[arg(
        long,
        short = 'o',
        value_name = "DIR",
        env = "PERSISTING_CAPTURE_STORAGE"
    )]
    output_dir: String,
    #[arg(long, short = 'c', value_name = "FILE")]
    config: PathBuf,
    #[arg(long)]
    debug: bool,
    #[arg(long, short = 'f', value_enum, default_value_t = capture::CaptureFormat::Markdown)]
    format: capture::CaptureFormat,
}

#[derive(Debug, Args)]
struct GatewaySelectArgs {
    /// Trajectory store (default: last `gateway start` or `PERSISTING_CAPTURE_STORAGE`).
    #[arg(
        long,
        short = 'o',
        value_name = "DIR",
        env = "PERSISTING_CAPTURE_STORAGE"
    )]
    output_dir: Option<String>,
}

#[derive(Debug, Args)]
struct TrajectoryAddArgs {
    /// Root directory for trajectory datasets (parent of `agent_id/session_id/` run dirs).
    #[arg(value_name = "STORAGE")]
    storage: String,
    /// Agent identity（单层路径段；省略则自动生成并在 stderr 打印）。
    #[arg(long, value_name = "SEG")]
    agent_id: Option<String>,
    /// Session / run id（单层路径段；省略则自动生成并在 stderr 打印）。
    #[arg(long, value_name = "SEG")]
    session_id: Option<String>,
    /// 输入格式；`auto` 时按 `--input` 文件名推断（`{session_id}.md` → markdown，`.jsonl` → jsonl，…）。
    #[arg(long, value_enum, default_value_t = TrajectoryAddFormat::Auto)]
    format: TrajectoryAddFormat,
    #[arg(long, default_value = "-")]
    input: String,
    /// 写入层：`lance` / `markdown` / `auto`（`auto` 按已有层探测，默认新建 Lance）。
    #[arg(long, value_enum, default_value_t = TrajectoryStorageCli::Auto)]
    storage_format: TrajectoryStorageCli,
}

#[derive(Debug, Args)]
struct TrajectoryTruncateArgs {
    /// Storage root or session directory; omit to use `PERSISTING_CAPTURE_STORAGE` / last proxy start.
    #[arg(value_name = "STORAGE")]
    storage: Option<String>,
    #[arg(long, value_name = "SEG")]
    agent_id: Option<String>,
    #[arg(long, value_name = "SEG")]
    session_id: Option<String>,
    #[arg(long, value_name = "SEG")]
    root_session_id: Option<String>,
    /// 保留按 `seq` 排序的前 N 条 Lance 行（仅 Lance 层；需更新 md 请单独 `materialize`）。
    #[arg(long)]
    keep_rows: usize,
}

#[derive(Debug, Args)]
struct TrajectoryExtractArgs {
    #[arg(value_name = "STORAGE")]
    storage: String,
    #[arg(value_name = "OUT_DIR")]
    out_dir: String,
    #[arg(long, value_name = "SEG")]
    agent_id: Option<String>,
    #[arg(long, value_name = "SEG")]
    session_id: Option<String>,
    #[arg(long, value_name = "SEG")]
    root_session_id: Option<String>,
    /// 主 Run story 时一并导出 `subagents/` 子树。
    #[arg(long)]
    include_subagents: bool,
}

#[derive(Debug, Args)]
struct TrajectoryReplayArgs {
    /// Storage root or session directory; omit to use `PERSISTING_CAPTURE_STORAGE` / last proxy start.
    #[arg(value_name = "STORAGE")]
    storage: Option<String>,
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
    /// Canonical 存储选择；`auto` 与 `lance` 当前都读取 Lance。
    #[arg(long, value_enum, default_value_t = TrajectoryStorageCli::Auto)]
    storage_format: TrajectoryStorageCli,
}

#[derive(Debug, Args)]
struct TrajectoryStatsArgs {
    /// Storage root or session directory; omit to use `PERSISTING_CAPTURE_STORAGE` / last proxy start.
    #[arg(value_name = "STORAGE")]
    storage: Option<String>,
    #[arg(long, value_name = "SEG")]
    agent_id: Option<String>,
    #[arg(long, value_name = "SEG")]
    session_id: Option<String>,
    /// 嵌套 subagent session 时指定父 session（路径 `{root}/subagents/{session_id}/`）。
    #[arg(long, value_name = "SEG")]
    root_session_id: Option<String>,
    /// Canonical 存储选择；统计以 Lance 为准，并附带 Markdown 调试视图信息。
    #[arg(long, value_enum, default_value_t = TrajectoryStorageCli::Auto)]
    storage_format: TrajectoryStorageCli,
    /// 逐轮一行摘要：用户/模型字符数、TTFT、TPOT。
    #[arg(long)]
    detail: bool,
    /// 输出后端：`plain`（稳妥纯文本）· `md`（易读 markdown）· `toml`/`json`（程序交互）· `auto`（TTY→md，管道→toml）。
    #[arg(long, value_enum, default_value_t = StatsOutputBackend::Auto)]
    output: StatsOutputBackend,
}

#[derive(Debug, Args)]
struct TrajectoryMaterializeArgs {
    /// Storage root or session directory; omit to use `PERSISTING_CAPTURE_STORAGE` / last proxy start.
    #[arg(value_name = "STORAGE")]
    storage: Option<String>,
    #[arg(long, value_name = "SEG")]
    agent_id: Option<String>,
    #[arg(long, value_name = "SEG")]
    session_id: Option<String>,
    #[arg(long, value_name = "SEG")]
    root_session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
enum JudgeScopeCli {
    #[default]
    Turn,
    Story,
}

impl From<JudgeScopeCli> for JudgeScope {
    fn from(v: JudgeScopeCli) -> Self {
        match v {
            JudgeScopeCli::Turn => JudgeScope::Turn,
            JudgeScopeCli::Story => JudgeScope::Story,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
enum JudgeMethodCli {
    #[default]
    Llm,
    Manual,
}

impl From<JudgeMethodCli> for JudgeMethod {
    fn from(v: JudgeMethodCli) -> Self {
        match v {
            JudgeMethodCli::Llm => JudgeMethod::Llm,
            JudgeMethodCli::Manual => JudgeMethod::Manual,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
enum JudgeSampleModeCli {
    #[default]
    Sequential,
    Random,
}

impl From<JudgeSampleModeCli> for JudgeSampleMode {
    fn from(v: JudgeSampleModeCli) -> Self {
        match v {
            JudgeSampleModeCli::Sequential => JudgeSampleMode::Sequential,
            JudgeSampleModeCli::Random => JudgeSampleMode::Random,
        }
    }
}

#[derive(Debug, Args)]
struct TrajectoryJudgeStatsArgs {
    /// Storage root or session directory; omit to use `PERSISTING_CAPTURE_STORAGE` / last proxy start.
    #[arg(value_name = "STORAGE", env = "PERSISTING_CAPTURE_STORAGE")]
    storage: Option<String>,
    #[arg(long, value_name = "SEG")]
    agent_id: Option<String>,
    #[arg(long, value_name = "SEG")]
    session_id: Option<String>,
    #[arg(long, value_name = "SEG")]
    root_session_id: Option<String>,
}

#[derive(Debug, Args)]
struct TrajectoryJudgeArgs {
    /// Storage root or session directory; omit to use `PERSISTING_CAPTURE_STORAGE` / last proxy start.
    #[arg(value_name = "STORAGE", env = "PERSISTING_CAPTURE_STORAGE")]
    storage: Option<String>,
    #[arg(long, value_name = "SEG")]
    agent_id: Option<String>,
    #[arg(long, value_name = "SEG")]
    session_id: Option<String>,
    #[arg(long, value_name = "SEG")]
    root_session_id: Option<String>,
    /// Primary rubric when `--rubrics` is empty.
    #[arg(long, default_value = "default")]
    rubric_id: String,
    /// Comma-separated score dimensions (e.g. `helpful,correct,safe`).
    #[arg(long, value_delimiter = ',')]
    rubrics: Vec<String>,
    /// `turn`: score each dialogue turn; `story`: score the full trajectory once.
    #[arg(long, value_enum, default_value_t = JudgeScopeCli::Turn)]
    scope: JudgeScopeCli,
    /// `llm`: model judge; `manual`: render markdown / turns and prompt for scores.
    #[arg(long, value_enum, default_value_t = JudgeMethodCli::Llm)]
    method: JudgeMethodCli,
    /// OpenAI-compatible chat model (`PERSISTING_JUDGE_MODEL` / `gpt-4o-mini` fallback).
    #[arg(long)]
    model: Option<String>,
    /// Skip LLM; write deterministic pass rows.
    #[arg(long)]
    dry_run: bool,
    /// Re-judge units that already have a row for this rubric.
    #[arg(long)]
    force: bool,
    /// Manual batch: pick sessions sequentially or at random (default: random when scanning storage).
    #[arg(long, value_enum)]
    sample: Option<JudgeSampleModeCli>,
    /// Manual batch: max sessions to score (default 1). Ignored with `--all`.
    #[arg(long, default_value_t = 1, conflicts_with = "all")]
    sample_limit: usize,
    /// Manual batch: score every session under storage (no `--session-id` / `--agent-id` required).
    #[arg(long)]
    all: bool,
    /// LLM: include up to N prior manual scores as few-shot examples per rubric.
    #[arg(long, default_value_t = 0)]
    few_shot: usize,
    /// Manual non-interactive: apply this score (0–100) to every rubric / turn (implies `--method manual`).
    #[arg(long, value_name = "N", env = "PERSISTING_JUDGE_SCORE")]
    score: Option<i64>,
    /// Manual non-interactive: verdict override (`pass` / `partial` / `fail`); default from score.
    #[arg(long, value_name = "VERDICT")]
    verdict: Option<String>,
    /// Manual non-interactive: rationale text (optional).
    #[arg(long, value_name = "TEXT")]
    rationale: Option<String>,
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

fn resolve_traj_storage_arg(storage: Option<String>) -> Result<String> {
    if let Some(s) = storage {
        return Ok(s);
    }
    let res = persisting_gateway::runtime::service::resolve_storage_detailed(None)?;
    Ok(res.storage.to_string_lossy().into_owned())
}

fn resolve_traj_ids_for_read(
    op: &str,
    storage: Option<String>,
    agent_id: Option<String>,
    session_id: Option<String>,
    root_session_id: Option<String>,
) -> Result<TrajLocation> {
    let path_arg = resolve_traj_storage_arg(storage)?;
    resolve_traj_read_location(op, path_arg, agent_id, session_id, root_session_id)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut lazy = ChronicleClient::new();
    match cli.command {
        Command::Execute(args) => dispatch_component("pvisor", &["run"], args)?,
        Command::Env(args) => dispatch_component("pvisor", &["env"], args)?,
        Command::Batch(args) => dispatch_component("ppilot", &["run"], args)?,
        Command::Query(args) => dispatch_component("ppilot", &["query"], args)?,
        Command::History(args) => run_history(&mut lazy, args)?,
        Command::Eval(args) => run_eval(&mut lazy, args)?,
        Command::Gateway(args) => run_gateway(&mut lazy, args)?,
    }
    Ok(())
}

fn dispatch_component(component: &str, prefix: &[&str], forwarded: ForwardArgs) -> Result<()> {
    let env_name = match component {
        "pvisor" => "PERSISTING_PVISOR_BIN",
        "ppilot" => "PERSISTING_PPILOT_BIN",
        _ => unreachable!("known Persisting component"),
    };
    let program = std::env::var_os(env_name)
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(|parent| parent.join(component)))
                .filter(|path| path.is_file())
        })
        .unwrap_or_else(|| PathBuf::from(component));
    let status = std::process::Command::new(&program)
        .args(prefix)
        .args(forwarded.args)
        .status()
        .with_context(|| {
            format!(
                "launch {component} component at {}; install the matching binary or set {env_name}",
                program.display()
            )
        })?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn run_eval(lazy: &mut ChronicleClient, args: EvalArgs) -> Result<()> {
    match &args.command {
        EvalCommand::Judge(args) => run_eval_judge(lazy, args),
        EvalCommand::Stats(args) => run_eval_stats(lazy, args),
    }
}

fn run_gateway(lazy: &mut ChronicleClient, args: GatewayArgs) -> Result<()> {
    match args.command {
        GatewayCommand::Serve(args) => match args.backend {
            GatewayBackend::Capture => run_capture_gateway(lazy, &args),
            GatewayBackend::Dlcapt => run_dlcapt_proxy(&args),
        },
        GatewayCommand::Start(args) => capture::daemon::cmd_start(capture::daemon::StartOptions {
            output_dir: PathBuf::from(&args.output_dir),
            config: args.config,
            debug: args.debug,
            format: args.format,
        }),
        GatewayCommand::Stop(args) => {
            capture::daemon::cmd_stop(args.output_dir.as_deref().map(Path::new))
        }
        GatewayCommand::List(args) => {
            let sessions = capture::daemon::cmd_list(args.output_dir.as_deref().map(Path::new))?;
            capture::daemon::print_list_table(&sessions);
            Ok(())
        }
        GatewayCommand::Status(args) => {
            capture::daemon::cmd_status(args.output_dir.as_deref().map(Path::new))
        }
    }
}

fn run_history_import(lazy: &mut ChronicleClient, args: &CaptureImportArgs) -> Result<()> {
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
    let summary = capture::import_to_trajectory(
        &merged.storage,
        &opts,
        |storage, agent_id, session_id, records_ronl| {
            eprintln!(
                "[persisting-cli] history import: {record_count} records -> {storage}/{agent_id}/{session_id}",
                record_count = records_ronl.lines().filter(|l| !l.trim().is_empty()).count(),
                storage = storage,
                agent_id = agent_id,
                session_id = session_id,
            );
            let payload = RequestBody::TrajectoryAppend(TrajectoryAppendRequest {
                storage: storage.to_string(),
                agent_id: agent_id.to_string(),
                session_id: session_id.to_string(),
                root_session_id: None,
                records_ronl: records_ronl.to_string(),
                storage_format: TrajectoryStorageFormat::Auto,
            });
            lazy.invoke(&payload)
        },
    )?;
    print_capture_summary(&summary, args.dry_run);
    Ok(())
}

fn capture_output_dir(args: &GatewayServeArgs) -> Option<String> {
    args.output_dir
        .clone()
        .or_else(|| std::env::var("PERSISTING_CAPTURE_STORAGE").ok())
}

fn run_capture_gateway(lazy: &mut ChronicleClient, args: &GatewayServeArgs) -> Result<()> {
    let format = args.format.unwrap_or(capture::CaptureFormat::Markdown);
    let capture_output_dir = capture_output_dir(args);
    let output_dir = capture_output_dir
        .as_deref()
        .context("gateway serve requires -o <DIR>")?;
    let config = args
        .config
        .as_deref()
        .context("gateway serve requires -c <proxy.toml>")?;
    run_capture_serve(
        lazy,
        &CaptureServeConfig {
            output_dir: output_dir.to_string(),
            config: config.to_path_buf(),
            debug: args.debug,
            format,
        },
    )
}

#[cfg(feature = "dlcapt")]
fn reject_dlcapt_capture_options(args: &GatewayServeArgs) -> Result<()> {
    if args.output_dir.is_some() {
        anyhow::bail!(
            "-o is only supported by the capture backend; configure store_dir in the dlcapt TOML"
        );
    }
    if args.format.is_some() {
        anyhow::bail!("-f is only supported by the capture backend");
    }
    if args.debug {
        anyhow::bail!("--debug is only supported by the capture backend");
    }
    Ok(())
}

#[cfg(not(feature = "dlcapt"))]
fn run_dlcapt_proxy(_args: &GatewayServeArgs) -> Result<()> {
    anyhow::bail!(
        "dlcapt backend is not included in this build; rebuild persisting-cli with --features dlcapt"
    )
}

#[cfg(feature = "dlcapt")]
fn init_dlcapt_cli_tracing_once() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "dlcapt=info,tower_http=info".into()),
        )
        .try_init();
}

#[cfg(feature = "dlcapt")]
fn run_dlcapt_proxy(args: &GatewayServeArgs) -> Result<()> {
    reject_dlcapt_capture_options(args)?;
    let config_path = args
        .config
        .as_deref()
        .context("gateway serve --backend dlcapt requires -c <dlcapt.toml>")?;
    let config = persisting_dlcapt::config::ProxyConfig::load(config_path)
        .with_context(|| format!("load dlcapt config {}", config_path.display()))?;
    init_dlcapt_cli_tracing_once();
    tokio::runtime::Runtime::new()
        .context("tokio runtime")?
        .block_on(persisting_dlcapt::serve(config))
}

struct TrajectoryAppendJob {
    storage: String,
    agent_id: String,
    session_id: String,
    root_session_id: Option<String>,
    record: persisting_gateway::record::CaptureRecord,
}

#[derive(Hash, Eq, PartialEq, Clone)]
struct TrajectoryBatchKey {
    storage: String,
    agent_id: String,
    session_id: String,
    root_session_id: Option<String>,
}

const CAPTURE_TRAJECTORY_BATCH: usize = 32;

fn should_flush_capture_record(record: &persisting_gateway::record::CaptureRecord) -> bool {
    matches!(
        record.kind.as_str(),
        "llm.request" | "llm.response" | "llm.spawn_link" | "session.started" | "session.ended"
    )
}

fn records_ronl_from_lines(lines: &[String]) -> String {
    if lines.len() == 1 {
        format!("{}\n", lines[0])
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

fn write_trajectory_dead_letter(key: &TrajectoryBatchKey, lines: &[String], error: &str) {
    let storage_path = std::path::Path::new(&key.storage);
    let records_ronl = records_ronl_from_lines(lines);
    if let Err(dl) = persisting_gateway::dead_letter::append_trajectory_dead_letter(
        storage_path,
        &key.agent_id,
        &key.session_id,
        key.root_session_id.as_deref(),
        &records_ronl,
        error,
    ) {
        eprintln!("[persisting-cli] trajectory dead letter write failed: {dl:#}");
    }
}

fn flush_capture_trajectory_batch_or_dead_letter(
    chronicle: &Chronicle,
    key: &TrajectoryBatchKey,
    lines: &[String],
) {
    if lines.is_empty() {
        return;
    }
    if let Err(e) = flush_capture_trajectory_batch(chronicle, key, lines) {
        write_trajectory_dead_letter(key, lines, &format!("{e:#}"));
        eprintln!("[persisting-cli] capture trajectory append failed: {e:#}");
    }
}

fn flush_capture_trajectory_batch(
    chronicle: &Chronicle,
    key: &TrajectoryBatchKey,
    lines: &[String],
) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let records_ronl = records_ronl_from_lines(lines);
    let payload = RequestBody::TrajectoryAppend(TrajectoryAppendRequest {
        storage: key.storage.clone(),
        agent_id: key.agent_id.clone(),
        session_id: key.session_id.clone(),
        root_session_id: key.root_session_id.clone(),
        records_ronl,
        storage_format: TrajectoryStorageFormat::Lance,
    });
    let response = chronicle.invoke(&payload)?;
    if !matches!(response, ResponseBody::TrajectoryAppend(_)) {
        anyhow::bail!("unexpected pChronicle response: {response:?}");
    }
    Ok(())
}

fn build_capture_trajectory_sink(
    storage: String,
    agent_id: String,
) -> Result<(
    std::sync::Arc<dyn persisting_gateway::sink::CaptureEventSink>,
    TrajectoryAppendWorker,
)> {
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

        let chronicle = Chronicle;
        let mut batches: HashMap<TrajectoryBatchKey, Vec<String>> = HashMap::new();

        while let Ok(job) = job_rx.recv() {
            let result = (|| -> Result<(), anyhow::Error> {
                let key = TrajectoryBatchKey {
                    storage: job.storage.clone(),
                    agent_id: job.agent_id,
                    session_id: job.session_id,
                    root_session_id: job.root_session_id,
                };
                let line =
                    persisting_pchronicle::encode_event_lines(std::slice::from_ref(&job.record))?
                        .into_iter()
                        .next()
                        .context("encode capture event produced no line")?;
                let flush_now = should_flush_capture_record(&job.record);
                let batch = batches.entry(key.clone()).or_default();
                batch.push(line);
                if batch.len() >= CAPTURE_TRAJECTORY_BATCH || flush_now {
                    let lines = batches.remove(&key).unwrap_or_default();
                    flush_capture_trajectory_batch_or_dead_letter(&chronicle, &key, &lines);
                }
                Ok(())
            })();
            if let Err(e) = result {
                eprintln!("[persisting-cli] capture trajectory append failed: {e:#}");
            }
        }

        for (key, lines) in batches {
            flush_capture_trajectory_batch_or_dead_letter(&chronicle, &key, &lines);
        }
    });

    let sink_storage = storage;
    let callback_sink = std::sync::Arc::new(persisting_gateway::sink::CallbackSink::new(
        agent_id,
        move |route, agent_id, record| {
            tx.send(TrajectoryAppendJob {
                storage: sink_storage.clone(),
                agent_id: agent_id.to_string(),
                session_id: route.storage_session_id.clone(),
                root_session_id: route.append_root_session(),
                record,
            })
            .map_err(|e| anyhow::anyhow!("pChronicle append channel closed: {e}"))?;
            Ok(())
        },
    ));
    let sink: std::sync::Arc<dyn persisting_gateway::sink::CaptureEventSink> = callback_sink;
    Ok((
        sink,
        TrajectoryAppendWorker {
            job_tx: Some(job_tx),
            join: Some(join),
        },
    ))
}

fn load_storage_agent_id(storage: &Path) -> String {
    for name in ["proxy.toml", "proxy.yaml"] {
        let path = storage.join(name);
        if path.is_file() {
            if let Ok(cfg) = persisting_gateway::config::ProxyConfig::from_file(&path) {
                return cfg.agent_id;
            }
        }
    }
    if let Ok(Some(state)) = persisting_gateway::runtime::service::CaptureDaemonState::read(storage)
    {
        if let Ok(cfg) =
            persisting_gateway::config::ProxyConfig::from_file(Path::new(&state.config_path))
        {
            return cfg.agent_id;
        }
    }
    "capture".into()
}

fn run_replay_dead_letter(
    _chronicle: &mut ChronicleClient,
    args: &CaptureReplayDeadLetterArgs,
) -> Result<()> {
    let storage = PathBuf::from(&args.output_dir);
    let storage = storage.canonicalize().unwrap_or(storage);
    let agent_id = load_storage_agent_id(&storage);
    let (sink, mut worker) =
        build_capture_trajectory_sink(storage.display().to_string(), agent_id)?;
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

fn run_capture_serve(_chronicle: &mut ChronicleClient, args: &CaptureServeConfig) -> Result<()> {
    let storage_path = PathBuf::from(&args.output_dir);
    let _run_session =
        persisting_gateway::runtime::run_env::ensure_serve_run_session(&storage_path)
            .with_context(|| format!("ensure serve run_session for {}", storage_path.display()))?;
    let applied = persisting_gateway::runtime::run_env::apply_daemon_env(&storage_path)
        .with_context(|| format!("apply daemon env snapshot for {}", storage_path.display()))?;
    if !applied.is_empty() {
        eprintln!(
            "[persisting-cli] gateway: applied daemon env snapshot ({} keys: {})",
            applied.len(),
            applied.join(", ")
        );
    }

    let config = persisting_gateway::config::ProxyConfig::from_file(&args.config)
        .with_context(|| format!("load proxy config {}", args.config.display()))?;

    capture::enable_capture_debug(
        &capture::CaptureDebugContext {
            storage: &storage_path,
            applied_env_keys: &applied,
        },
        args.debug,
    )?;

    capture::usage::eprint_serve_banner(&capture::usage::ServeBanner {
        listen: &config.listen,
        admin_listen: &config.admin_listen,
        output_dir: &storage_path,
        agent_id: &config.agent_id,
        format: args.format,
        background: false,
        pid: None,
    });

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let (sink, mut worker) =
        build_capture_trajectory_sink(args.output_dir.clone(), config.agent_id.clone())?;

    let rt = tokio::runtime::Runtime::new().context("tokio runtime")?;
    rt.block_on(persisting_gateway::serve(
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
        "[persisting-cli] history import {mode}: {} records @ {} (agent_id={} session_id={})",
        summary.record_count, summary.storage, summary.agent_id, summary.session_id
    );
    for (src, n) in &summary.sources {
        eprintln!("[persisting-cli] capture   source {src}: {n}");
    }
}

fn invoke_trajectory_stats(
    lazy: &mut ChronicleClient,
    req: TrajectoryStatsRequest,
) -> Result<TrajectoryStatsResponse> {
    let payload = RequestBody::TrajectoryStats(req);
    match lazy.invoke_silent(&payload)? {
        ResponseBody::TrajectoryStats(r) => Ok(r),
        other => anyhow::bail!("unexpected pChronicle response: {other:?}"),
    }
}

fn invoke_trajectory_judge(
    lazy: &mut ChronicleClient,
    req: TrajectoryJudgeRequest,
) -> Result<TrajectoryJudgeResponse> {
    let payload = RequestBody::TrajectoryJudge(req);
    match lazy.invoke_silent(&payload)? {
        ResponseBody::TrajectoryJudge(r) => Ok(r),
        other => anyhow::bail!("unexpected pChronicle response: {other:?}"),
    }
}

fn invoke_trajectory_judge_stats(
    lazy: &mut ChronicleClient,
    req: TrajectoryJudgeStatsRequest,
) -> Result<TrajectoryJudgeStatsResponse> {
    let payload = RequestBody::TrajectoryJudgeStats(req);
    match lazy.invoke_silent(&payload)? {
        ResponseBody::TrajectoryJudgeStats(r) => Ok(r),
        other => anyhow::bail!("unexpected pChronicle response: {other:?}"),
    }
}

fn invoke_trajectory_replay(
    lazy: &mut ChronicleClient,
    req: TrajectoryReplayRequest,
) -> Result<TrajectoryReplayResponse> {
    let payload = RequestBody::TrajectoryReplay(req);
    match lazy.invoke_silent(&payload)? {
        ResponseBody::TrajectoryReplay(r) => Ok(r),
        other => anyhow::bail!("unexpected pChronicle response: {other:?}"),
    }
}

fn run_trajectory_stats_detail(
    lazy: &mut ChronicleClient,
    loc: &TrajLocation,
    storage_format: TrajectoryStorageCli,
    backend: ResolvedStatsOutputBackend,
) -> Result<()> {
    let stats = invoke_trajectory_stats(
        lazy,
        TrajectoryStatsRequest {
            storage: loc.storage.clone(),
            agent_id: loc.agent_id.clone(),
            session_id: loc.session_id.clone(),
            storage_format: storage_format.into(),
            root_session_id: loc.root_session_id.clone(),
        },
    )?;
    if stats.status != "ok" {
        return print_trajectory_stats_summary(&stats, backend);
    }
    let replay_format = storage_format.into();
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
            storage_format: replay_format,
            root_session_id: loc.root_session_id.clone(),
        },
    )?;
    let storage = loc.storage.clone();
    let agent_id = loc.agent_id.clone();
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
    print_trajectory_stats_detail(&stats, &tree, Some(&loc.agent_id), backend)
}

fn stats_detail_section_label(loc: &TrajLocation) -> String {
    match loc.root_session_id.as_deref() {
        Some(root) if root != loc.session_id => {
            format!("{} / {} / {}", loc.agent_id, root, loc.session_id)
        }
        _ => format!("{} / {}", loc.agent_id, loc.session_id),
    }
}

fn run_history(lazy: &mut ChronicleClient, args: HistoryArgs) -> Result<()> {
    match &args.command {
        HistoryCommand::Import(args) => run_history_import(lazy, args)?,
        HistoryCommand::ReplayDeadLetter(args) => run_replay_dead_letter(lazy, args)?,
        HistoryCommand::Add(args) => {
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
                "[persisting-cli] trajectory add: read {} bytes from {:?} (format={input_format:?} storage={storage_format:?}), converting…",
                raw.len(),
                args.input,
            );
            let records_ronl = TrajectoryFormatManager::prepare_append_batch(input_format, &raw)
                .context("normalize trajectory add input")?;
            eprintln!(
                "[persisting-cli] trajectory add: {} bytes internal payload…",
                records_ronl.len()
            );
            let payload = RequestBody::TrajectoryAppend(TrajectoryAppendRequest {
                storage: args.storage.clone(),
                agent_id,
                session_id,
                root_session_id: None,
                records_ronl,
                storage_format,
            });
            eprintln!("[persisting-cli] trajectory add: writing through pChronicle…");
            lazy.invoke(&payload)?;
            eprintln!("[persisting-cli] trajectory add: pChronicle returned");
        }
        HistoryCommand::Truncate(args) => {
            let loc = resolve_traj_ids_for_read(
                "trajectory truncate",
                args.storage.clone(),
                args.agent_id.clone(),
                args.session_id.clone(),
                args.root_session_id.clone(),
            )?;
            let payload = RequestBody::TrajectoryTruncate(TrajectoryTruncateRequest {
                storage: loc.storage,
                agent_id: loc.agent_id,
                session_id: loc.session_id,
                root_session_id: loc.root_session_id,
                keep_rows: args.keep_rows,
            });
            lazy.invoke(&payload)?;
        }
        HistoryCommand::Extract(args) => {
            let loc = resolve_traj_ids_for_read(
                "trajectory extract",
                Some(args.storage.clone()),
                args.agent_id.clone(),
                args.session_id.clone(),
                args.root_session_id.clone(),
            )?;
            let payload = RequestBody::TrajectoryExtract(TrajectoryExtractRequest {
                storage: loc.storage,
                agent_id: loc.agent_id,
                session_id: loc.session_id,
                root_session_id: loc.root_session_id,
                out_dir: args.out_dir.clone(),
                include_subagents: args.include_subagents,
            });
            lazy.invoke(&payload)?;
        }
        HistoryCommand::Replay(args) => {
            let loc = resolve_traj_ids_for_read(
                "trajectory replay",
                args.storage.clone(),
                args.agent_id.clone(),
                args.session_id.clone(),
                args.root_session_id.clone(),
            )?;
            let payload = RequestBody::TrajectoryReplay(TrajectoryReplayRequest {
                storage: loc.storage,
                agent_id: loc.agent_id,
                session_id: loc.session_id,
                offset: args.offset,
                limit: args.limit,
                storage_format: args.storage_format.into(),
                root_session_id: loc.root_session_id,
            });
            lazy.invoke(&payload)?;
        }
        HistoryCommand::Stats(args) => {
            let path_arg = resolve_traj_storage_arg(args.storage.clone())?;
            let backend = args.output.resolve();
            let mut locations = list_traj_read_locations(
                path_arg.clone(),
                args.agent_id.clone(),
                args.session_id.clone(),
                args.root_session_id.clone(),
            )?;
            if args.session_id.is_none() {
                locations = expand_story_locations_blocking(locations)?;
                locations = drop_lifecycle_run_partitions(locations);
            }
            if locations.is_empty() {
                anyhow::bail!("trajectory stats: no sessions found under {path_arg}");
            }
            if args.detail && !supports_detail_tree(backend) {
                anyhow::bail!(
                    "--detail requires --output plain or md (toml/json are summary-only)"
                );
            }
            if args.detail {
                for (i, loc) in locations.iter().enumerate() {
                    if i > 0 {
                        println!();
                    }
                    if locations.len() > 1 {
                        print_stats_section_divider(&stats_detail_section_label(loc), backend);
                    }
                    run_trajectory_stats_detail(lazy, loc, args.storage_format, backend)?;
                }
            } else if locations.len() == 1 {
                let loc = &locations[0];
                let stats = invoke_trajectory_stats(
                    lazy,
                    TrajectoryStatsRequest {
                        storage: loc.storage.clone(),
                        agent_id: loc.agent_id.clone(),
                        session_id: loc.session_id.clone(),
                        storage_format: args.storage_format.into(),
                        root_session_id: loc.root_session_id.clone(),
                    },
                )?;
                print_trajectory_stats_summary(&stats, backend)?;
            } else {
                let storage_format = args.storage_format.into();
                let mut rows = Vec::with_capacity(locations.len());
                for loc in &locations {
                    rows.push(invoke_trajectory_stats(
                        lazy,
                        TrajectoryStatsRequest {
                            storage: loc.storage.clone(),
                            agent_id: loc.agent_id.clone(),
                            session_id: loc.session_id.clone(),
                            storage_format,
                            root_session_id: loc.root_session_id.clone(),
                        },
                    )?);
                }
                let judge_agg = invoke_trajectory_judge_stats(
                    lazy,
                    TrajectoryJudgeStatsRequest {
                        storage: path_arg.clone(),
                        agent_id: args.agent_id.clone(),
                        session_id: args.session_id.clone(),
                        root_session_id: args.root_session_id.clone(),
                    },
                )
                .ok();
                print_trajectory_stats_list(
                    &locations[0].storage,
                    &rows,
                    judge_agg.as_ref(),
                    backend,
                )?;
            }
        }
        HistoryCommand::Materialize(args) => {
            let storage = resolve_traj_storage_arg(args.storage.clone())?;
            let (agent_id, session_id) =
                resolve_traj_ids_for_write(args.agent_id.clone(), args.session_id.clone())?;
            let payload = RequestBody::TrajectoryMaterialize(TrajectoryMaterializeRequest {
                storage,
                agent_id,
                session_id,
                root_session_id: args.root_session_id.clone(),
            });
            lazy.invoke(&payload)?;
        }
        HistoryCommand::Convert(args) => trajectory_convert::run_traj_convert(args)?,
    }
    Ok(())
}

fn run_eval_stats(lazy: &mut ChronicleClient, args: &TrajectoryJudgeStatsArgs) -> Result<()> {
    let storage = resolve_traj_storage_arg(args.storage.clone())?;
    let req = TrajectoryJudgeStatsRequest {
        storage,
        agent_id: args.agent_id.clone(),
        session_id: args.session_id.clone(),
        root_session_id: args.root_session_id.clone(),
    };
    let payload = RequestBody::TrajectoryJudgeStats(req);
    lazy.invoke(&payload)?;
    Ok(())
}

fn resolve_judge_rubrics(args: &TrajectoryJudgeArgs) -> Vec<String> {
    if !args.rubrics.is_empty() {
        return args.rubrics.clone();
    }
    vec![args.rubric_id.clone()]
}

fn resolve_judge_locations(
    args: &TrajectoryJudgeArgs,
    method: JudgeMethod,
) -> Result<Vec<TrajLocation>> {
    if args.session_id.is_some() {
        return Ok(vec![resolve_traj_ids_for_read(
            "trajectory judge",
            args.storage.clone(),
            args.agent_id.clone(),
            args.session_id.clone(),
            args.root_session_id.clone(),
        )?]);
    }

    if method != JudgeMethod::Manual {
        anyhow::bail!(
            "LLM judge requires --session-id; use --method manual to score storage without session ids"
        );
    }

    let path = resolve_traj_storage_arg(args.storage.clone())?;
    let mut locs = list_traj_read_locations(
        path,
        args.agent_id.clone(),
        None,
        args.root_session_id.clone(),
    )?;
    if locs.is_empty() {
        anyhow::bail!("no trajectory sessions found for manual judge");
    }
    locs = expand_story_locations_blocking(locs)?;
    locs = drop_lifecycle_run_partitions(locs);
    if locs.is_empty() {
        anyhow::bail!("no scorable sessions after expanding Lance partitions");
    }

    if args.all {
        let mode = args
            .sample
            .map(JudgeSampleMode::from)
            .unwrap_or(JudgeSampleMode::Sequential);
        return Ok(judge_manual::sample_locations(locs, mode, 0));
    }

    let mode = args
        .sample
        .map(JudgeSampleMode::from)
        .unwrap_or(JudgeSampleMode::Random);
    let limit = args.sample_limit.max(1);
    Ok(judge_manual::sample_locations(locs, mode, limit))
}

fn run_eval_judge(lazy: &mut ChronicleClient, args: &TrajectoryJudgeArgs) -> Result<()> {
    let rubric_ids = resolve_judge_rubrics(args);
    let scope: JudgeScope = args.scope.into();
    let method: JudgeMethod = if args.score.is_some() {
        JudgeMethod::Manual
    } else {
        args.method.into()
    };
    if args.score.is_some() && args.dry_run {
        anyhow::bail!("--score cannot be combined with --dry-run");
    }

    let locations = resolve_judge_locations(args, method)?;

    if locations.is_empty() {
        anyhow::bail!("no sessions matched for judge");
    }

    let total = locations.len();
    let mode_label = if args.score.is_some() {
        "fixed-score"
    } else if method == JudgeMethod::Manual {
        "manual"
    } else {
        "LLM"
    };
    eprintln!(
        "{mode_label} judge: {total} session(s) to score ({}{})",
        if args.all {
            "all in storage"
        } else {
            "sampled"
        },
        if args.agent_id.is_some() {
            format!(", agent={}", args.agent_id.as_deref().unwrap())
        } else {
            String::new()
        }
    );
    if let Some(score) = args.score {
        eprintln!(
            "  fixed score={score} scope={scope:?} rubrics={}",
            rubric_ids.join(",")
        );
    }

    let mut ok = 0usize;
    let mut skipped = 0usize;
    for (idx, loc) in locations.iter().enumerate() {
        eprintln!(
            "\n>>> [{}/{}] judge {} / {} ({:?}/{:?})",
            idx + 1,
            total,
            loc.agent_id,
            loc.session_id,
            method,
            scope
        );

        match run_eval_judge_one(lazy, loc, args, &rubric_ids, scope, method, total == 1) {
            Ok(out) if out.judged_calls > 0 => ok += 1,
            Ok(out) => {
                eprintln!(
                    "  skip {} / {}: already judged ({} unit(s); use --force to re-score)",
                    loc.agent_id, loc.session_id, out.skipped_calls
                );
                skipped += 1;
            }
            Err(e) => {
                eprintln!("  skip {} / {}: {e:#}", loc.agent_id, loc.session_id);
                skipped += 1;
                if total == 1 && args.session_id.is_some() {
                    return Err(e);
                }
            }
        }
    }

    if ok == 0 {
        anyhow::bail!(
            "no sessions scored ({skipped} skipped); try --scope story or check capture has dialogue turns"
        );
    }
    if total > 1 || skipped > 0 {
        eprintln!("\nDone: {ok} scored, {skipped} skipped (of {total} sessions).");
    }
    Ok(())
}

fn dialogue_turn_count(story: &persisting_gateway::engine::Story) -> usize {
    story
        .turns
        .iter()
        .filter(|t| t.kind == TurnKind::Dialogue)
        .filter(|t| {
            t.user.as_ref().is_some_and(|u| u.call_id.is_some())
                || t.assistant.as_ref().is_some_and(|a| a.call_id.is_some())
        })
        .count()
}

fn manual_judge_incomplete(
    lazy: &mut ChronicleClient,
    loc: &TrajLocation,
    scope: JudgeScope,
    rubrics: &[String],
    story: &persisting_gateway::engine::Story,
) -> Result<bool> {
    let js = invoke_trajectory_judge_stats(
        lazy,
        TrajectoryJudgeStatsRequest {
            storage: loc.storage.clone(),
            agent_id: Some(loc.agent_id.clone()),
            session_id: Some(loc.session_id.clone()),
            root_session_id: loc.root_session_id.clone(),
        },
    )?;
    let session = js
        .sessions
        .iter()
        .find(|s| s.session_id == loc.session_id)
        .or_else(|| js.sessions.first());
    let Some(session) = session else {
        return Ok(true);
    };
    Ok(match scope {
        JudgeScope::Story => session.story_judgments < rubrics.len(),
        JudgeScope::Turn => {
            let turns = dialogue_turn_count(story);
            session.turn_judgments < turns.saturating_mul(rubrics.len())
        }
    })
}

fn run_eval_judge_one(
    lazy: &mut ChronicleClient,
    loc: &TrajLocation,
    args: &TrajectoryJudgeArgs,
    rubric_ids: &[String],
    scope: JudgeScope,
    method: JudgeMethod,
    print_response: bool,
) -> Result<TrajectoryJudgeResponse> {
    let manual_scores = if method == JudgeMethod::Manual {
        let replay = invoke_trajectory_replay(
            lazy,
            TrajectoryReplayRequest {
                storage: loc.storage.clone(),
                agent_id: loc.agent_id.clone(),
                session_id: loc.session_id.clone(),
                offset: 0,
                limit: None,
                storage_format: TrajectoryStorageFormat::Lance,
                root_session_id: loc.root_session_id.clone(),
            },
        )?;
        let root = loc
            .root_session_id
            .as_deref()
            .unwrap_or(loc.session_id.as_str());
        let story = judge_manual::story_from_replay_json(&replay.records, &loc.session_id, root)?;
        if !args.force && !manual_judge_incomplete(lazy, loc, scope, rubric_ids, &story)? {
            eprintln!("  already judged, skipping prompts (use --force to re-score)");
            Vec::new()
        } else if let Some(score) = args.score {
            judge_manual::fixed_manual_scores(
                &story,
                scope,
                rubric_ids,
                score,
                args.verdict.as_deref(),
                args.rationale.as_deref(),
            )?
        } else {
            judge_manual::collect_manual_scores(&story, scope, rubric_ids)?
        }
    } else {
        Vec::new()
    };

    let req = TrajectoryJudgeRequest {
        storage: loc.storage.clone(),
        agent_id: loc.agent_id.clone(),
        session_id: loc.session_id.clone(),
        root_session_id: loc.root_session_id.clone(),
        rubric_id: args.rubric_id.clone(),
        rubric_ids: rubric_ids.to_vec(),
        scope,
        method,
        model: args.model.clone(),
        dry_run: args.dry_run,
        force: args.force,
        manual_scores,
        few_shot_limit: args.few_shot,
    };
    let out = invoke_trajectory_judge(lazy, req)?;
    if print_response {
        print_trajectory_judge_as_toml(&out)?;
    }
    Ok(out)
}

fn read_input(path: &str) -> Result<String> {
    if path == "-" {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        return Ok(buffer);
    }
    Ok(fs::read_to_string(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_command_tree_parses() {
        for args in [
            vec!["persisting", "execute", "--", "/bin/true"],
            vec!["persisting", "exec", "--", "/bin/true"],
            vec!["persisting", "env", "list"],
            vec!["persisting", "environment", "status", "demo"],
            vec!["persisting", "batch", "plan.py", "--workers", "2"],
            vec!["persisting", "query", "input.jsonl", "--sql", "SELECT 1"],
            vec!["persisting", "history", "stats", "./store"],
            vec!["persisting", "eval", "stats", "./store"],
            vec!["persisting", "gateway", "status"],
        ] {
            Cli::try_parse_from(args).expect("valid unified Persisting command");
        }
    }

    #[test]
    fn removed_trajectory_compatibility_commands_are_rejected() {
        assert!(Cli::try_parse_from(["persisting", "traj", "stats", "./store"]).is_err());
        assert!(Cli::try_parse_from(["persisting", "trajectory", "stats", "./store"]).is_err());
    }

    #[test]
    fn removed_search_command_is_rejected() {
        assert!(Cli::try_parse_from(["persisting", "search", "query", "./store", "text"]).is_err());
    }

    fn capture_record(kind: &str) -> persisting_gateway::record::CaptureRecord {
        persisting_gateway::record::CaptureRecord {
            identity: Default::default(),
            seq: 0,
            source: "test".to_string(),
            kind: kind.to_string(),
            timestamp: None,
            session_id: None,
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({}),
        }
    }

    #[test]
    fn capture_stream_records_use_batch_flush() {
        assert!(!should_flush_capture_record(&capture_record(
            "llm.response.stream"
        )));
        assert!(should_flush_capture_record(&capture_record("llm.response")));
        assert!(should_flush_capture_record(&capture_record(
            "session.ended"
        )));
    }
}
