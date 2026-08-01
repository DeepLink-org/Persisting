//! Standalone `pvisor` command-line frontend.

mod run;
pub mod runtime;
mod trajectory;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "pvisor",
    version,
    about = "Foreground Agent Run manager: execute, control, Gateway, and OverlayFS"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Execute one Agent Run under pVisor management (default command).
    Run(Box<run::RunArgs>),
    /// Show the selected Run's process, filesystem, and network status.
    Status(runtime::StatusArgs),
    /// Open a read-only shell or run a command against a Run workspace.
    Inspect(runtime::InspectArgs),
    /// Apply a stopped Run's staged filesystem changes to its target.
    Apply(runtime::ApplyArgs),
    /// Drop a stopped Run's staged filesystem changes.
    Drop(runtime::SelectArgs),
}

pub fn main() -> anyhow::Result<()> {
    let args = normalize_default_run(std::env::args_os().collect());
    match Cli::parse_from(args).command {
        Command::Run(args) => {
            let code = tokio::runtime::Runtime::new()?.block_on(run::run(*args))?;
            if code != 0 {
                std::process::exit(code);
            }
        }
        Command::Status(args) => runtime::status(args)?,
        Command::Inspect(args) => {
            let code = runtime::inspect(args)?;
            if code != 0 {
                std::process::exit(code);
            }
        }
        Command::Apply(args) => runtime::apply(args)?,
        Command::Drop(args) => runtime::drop_overlay(args)?,
    }
    Ok(())
}

fn normalize_default_run(mut args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    let first = args.get(1).and_then(|value| value.to_str());
    let reserved = ["run", "status", "inspect", "apply", "drop", "help"];
    if first.is_some_and(|value| {
        !reserved.contains(&value) && value != "--help" && value != "-h" && value != "--version"
    }) {
        args.insert(1, "run".into());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_cli_is_small_and_run_can_be_explicit() {
        for args in [
            vec!["pvisor", "status"],
            vec!["pvisor", "inspect", "run-1", "--", "rg", "TODO"],
            vec!["pvisor", "apply", "run-1"],
            vec!["pvisor", "apply", "run-1", "--target", "/tmp/restored"],
            vec!["pvisor", "drop", "run-1"],
            vec!["pvisor", "run", "--", "/usr/bin/true"],
        ] {
            Cli::try_parse_from(args).expect("valid pvisor command");
        }
    }

    #[test]
    fn unknown_first_token_becomes_default_run() {
        let args = normalize_default_run(vec!["pvisor".into(), "/bin/true".into()]);
        assert_eq!(args[1], "run");
    }
}
