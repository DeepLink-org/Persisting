//! pPilot commands for operating pChronicle stores.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use persisting_pchronicle::{
    convert::atif_to_storyline, AtifReader, LanceMaintenanceOptions, StorylineLanceStore,
};

#[derive(Debug, Args)]
pub struct ChronicleArgs {
    #[command(subcommand)]
    pub command: ChronicleCommand,
}

#[derive(Debug, Subcommand)]
pub enum ChronicleCommand {
    /// Import ATIF JSON, JSONL, or a directory into Storyline Lance.
    Import(ChronicleImportArgs),
    /// Compact fragments, refresh scalar indices, and vacuum old versions.
    Maintain(ChronicleMaintainArgs),
}

#[derive(Debug, Args)]
pub struct ChronicleImportArgs {
    /// ATIF JSON/JSONL file or directory.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Storyline Lance store path or object-store URI.
    #[arg(value_name = "STORE")]
    pub store: String,
}

#[derive(Debug, Args)]
pub struct ChronicleMaintainArgs {
    /// Storyline Lance store path or object-store URI.
    #[arg(value_name = "STORE")]
    pub store: String,

    /// Keep Lance versions newer than this many hours; zero disables vacuum.
    #[arg(long, default_value_t = 168)]
    pub vacuum_retention_hours: u64,

    /// Skip fragment compaction.
    #[arg(long)]
    pub no_compact: bool,

    /// Skip creation and refresh of scalar indices.
    #[arg(long)]
    pub no_optimize_indices: bool,

    /// Target rows per compacted fragment.
    #[arg(long, default_value_t = 1024 * 1024)]
    pub target_rows_per_fragment: usize,
}

pub async fn run_chronicle(args: ChronicleArgs) -> Result<()> {
    match args.command {
        ChronicleCommand::Import(args) => import_atif(args).await,
        ChronicleCommand::Maintain(args) => maintain_storyline(args).await,
    }
}

async fn import_atif(args: ChronicleImportArgs) -> Result<()> {
    let store = StorylineLanceStore::open_uri(&args.store)
        .await
        .with_context(|| format!("open Storyline Lance store {}", args.store))?;
    let report = if store.current_table_paths().await?.is_none() {
        store.import_atif_stream(&args.input).await?
    } else {
        let trajectories = AtifReader::open(&args.input)
            .with_context(|| format!("open ATIF input {}", args.input.display()))?;
        let stories = trajectories.map(|trajectory| {
            let trajectory = trajectory?;
            atif_to_storyline(&trajectory).map_err(anyhow::Error::from)
        });
        store.replace_storyline_stream(stories).await?
    };
    println!(
        "imported_trajectories={} imported_steps={} store={}",
        report.storylines, report.steps, args.store
    );
    Ok(())
}

async fn maintain_storyline(args: ChronicleMaintainArgs) -> Result<()> {
    let store = StorylineLanceStore::open_uri(&args.store)
        .await
        .with_context(|| format!("open Storyline Lance store {}", args.store))?;
    let options = LanceMaintenanceOptions {
        compact: !args.no_compact,
        optimize_indices: !args.no_optimize_indices,
        vacuum_older_than: (args.vacuum_retention_hours > 0)
            .then(|| Duration::from_secs(args.vacuum_retention_hours.saturating_mul(3600))),
        target_rows_per_fragment: args.target_rows_per_fragment,
    };
    let report = store.maintain(&options).await?;
    println!(
        "maintained_generation={} fragments_removed={} fragments_added={} old_versions_removed={} bytes_removed={}",
        report.generation.as_deref().unwrap_or("none"),
        report.runs.fragments_removed
            + report.steps.fragments_removed
            + report.tool_calls.fragments_removed,
        report.runs.fragments_added
            + report.steps.fragments_added
            + report.tool_calls.fragments_added,
        report.runs.old_versions_removed
            + report.steps.old_versions_removed
            + report.tool_calls.old_versions_removed,
        report.runs.bytes_removed + report.steps.bytes_removed + report.tool_calls.bytes_removed,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(flatten)]
        args: ChronicleArgs,
    }

    #[test]
    fn parses_import_arguments() {
        let parsed = TestCli::try_parse_from(["test", "import", "input.ndjson", "store"])
            .expect("chronicle import should parse");
        let ChronicleCommand::Import(args) = parsed.args.command else {
            panic!("expected import command");
        };
        assert_eq!(args.input, PathBuf::from("input.ndjson"));
        assert_eq!(args.store, "store");
    }

    #[test]
    fn parses_maintenance_policy() {
        let parsed = TestCli::try_parse_from([
            "test",
            "maintain",
            "store",
            "--vacuum-retention-hours",
            "24",
            "--no-compact",
        ])
        .expect("chronicle maintain should parse");
        let ChronicleCommand::Maintain(args) = parsed.args.command else {
            panic!("expected maintain command");
        };
        assert_eq!(args.store, "store");
        assert_eq!(args.vacuum_retention_hours, 24);
        assert!(args.no_compact);
        assert!(!args.no_optimize_indices);
    }

    #[tokio::test]
    async fn imports_atif_into_storyline_lance() {
        let input = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../persisting-pchronicle/tests/fixtures/atif/dialogue_10.json");
        let temporary = tempfile::tempdir().unwrap();
        let store_path = temporary.path().join("storyline-store");
        run_chronicle(ChronicleArgs {
            command: ChronicleCommand::Import(ChronicleImportArgs {
                input,
                store: store_path.to_string_lossy().into_owned(),
            }),
        })
        .await
        .unwrap();

        let store = StorylineLanceStore::open(&store_path).await.unwrap();
        assert_eq!(store.list_runs().await.unwrap().len(), 1);
        assert_eq!(
            store.list_steps("fixture-dialogue_10").await.unwrap().len(),
            10
        );
    }
}
