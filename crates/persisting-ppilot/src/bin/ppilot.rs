use std::path::PathBuf;
use std::process::ExitCode;
use std::{fs, io::Write};

use anyhow::{Context, Result};
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use persisting_ppilot::{
    init_tracing_with_verbose, process_federated_count, process_script, process_trajectories,
    produce_from_planner, produce_trajectories, run_chronicle, run_convert, run_ppilot, run_query,
    run_self_test, AnalysisOutputFormat, BatchAnalysisOptions, BatchProductionManifest,
    BatchProductionOptions, ChronicleArgs, ConvertArgs, CountTable, FederatedCountOptions,
    PPilotArgs, ProcessScriptOptions, QueryArgs,
};

#[derive(Debug, Parser)]
#[command(
    name = "ppilot",
    version,
    about = "Run, inspect, and analyze durable agent workloads"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a pPilot plan with bounded concurrency and durable resume.
    Run(Box<PPilotArgs>),
    /// Query pChronicle by SQL, point, batch, or live follow mode.
    Query(Box<QueryArgs>),
    /// Import and operate pChronicle trajectory stores.
    Chronicle(ChronicleArgs),
    /// Convert trajectory corpora between pChronicle formats.
    Convert(ConvertArgs),
    /// Stream Runs from a Python planner into independent pVisor workspaces.
    Produce(ProduceArgs),
    /// Run read-only SQL over automatically sharded ATIF trajectories.
    Analysis(AnalysisArgs),
    /// Process and aggregate ATIF trajectories across Pulsing workers.
    Process(ProcessArgs),
    /// Run the built-in plan/execute smoke test.
    SelfTest(SelfTestArgs),
}

#[derive(Debug, Args)]
struct ProduceArgs {
    /// Python planner defining plan(); .json manifests remain accepted for compatibility.
    #[arg(value_name = "PLANNER")]
    planner: PathBuf,
    /// Root containing one durable pVisor workspace per Run.
    #[arg(short, long, value_name = "DIR")]
    output: PathBuf,
    /// Maximum concurrent pVisor Runs.
    #[arg(short = 'j', long, default_value_t = 4)]
    parallelism: usize,
    /// Disable the capture Gateway (mainly for local diagnostics).
    #[arg(long)]
    no_capture: bool,
    /// Job-scoped aggregate rate delivered through the embedded Supervisor.
    #[arg(long, value_name = "RATE", value_parser = persisting_ppilot::parse_bandwidth)]
    cluster_network_limit: Option<u64>,
    /// Python interpreter used to evaluate the planner.
    #[arg(long, env = "PERSISTING_PYTHON", default_value = "python3")]
    python: PathBuf,
    /// Stable batch identifier; defaults to the planner filename stem.
    #[arg(long, value_name = "ID")]
    batch_id: Option<String>,
    /// Arguments forwarded to the planner after `--`.
    #[arg(last = true, value_name = "ARG")]
    planner_args: Vec<String>,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("sql_input")
        .required(true)
        .multiple(false)
        .args(["sql", "sql_file"])
))]
struct AnalysisArgs {
    /// ATIF JSON/JSONL file or directory.
    #[arg(value_name = "INPUT")]
    input: PathBuf,
    /// Output directory for shard files, combined result, and report.
    /// Without this option, only the combined result is written to stdout.
    #[arg(short, long, value_name = "DIR")]
    output: Option<PathBuf>,
    /// Number of automatic data shards processed concurrently.
    #[arg(short = 'j', long, default_value_t = 4)]
    parallelism: usize,
    /// Read-only SQL executed independently on every shard.
    #[arg(long, short = 'q', value_name = "SQL")]
    sql: Option<String>,
    /// Read read-only SQL from a UTF-8 file.
    #[arg(long, value_name = "FILE")]
    sql_file: Option<PathBuf>,
    /// Combined result format for stdout or results.<format>.
    #[arg(long, value_enum, default_value_t = AnalysisOutputFormat::Jsonl)]
    fmt: AnalysisOutputFormat,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("processor")
        .required(true)
        .multiple(false)
        .args(["script", "count"])
))]
struct ProcessArgs {
    /// ATIF JSON/JSONL file or directory.
    #[arg(value_name = "INPUT")]
    input: PathBuf,
    /// Output directory for results and report; stdout is used when omitted.
    #[arg(short, long, value_name = "DIR")]
    output: Option<PathBuf>,
    /// Python map/reduce script transferred to every mapper worker.
    #[arg(long, value_name = "FILE")]
    script: Option<PathBuf>,
    /// Number of deterministic mapper shards.
    #[arg(short = 'j', long, alias = "parallelism", default_value_t = 4)]
    mappers: usize,
    /// Python interpreter available on every mapper node.
    #[arg(long, env = "PERSISTING_PYTHON", default_value = "python3")]
    python: PathBuf,
    /// Two-level federated count metric over normalized pChronicle data.
    #[arg(long, value_enum, value_name = "METRIC")]
    count: Option<CountTableArg>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CountTableArg {
    Runs,
    Steps,
    ToolCalls,
    LlmCalls,
    CopiedContextSteps,
}

impl From<CountTableArg> for CountTable {
    fn from(value: CountTableArg) -> Self {
        match value {
            CountTableArg::Runs => Self::Runs,
            CountTableArg::Steps => Self::Steps,
            CountTableArg::ToolCalls => Self::ToolCalls,
            CountTableArg::LlmCalls => Self::LlmCalls,
            CountTableArg::CopiedContextSteps => Self::CopiedContextSteps,
        }
    }
}

#[derive(Debug, Args)]
struct SelfTestArgs {
    /// Python interpreter used by the smoke plan.
    #[arg(long, env = "PERSISTING_PYTHON", default_value = "python3")]
    python: PathBuf,

    #[arg(short = 'w', long, default_value_t = 4)]
    workers: usize,

    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let verbose = match &cli.command {
        Command::Run(args) => args.verbose,
        Command::SelfTest(args) => args.verbose,
        Command::Query(_) => false,
        Command::Chronicle(_)
        | Command::Convert(_)
        | Command::Produce(_)
        | Command::Analysis(_)
        | Command::Process(_) => false,
    };
    init_tracing_with_verbose(verbose);

    match dispatch(cli.command).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(command: Command) -> Result<ExitCode> {
    match command {
        Command::Run(args) => run_ppilot(*args).await,
        Command::Query(args) => {
            run_query(*args).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Chronicle(args) => {
            run_chronicle(args).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Convert(args) => {
            run_convert(args).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Produce(args) => {
            let options = BatchProductionOptions {
                output_dir: args.output,
                parallelism: args.parallelism,
                capture_gateway: !args.no_capture,
                supervisor_network_limit_bytes_per_second: args.cluster_network_limit,
            };
            let is_legacy_json = args
                .planner
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
            let report = if is_legacy_json {
                if !args.planner_args.is_empty() {
                    anyhow::bail!("legacy JSON manifests do not accept planner arguments");
                }
                let mut manifest = BatchProductionManifest::from_path(&args.planner)?;
                if let Some(batch_id) = args.batch_id {
                    manifest.batch_id = batch_id;
                }
                produce_trajectories(manifest, options).await?
            } else {
                let batch_id = args.batch_id.unwrap_or_else(|| {
                    args.planner
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or("production")
                        .to_owned()
                });
                produce_from_planner(
                    args.planner,
                    args.python,
                    args.planner_args,
                    batch_id,
                    options,
                )
                .await?
            };
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(if report.failed == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Command::Analysis(args) => {
            let sql = match (args.sql, args.sql_file) {
                (Some(sql), None) => sql,
                (None, Some(path)) => std::fs::read_to_string(&path)
                    .with_context(|| format!("read SQL file {}", path.display()))?,
                _ => unreachable!("clap requires exactly one SQL input"),
            };
            let temporary = args
                .output
                .is_none()
                .then(tempfile::tempdir)
                .transpose()
                .context("create temporary analysis output")?;
            let output_dir = args.output.clone().unwrap_or_else(|| {
                temporary
                    .as_ref()
                    .expect("temporary output exists")
                    .path()
                    .to_path_buf()
            });
            let report = process_trajectories(BatchAnalysisOptions {
                input: args.input,
                sql,
                output_dir,
                parallelism: args.parallelism,
                format: args.fmt,
            })
            .await?;
            if args.output.is_none() {
                let bytes = fs::read(&report.output)
                    .with_context(|| format!("read analysis result {}", report.output.display()))?;
                std::io::stdout()
                    .lock()
                    .write_all(&bytes)
                    .context("write analysis result to stdout")?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Process(args) => {
            match (args.script, args.count) {
                (Some(script), None) => {
                    if let Some(report) = process_script(ProcessScriptOptions {
                        input: args.input,
                        script,
                        output_dir: args.output.clone(),
                        mappers: args.mappers,
                        python: args.python,
                    })
                    .await?
                    {
                        if args.output.is_none() {
                            println!("{}", serde_json::to_string_pretty(&report.result)?);
                        }
                    }
                }
                (None, Some(table)) => {
                    let temporary = args
                        .output
                        .is_none()
                        .then(tempfile::tempdir)
                        .transpose()
                        .context("create temporary process output")?;
                    let output_dir = args.output.clone().unwrap_or_else(|| {
                        temporary
                            .as_ref()
                            .expect("temporary process output exists")
                            .path()
                            .to_path_buf()
                    });
                    if let Some(report) = process_federated_count(FederatedCountOptions {
                        input: args.input,
                        output_dir,
                        parallelism: args.mappers,
                        table: table.into(),
                    })
                    .await?
                    {
                        if args.output.is_none() {
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&serde_json::json!({
                                    "metric": report.table,
                                    "count": report.count,
                                }))?
                            );
                        }
                    }
                }
                _ => unreachable!("clap requires exactly one process mode"),
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::SelfTest(args) => {
            let report = run_self_test(args.python, args.workers, args.verbose).await?;
            Ok(if report.passed() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_public_subcommands() {
        let legacy_query = Cli::try_parse_from([
            "ppilot",
            "query",
            "input.jsonl",
            "--source",
            "atif",
            "--sql",
            "SELECT 1",
        ])
        .unwrap();
        assert!(matches!(legacy_query.command, Command::Query(_)));

        let sql = Cli::try_parse_from([
            "ppilot",
            "query",
            "sql",
            "input.jsonl",
            "--source",
            "atif",
            "--max-files",
            "100",
            "--max-entries",
            "200",
            "--max-file-bytes",
            "1048576",
            "--max-record-bytes",
            "524288",
            "--max-concurrent-files",
            "2",
            "--memory-limit-bytes",
            "67108864",
            "--max-spill-bytes",
            "134217728",
            "--timeout-seconds",
            "30",
            "--max-output-rows",
            "1000",
            "--content-read-mode",
            "preview",
            "--query-metrics",
            "--sql",
            "SELECT 1",
        ])
        .unwrap();
        let Command::Query(sql) = sql.command else {
            panic!("expected query command");
        };
        assert!(matches!(
            sql.command,
            Some(persisting_ppilot::QueryCommand::Sql(
                persisting_ppilot::SqlQueryArgs {
                    max_files: 100,
                    max_entries: 200,
                    max_file_bytes: 1_048_576,
                    max_record_bytes: 524_288,
                    max_concurrent_files: Some(2),
                    memory_limit_bytes: Some(67_108_864),
                    max_spill_bytes: Some(134_217_728),
                    timeout_seconds: Some(30),
                    max_output_rows: Some(1000),
                    content_read_mode: persisting_ppilot::QueryContentReadMode::Preview,
                    query_metrics: true,
                    ..
                }
            ))
        ));

        let point = Cli::try_parse_from([
            "ppilot",
            "query",
            "point",
            "storyline-store",
            "--session-id",
            "run-a",
            "--step-id",
            "3",
        ])
        .unwrap();
        let Command::Query(point) = point.command else {
            panic!("expected query command");
        };
        assert!(matches!(
            point.command,
            Some(persisting_ppilot::QueryCommand::Point(_))
        ));

        let batch = Cli::try_parse_from([
            "ppilot",
            "query",
            "batch",
            "storyline-store",
            "--session-id",
            "run-a,run-b",
        ])
        .unwrap();
        let Command::Query(batch) = batch.command else {
            panic!("expected query command");
        };
        assert!(matches!(
            batch.command,
            Some(persisting_ppilot::QueryCommand::Batch(_))
        ));

        let follow = Cli::try_parse_from([
            "ppilot",
            "query",
            "follow",
            "capture",
            "--agent-id",
            "agent-a",
            "--session-id",
            "run-a",
        ])
        .unwrap();
        let Command::Query(follow) = follow.command else {
            panic!("expected query command");
        };
        assert!(matches!(
            follow.command,
            Some(persisting_ppilot::QueryCommand::Follow(_))
        ));

        let chronicle = Cli::try_parse_from([
            "ppilot",
            "chronicle",
            "import",
            "input.ndjson",
            "storyline-store",
        ]);
        assert!(matches!(chronicle.unwrap().command, Command::Chronicle(_)));

        let convert = Cli::try_parse_from([
            "ppilot",
            "convert",
            "input",
            "output",
            "--from",
            "openai_msg",
            "--to",
            "lance",
        ]);
        assert!(matches!(convert.unwrap().command, Command::Convert(_)));

        let run = Cli::try_parse_from(["ppilot", "run", "plan.py", "--workers", "2"]);
        assert!(matches!(run.unwrap().command, Command::Run(_)));

        let self_test = Cli::try_parse_from(["ppilot", "self-test", "--workers", "1"]);
        assert!(matches!(self_test.unwrap().command, Command::SelfTest(_)));

        let produce = Cli::try_parse_from([
            "ppilot",
            "produce",
            "production.py",
            "--output",
            "runs",
            "-j",
            "8",
            "--cluster-network-limit",
            "10mbps",
            "--",
            "--dataset",
            "train",
        ]);
        let Command::Produce(produce) = produce.unwrap().command else {
            panic!("expected produce command")
        };
        assert_eq!(produce.cluster_network_limit, Some(1_250_000));
        assert_eq!(produce.planner_args, ["--dataset", "train"]);

        let analysis = Cli::try_parse_from([
            "ppilot",
            "analysis",
            "atif",
            "--output",
            "analysis",
            "--sql",
            "SELECT * FROM runs",
        ]);
        assert!(matches!(analysis.unwrap().command, Command::Analysis(_)));

        let count = Cli::try_parse_from([
            "ppilot", "process", "atif", "--output", "analysis", "--count", "steps",
        ]);
        assert!(matches!(count.unwrap().command, Command::Process(_)));
        for metric in ["llm-calls", "copied-context-steps"] {
            let count = Cli::try_parse_from([
                "ppilot", "process", "atif", "--output", "analysis", "--count", metric,
            ]);
            assert!(
                matches!(count.unwrap().command, Command::Process(_)),
                "metric {metric} should parse"
            );
        }
        let script = Cli::try_parse_from([
            "ppilot",
            "process",
            "atif",
            "--script",
            "job.py",
            "--mappers",
            "8",
        ]);
        assert!(matches!(script.unwrap().command, Command::Process(_)));

        assert!(Cli::try_parse_from([
            "ppilot",
            "trajectory",
            "produce",
            "manifest.json",
            "--output",
            "runs",
        ])
        .is_err());
    }

    #[test]
    fn query_requires_exactly_one_sql_input() {
        assert!(Cli::try_parse_from(["ppilot", "query", "input.jsonl"]).is_err());
        assert!(Cli::try_parse_from([
            "ppilot",
            "query",
            "input.jsonl",
            "--sql",
            "SELECT 1",
            "--sql-file",
            "query.sql",
        ])
        .is_err());
    }

    #[test]
    fn analysis_requires_exactly_one_sql_input_and_process_rejects_sql() {
        assert!(
            Cli::try_parse_from(["ppilot", "analysis", "atif", "--output", "analysis",]).is_err()
        );
        assert!(Cli::try_parse_from([
            "ppilot",
            "analysis",
            "atif",
            "--output",
            "analysis",
            "--sql",
            "SELECT 1",
            "--sql-file",
            "query.sql",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "ppilot", "process", "atif", "--output", "analysis", "--sql", "SELECT 1",
        ])
        .is_err());
        assert!(
            Cli::try_parse_from(["ppilot", "process", "atif", "--output", "analysis",]).is_err()
        );
        assert!(Cli::try_parse_from([
            "ppilot", "process", "atif", "--script", "job.py", "--count", "steps",
        ])
        .is_err());
    }
}
