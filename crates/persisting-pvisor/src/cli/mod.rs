//! Standalone `pvisor` command-line frontend.

mod env;
mod product;
mod replay;
mod run;
pub mod runtime;
mod trajectory;

use clap::{Parser, Subcommand};

#[cfg(target_os = "linux")]
const ROOT_ABOUT: &str =
    "Foreground Agent Run manager with rootless Linux sandboxing and reviewable workspaces";
#[cfg(target_os = "linux")]
const ROOT_LONG_ABOUT: &str = "Foreground Agent Run manager: execute, control, Gateway, and OverlayFS.\n\nOn Linux, host runs use safe-best-effort rootless isolation when supported: user and mount namespaces, a minimal synthetic root with chroot, Landlock, no_new_privs, and dropped capabilities. Add `--overlaynet-deny-all` to isolate direct network sockets in a private network namespace.";

#[cfg(target_os = "macos")]
const ROOT_ABOUT: &str =
    "Foreground Agent Run manager with Seatbelt isolation and reviewable workspaces";
#[cfg(target_os = "macos")]
const ROOT_LONG_ABOUT: &str = "Foreground Agent Run manager: execute, control, Gateway, and OverlayFS.\n\nOn macOS, host runs use safe-best-effort Seatbelt confinement when supported. Full-disk reads remain available for local toolchain compatibility. `--overlaynet-deny-all` also blocks IP and ambient host Unix sockets while retaining Run-local IPC.";

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const ROOT_ABOUT: &str = "Foreground Agent Run manager with staged, reviewable workspaces";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const ROOT_LONG_ABOUT: &str =
    "Foreground Agent Run manager: execute, control, Gateway, and OverlayFS.";

#[derive(Debug, Parser)]
#[command(
    name = "pvisor",
    version,
    about = ROOT_ABOUT,
    long_about = ROOT_LONG_ABOUT
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(
        about = run::RUN_COMMAND_ABOUT,
        long_about = run::RUN_COMMAND_LONG_ABOUT
    )]
    Run(Box<run::RunArgs>),
    /// Replay an agent-native trajectory in this sandbox, then continue the same Agent.
    Replay(Box<replay::ReplayArgs>),
    /// Manage durable reusable execution environments.
    Env(env::EnvArgs),
    /// Show the selected Run's process, filesystem, and network status.
    Status(runtime::StatusArgs),
    /// Open a read-only shell or run a command against a Run filesystem view.
    Inspect(runtime::InspectArgs),
    /// Review the durable Run Bundle before accepting filesystem changes.
    Review(product::ReviewArgs),
    /// Create a stopped-consistent logical filesystem checkpoint.
    Checkpoint(product::CheckpointArgs),
    /// Start a new safe Run from a logical checkpoint.
    Fork(run::ForkArgs),
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
        Command::Replay(args) => {
            let code = replay::run(*args);
            if code != 0 {
                std::process::exit(code);
            }
        }
        Command::Env(args) => {
            let code = env::run(args)?;
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
        Command::Review(args) => product::review(args)?,
        Command::Checkpoint(args) => product::checkpoint(args)?,
        Command::Fork(args) => {
            let code = tokio::runtime::Runtime::new()?.block_on(run::fork(args))?;
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
    let reserved = [
        "run",
        "replay",
        "env",
        "status",
        "inspect",
        "review",
        "checkpoint",
        "fork",
        "apply",
        "drop",
        "help",
    ];
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
            vec!["pvisor", "review", "run-1"],
            vec!["pvisor", "checkpoint", "run-1", "--name", "before"],
            vec!["pvisor", "fork", "run-1", "--", "codex"],
            vec!["pvisor", "apply", "run-1"],
            vec!["pvisor", "apply", "run-1", "--target", "/tmp/restored"],
            vec!["pvisor", "apply", "run-1", "--path", "src"],
            vec![
                "pvisor",
                "apply",
                "run-1",
                "--include",
                "src/**",
                "--exclude",
                "src/generated/**",
            ],
            vec!["pvisor", "drop", "run-1"],
            vec!["pvisor", "env", "create", "demo", "--target", "/tmp"],
            vec!["pvisor", "env", "exec", "demo", "--", "/bin/true"],
            vec!["pvisor", "env", "shell", "demo"],
            vec!["pvisor", "env", "list"],
            vec!["pvisor", "env", "status", "demo"],
            vec!["pvisor", "env", "delete", "demo", "--force"],
            vec!["pvisor", "run", "--", "/usr/bin/true"],
            vec!["pvisor", "run", "/usr/bin/true"],
            vec![
                "pvisor",
                "replay",
                "--agent",
                "claude-code",
                "--trajectory",
                "/input/session.jsonl",
                "--after-step",
                "30",
            ],
            vec![
                "pvisor",
                "replay",
                "--agent",
                "claude-code",
                "--trajectory",
                "/input/session.jsonl",
                "--after-step",
                "30",
                "--boundary-user-prompt",
                "Review the fresh observation.",
                "--agent-entrypoint",
                "/usr/bin/claude",
                "--overlayfs-path",
                "/workspace",
            ],
        ] {
            Cli::try_parse_from(args).expect("valid pvisor command");
        }
    }

    #[test]
    fn replay_rejects_removed_chronicle_flag() {
        let error = Cli::try_parse_from([
            "pvisor",
            "replay",
            "--agent",
            "claude-code",
            "--trajectory",
            "/input/session.jsonl",
            "--after-step",
            "30",
            "--chronicle-mode",
            "off",
        ])
        .unwrap_err();
        assert!(error.to_string().contains("--chronicle-mode"));
    }

    #[test]
    fn replay_modes_are_mutually_exclusive_cli_flags() {
        for mode in ["--prepare-only", "--replay-only"] {
            Cli::try_parse_from([
                "pvisor",
                "replay",
                "--agent",
                "claude-code",
                "--trajectory",
                "/input/session.jsonl",
                "--after-step",
                "1",
                mode,
            ])
            .expect("individual replay mode flag must be accepted");
        }

        let error = Cli::try_parse_from([
            "pvisor",
            "replay",
            "--agent",
            "claude-code",
            "--trajectory",
            "/input/session.jsonl",
            "--after-step",
            "1",
            "--prepare-only",
            "--replay-only",
        ])
        .unwrap_err();
        assert!(error.to_string().contains("cannot be used with"));
    }

    #[test]
    fn replay_help_describes_phase_modes() {
        let help = Cli::try_parse_from(["pvisor", "replay", "--help"])
            .unwrap_err()
            .to_string();

        assert!(help.contains("--prepare-only"));
        assert!(help.contains("without executing tools or starting an Agent"));
        assert!(help.contains("--replay-only"));
        assert!(help.contains("stop before the next model request"));
        assert!(help.contains("--allow-stale-observations"));
        assert!(help.contains("--boundary-user-prompt"));
        assert!(help.contains("after the replayed boundary observation"));
        assert!(help.contains("including the replayed prefix and any live continuation"));
    }

    #[test]
    fn unknown_first_token_becomes_default_run() {
        let args = normalize_default_run(vec!["pvisor".into(), "/bin/true".into()]);
        assert_eq!(args[1], "run");
    }

    #[test]
    fn root_help_names_the_effective_platform_boundary() {
        let help = Cli::try_parse_from(["pvisor", "--help"])
            .unwrap_err()
            .to_string();

        #[cfg(target_os = "linux")]
        {
            assert!(help.contains("safe-best-effort"));
            assert!(help.contains("rootless isolation"));
            assert!(help.contains("namespace"));
            assert!(help.contains("Landlock"));
        }
        #[cfg(target_os = "macos")]
        {
            assert!(help.contains("macFUSE"));
            assert!(help.contains("Seatbelt"));
            assert!(help.contains("Full-disk reads remain available"));
        }
    }
}
