use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use persisting_ppilot::{
    init_tracing_with_verbose, produce_from_planner, produce_trajectories, run_ppilot,
    BatchProductionManifest, BatchProductionOptions, PPilotArgs,
};

#[derive(Debug, Parser)]
#[command(
    name = "ppilot",
    version,
    about = "Produce durable Agent Runs at scale"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a pPilot plan with bounded concurrency and durable resume.
    Run(Box<PPilotArgs>),
    /// Stream Runs from a Python planner into independent pVisor workspaces.
    Produce(ProduceArgs),
}

#[derive(Debug, Args)]
struct ProduceArgs {
    /// Python planner defining plan(); JSON manifests remain accepted for compatibility.
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
    /// Standalone pVisor executable used for every produced Run.
    #[arg(long, env = "PERSISTING_PVISOR_BIN", default_value = "pvisor")]
    pvisor_binary: PathBuf,
    /// Stable batch identifier; defaults to the planner filename stem.
    #[arg(long, value_name = "ID")]
    batch_id: Option<String>,
    /// Arguments forwarded to the planner after `--`.
    #[arg(last = true, value_name = "ARG")]
    planner_args: Vec<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let verbose = matches!(&cli.command, Command::Run(args) if args.verbose);
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
        Command::Produce(args) => {
            let options = BatchProductionOptions {
                output_dir: args.output,
                pvisor_binary: args.pvisor_binary,
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
                    anyhow::bail!("JSON manifests do not accept planner arguments");
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_only_production_subcommands() {
        assert!(matches!(
            Cli::try_parse_from(["ppilot", "run", "plan.py", "--workers", "2"])
                .unwrap()
                .command,
            Command::Run(_)
        ));

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
        ])
        .unwrap();
        let Command::Produce(produce) = produce.command else {
            panic!("expected produce command")
        };
        assert_eq!(produce.cluster_network_limit, Some(1_250_000));
        assert_eq!(produce.planner_args, ["--dataset", "train"]);

        for removed in [
            "query",
            "chronicle",
            "convert",
            "analysis",
            "process",
            "self-test",
        ] {
            assert!(
                Cli::try_parse_from(["ppilot", removed]).is_err(),
                "removed subcommand {removed} must not parse"
            );
        }
    }
}
