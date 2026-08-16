use std::io::{self, IsTerminal};
use std::process::ExitCode;

use clap::Parser;
use persisting_pchronicle_cli::{run_with_stdio, Cli};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let stdin_is_terminal = io::stdin().is_terminal();
    let stdout_is_terminal = io::stdout().is_terminal();
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();

    match run_with_stdio(
        cli,
        stdin_is_terminal,
        stdout_is_terminal,
        &mut stdin,
        &mut stdout,
        &mut stderr,
    )
    .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            use std::io::Write as _;
            let _ = writeln!(stderr, "error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
