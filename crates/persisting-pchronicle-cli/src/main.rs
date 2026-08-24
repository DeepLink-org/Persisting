use std::io::{self, IsTerminal};
use std::process::ExitCode;

use clap::Parser;
use persisting_pchronicle_cli::{error_code, error_exit_code, run_with_stdio, Cli};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let debug_errors = cli.debug_errors();
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
            let code = error_code(&error);
            let rendered = render_error(&error, debug_errors);
            let duplicated_prefix = format!("{code}: ");
            let rendered = rendered
                .strip_prefix(&duplicated_prefix)
                .unwrap_or(&rendered);
            let _ = writeln!(stderr, "error[{}]: {}", code, rendered);
            ExitCode::from(error_exit_code(&error))
        }
    }
}

fn render_error(error: &anyhow::Error, detailed: bool) -> String {
    if detailed {
        format!("{error:#}")
    } else {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_error_rendering_omits_nested_sources_until_explicitly_requested() {
        let error = anyhow::Error::new(std::io::Error::other("nested-error-sentinel"))
            .context("top-level summary");

        assert_eq!(render_error(&error, false), "top-level summary");
        assert_eq!(
            render_error(&error, true),
            "top-level summary: nested-error-sentinel"
        );
    }
}
