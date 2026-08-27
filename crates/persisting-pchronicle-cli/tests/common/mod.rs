use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use persisting_pchronicle_cli::{Cli, run};
use serde_json::Value;

#[derive(Debug, Clone, Copy)]
pub struct ExampleFixture {
    pub name: &'static str,
    pub source_file: &'static str,
    pub imported_source: &'static str,
    pub detected_format: &'static str,
    pub identity_flag: &'static str,
    pub identity: &'static str,
    pub session_id: &'static str,
    pub runs: u64,
    pub trajectories: u64,
    pub steps: u64,
    pub tool_calls: u64,
}

pub const EXAMPLE_FIXTURES: [ExampleFixture; 3] = [
    ExampleFixture {
        name: "atif",
        source_file: "atif/support-ticket.json",
        imported_source: "trajectories.atif.json",
        detected_format: "atif",
        identity_flag: "--session-id",
        identity: "support-001",
        session_id: "support-001",
        runs: 1,
        trajectories: 1,
        steps: 3,
        tool_calls: 1,
    },
    ExampleFixture {
        name: "openai-messages",
        source_file: "openai-messages/training.json",
        imported_source: "session_steps.json",
        detected_format: "openai-msg",
        identity_flag: "--session-id",
        identity: "training-002",
        session_id: "training-002",
        runs: 2,
        trajectories: 2,
        steps: 4,
        tool_calls: 0,
    },
    ExampleFixture {
        name: "actf",
        source_file: "actf/code-repair.actf.json",
        imported_source: "trajectories.actf.json",
        detected_format: "actf",
        identity_flag: "--run-id",
        identity: "example-code-repair",
        session_id: "example-code-repair",
        runs: 1,
        trajectories: 1,
        steps: 2,
        tool_calls: 1,
    },
];

impl ExampleFixture {
    pub fn dataset(self) -> PathBuf {
        examples_root().join(self.name)
    }

    pub fn source(self) -> PathBuf {
        examples_root().join(self.source_file)
    }

    pub fn dataset_source_name(self) -> &'static str {
        self.source_file
            .split_once('/')
            .expect("fixture source must contain its Dataset directory")
            .1
    }
}

pub fn examples_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/data")
}

#[derive(Debug)]
pub struct RunOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl RunOutput {
    pub fn json(&self) -> Result<Value> {
        Ok(serde_json::from_slice(&self.stdout)?)
    }

    pub fn stderr_text(&self) -> Result<&str> {
        Ok(std::str::from_utf8(&self.stderr)?)
    }
}

pub async fn run_cli(args: impl IntoIterator<Item = impl Into<String>>) -> Result<RunOutput> {
    let mut argv = vec!["pchronicle".to_owned()];
    argv.extend(args.into_iter().map(Into::into));
    let cli = Cli::try_parse_from(argv)?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run(cli, false, &mut stdout, &mut stderr).await?;
    Ok(RunOutput { stdout, stderr })
}
