use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use persisting_ppilot::{
    init_tracing_with_verbose, run_ppilot, run_query, run_self_test, PPilotArgs, QueryArgs,
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
    Run(PPilotArgs),
    /// Run read-only SQL against Storyline Lance or ATIF JSON/JSONL.
    Query(QueryArgs),
    /// Run the built-in plan/execute smoke test.
    SelfTest(SelfTestArgs),
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
        Command::Run(args) => run_ppilot(args).await,
        Command::Query(args) => {
            run_query(args).await?;
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
        let query = Cli::try_parse_from([
            "ppilot",
            "query",
            "input.jsonl",
            "--source",
            "atif",
            "--sql",
            "SELECT 1",
        ]);
        assert!(matches!(query.unwrap().command, Command::Query(_)));

        let run = Cli::try_parse_from(["ppilot", "run", "plan.py", "--workers", "2"]);
        assert!(matches!(run.unwrap().command, Command::Run(_)));

        let self_test = Cli::try_parse_from(["ppilot", "self-test", "--workers", "1"]);
        assert!(matches!(self_test.unwrap().command, Command::SelfTest(_)));
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
}
