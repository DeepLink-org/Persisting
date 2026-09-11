#![recursion_limit = "256"]

use std::io::{self, IsTerminal};
use std::process::ExitCode;

use clap::Parser;
use persisting_pchronicle_cli::{
    Cli, apply_catalog_backend_env_before_runtime, error_code, error_exit_code, run_with_stdio,
};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let debug_errors = cli.debug_errors();
    // OpenDAL/Lance read AWS_* from the process environment. Applying catalog
    // backend keys after the multi-threaded Tokio runtime starts is racy on
    // macOS; do it before any worker threads exist.
    if let Err(error) = apply_catalog_backend_env_before_runtime(&cli) {
        use std::io::Write as _;
        let code = error_code(&error);
        let rendered = render_error(&error, debug_errors);
        let _ = writeln!(io::stderr(), "error[{code}]: {rendered}");
        return ExitCode::from(error_exit_code(&error));
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            use std::io::Write as _;
            let _ = writeln!(
                io::stderr(),
                "error[internal]: start tokio runtime: {error}"
            );
            return ExitCode::from(1);
        }
    };
    runtime.block_on(async_main(cli, debug_errors))
}

async fn async_main(cli: Cli, debug_errors: bool) -> ExitCode {
    let stdin_is_terminal = io::stdin().is_terminal();
    let stdout_is_terminal = io::stdout().is_terminal();
    let stderr_is_terminal = io::stderr().is_terminal();
    // Do not hold StdoutLock/StderrLock for the process lifetime. `pchronicle
    // serve` logs from Tokio worker threads via tracing; on macOS those writes
    // take the stdout lock, so a process-wide lock deadlocks the runtime.
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();

    match run_with_stdio(
        cli,
        stdin_is_terminal,
        stdout_is_terminal,
        stderr_is_terminal,
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
