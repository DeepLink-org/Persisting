use std::io::{self, IsTerminal};
use std::process::ExitCode;

use clap::Parser;
use persisting_pchronicle_cli::{run, Cli};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let stdout_is_terminal = io::stdout().is_terminal();
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();

    match run(cli, stdout_is_terminal, &mut stdout, &mut stderr).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            use std::io::Write as _;
            let _ = writeln!(stderr, "error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
