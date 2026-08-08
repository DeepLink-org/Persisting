//! pPilot commands for operating pChronicle stores.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use persisting_pchronicle::{
    actf_to_storylines, convert::atif_to_storyline, detect_format, recover_openai_msg_files,
    ActfDocument, AtifReader, ChronicleFormat, LanceMaintenanceOptions, OpenaiMsgCorpusReader,
    StorylineDocument, StorylineLanceStore,
};

#[derive(Debug, Args)]
pub struct ChronicleArgs {
    #[command(subcommand)]
    pub command: ChronicleCommand,
}

#[derive(Debug, Subcommand)]
pub enum ChronicleCommand {
    /// Import ATIF, ACTF, or OpenAI-message JSON into Storyline Lance.
    Import(ChronicleImportArgs),
    /// Recover losslessly imported OpenAI-message JSON files.
    Export(ChronicleExportArgs),
    /// Compact fragments, refresh scalar indices, and vacuum old versions.
    Maintain(ChronicleMaintainArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ChronicleImportFormat {
    Auto,
    Atif,
    Actf,
    #[value(name = "openai_msg")]
    OpenaiMsg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ChronicleExportFormat {
    #[value(name = "openai_msg")]
    OpenaiMsg,
}

#[derive(Debug, Args)]
pub struct ChronicleImportArgs {
    /// ATIF JSON/JSONL, ACTF JSON, or OpenAI-message JSON file or directory.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Storyline Lance store path or object-store URI.
    #[arg(value_name = "STORE")]
    pub store: String,

    /// Input format; auto inspects every input file and rejects mixed directories.
    #[arg(long, value_enum, default_value_t = ChronicleImportFormat::Auto)]
    pub format: ChronicleImportFormat,
}

#[derive(Debug, Args)]
pub struct ChronicleExportArgs {
    /// Storyline Lance store path or object-store URI.
    #[arg(value_name = "STORE")]
    pub store: String,

    /// Directory in which original OpenAI JSON file groupings are restored.
    #[arg(value_name = "OUTPUT_DIR")]
    pub output: PathBuf,

    /// Output trajectory format.
    #[arg(long, value_enum)]
    pub format: ChronicleExportFormat,

    /// Overwrite recovered files that already exist.
    #[arg(long)]
    pub force: bool,
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
        ChronicleCommand::Import(args) => import_trajectories(args).await,
        ChronicleCommand::Export(args) => export_openai(args).await,
        ChronicleCommand::Maintain(args) => maintain_storyline(args).await,
    }
}

async fn import_trajectories(args: ChronicleImportArgs) -> Result<()> {
    let format = match args.format {
        ChronicleImportFormat::Auto => detect_import_format(&args.input)?,
        format => format,
    };
    let store = StorylineLanceStore::open_uri(&args.store)
        .await
        .with_context(|| format!("open Storyline Lance store {}", args.store))?;
    let report = match format {
        ChronicleImportFormat::Atif if store.current_table_paths().await?.is_none() => {
            store.import_atif_stream(&args.input).await?
        }
        ChronicleImportFormat::Atif => {
            let trajectories = AtifReader::open(&args.input)
                .with_context(|| format!("open ATIF input {}", args.input.display()))?;
            let stories = trajectories.map(|trajectory| {
                let trajectory = trajectory?;
                atif_to_storyline(&trajectory).map_err(anyhow::Error::from)
            });
            store.replace_storyline_stream(stories).await?
        }
        ChronicleImportFormat::OpenaiMsg => {
            let stories = OpenaiMsgCorpusReader::open(&args.input)
                .with_context(|| format!("open OpenAI input {}", args.input.display()))?
                .map(|story| story.map_err(anyhow::Error::from));
            store.replace_storyline_stream(stories).await?
        }
        ChronicleImportFormat::Actf => {
            let stories = load_actf_storylines(&args.input)?
                .into_iter()
                .map(Ok::<StorylineDocument, anyhow::Error>);
            store.replace_storyline_stream(stories).await?
        }
        ChronicleImportFormat::Auto => unreachable!("auto import format was resolved above"),
    };
    println!(
        "imported_trajectories={} imported_steps={} format={} store={}",
        report.storylines,
        report.steps,
        match format {
            ChronicleImportFormat::Atif => "atif",
            ChronicleImportFormat::Actf => "actf",
            ChronicleImportFormat::OpenaiMsg => "openai_msg",
            ChronicleImportFormat::Auto => unreachable!(),
        },
        args.store,
    );
    Ok(())
}

async fn export_openai(args: ChronicleExportArgs) -> Result<()> {
    let store = StorylineLanceStore::open_uri(&args.store)
        .await
        .with_context(|| format!("open Storyline Lance store {}", args.store))?;
    let runs = store.list_runs().await?;
    anyhow::ensure!(!runs.is_empty(), "Storyline Lance store is empty");
    let session_ids = runs
        .into_iter()
        .map(|run| run.session_id)
        .collect::<Vec<_>>();
    let stories = store
        .get_storylines(&session_ids)
        .await?
        .into_iter()
        .zip(&session_ids)
        .map(|(story, session_id)| {
            story.with_context(|| format!("missing Storyline for session {session_id}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let recovered = recover_openai_msg_files(&stories).map_err(anyhow::Error::from)?;

    if args.output.exists() {
        anyhow::ensure!(
            args.output.is_dir(),
            "OpenAI export output is not a directory: {}",
            args.output.display()
        );
    } else {
        fs::create_dir_all(&args.output)
            .with_context(|| format!("create export directory {}", args.output.display()))?;
    }
    for file in &recovered {
        let destination = args.output.join(&file.relative_path);
        if destination.exists() && !args.force {
            anyhow::bail!(
                "refusing to overwrite {}; pass --force to replace recovered files",
                destination.display()
            );
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create export directory {}", parent.display()))?;
        }
        let json =
            serde_json::to_string_pretty(&file.document).context("encode recovered OpenAI JSON")?;
        fs::write(&destination, format!("{json}\n"))
            .with_context(|| format!("write recovered OpenAI file {}", destination.display()))?;
    }
    println!(
        "exported_files={} exported_trajectories={} output={}",
        recovered.len(),
        stories.len(),
        args.output.display()
    );
    Ok(())
}

fn detect_import_format(input: &Path) -> Result<ChronicleImportFormat> {
    let files = if input.is_file() {
        vec![input.to_path_buf()]
    } else if input.is_dir() {
        let mut files = fs::read_dir(input)
            .with_context(|| format!("read import directory {}", input.display()))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        files.retain(|path| {
            matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("json" | "jsonl" | "ndjson")
            )
        });
        files.sort();
        files
    } else {
        anyhow::bail!("import input does not exist: {}", input.display());
    };
    anyhow::ensure!(
        !files.is_empty(),
        "import input contains no JSON files: {}",
        input.display()
    );

    let mut detected = None;
    for file in files {
        let text = fs::read_to_string(&file)
            .with_context(|| format!("read import input {}", file.display()))?;
        let content = if matches!(
            file.extension().and_then(|value| value.to_str()),
            Some("jsonl" | "ndjson")
        ) {
            text.lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("")
        } else {
            text.as_str()
        };
        let format = detect_format(Some(&file), Some(content))
            .map_err(anyhow::Error::from)?
            .with_context(|| format!("cannot detect trajectory format for {}", file.display()))?;
        let format = match format {
            ChronicleFormat::Atif => ChronicleImportFormat::Atif,
            ChronicleFormat::Actf => ChronicleImportFormat::Actf,
            ChronicleFormat::OpenaiMsg => ChronicleImportFormat::OpenaiMsg,
            other => anyhow::bail!(
                "unsupported chronicle import format '{}' in {}",
                other,
                file.display()
            ),
        };
        if let Some(previous) = detected {
            anyhow::ensure!(
                previous == format,
                "mixed trajectory formats in {} (found {:?} and {:?})",
                input.display(),
                previous,
                format
            );
        } else {
            detected = Some(format);
        }
    }
    Ok(detected.expect("non-empty input file list"))
}

fn load_actf_storylines(input: &Path) -> Result<Vec<StorylineDocument>> {
    let files = if input.is_file() {
        vec![input.to_path_buf()]
    } else {
        let mut files = fs::read_dir(input)
            .with_context(|| format!("read ACTF input directory {}", input.display()))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        files.retain(|path| path.extension().and_then(|value| value.to_str()) == Some("json"));
        files.sort();
        files
    };
    anyhow::ensure!(
        !files.is_empty(),
        "ACTF input contains no JSON files: {}",
        input.display()
    );
    let mut stories = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file)
            .with_context(|| format!("read ACTF input {}", file.display()))?;
        let document = ActfDocument::from_json_str(&text)
            .with_context(|| format!("parse ACTF input {}", file.display()))?;
        stories.extend(actf_to_storylines(&document).map_err(anyhow::Error::from)?);
    }
    Ok(stories)
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
        assert_eq!(args.format, ChronicleImportFormat::Auto);
    }

    #[test]
    fn parses_openai_export_arguments() {
        let parsed = TestCli::try_parse_from([
            "test",
            "export",
            "store",
            "recovered",
            "--format",
            "openai_msg",
            "--force",
        ])
        .expect("chronicle export should parse");
        let ChronicleCommand::Export(args) = parsed.args.command else {
            panic!("expected export command");
        };
        assert_eq!(args.store, "store");
        assert_eq!(args.output, PathBuf::from("recovered"));
        assert_eq!(args.format, ChronicleExportFormat::OpenaiMsg);
        assert!(args.force);
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
                format: ChronicleImportFormat::Auto,
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

    #[tokio::test]
    async fn openai_json_roundtrips_through_storyline_lance() {
        let temporary = tempfile::tempdir().unwrap();
        let input_dir = temporary.path().join("input");
        let store_path = temporary.path().join("storyline-store");
        let output_dir = temporary.path().join("output");
        fs::create_dir(&input_dir).unwrap();
        let input_path = input_dir.join("openai.json");
        let input = serde_json::json!([
            {
                "id":"evt-2","session_id":"s-1","step_id":2,
                "agent_model":"gpt-test","created_at":1700000001,
                "messages":[
                    {"role":"user","content":[{"type":"text","text":"next"}]},
                    {"role":"assistant","content":[{"type":"text","text":"world"}]}
                ],
                "response":{"role":"assistant","content":[]},
                "unknown":null
            },
            {
                "id":"evt-1","session_id":"s-1","step_id":1,
                "agent_model":"gpt-test","created_at":1700000000,
                "messages":[
                    {"role":"user","content":"hello"},
                    {"role":"assistant","content":"answer"}
                ],
                "response":{"role":"assistant","content":""}
            },
            {
                "id":"evt-3","session_id":"s-2","step_id":1,
                "agent_model":"gpt-test",
                "messages":[
                    {"role":"user","content":"other"},
                    {"role":"assistant","content":"done"}
                ],
                "response":{"role":"assistant","content":null}
            }
        ]);
        fs::write(&input_path, serde_json::to_vec_pretty(&input).unwrap()).unwrap();

        run_chronicle(ChronicleArgs {
            command: ChronicleCommand::Import(ChronicleImportArgs {
                input: input_dir,
                store: store_path.to_string_lossy().into_owned(),
                format: ChronicleImportFormat::Auto,
            }),
        })
        .await
        .unwrap();

        let store = StorylineLanceStore::open(&store_path).await.unwrap();
        assert_eq!(store.list_runs().await.unwrap().len(), 2);
        assert_eq!(store.list_steps("s-1").await.unwrap().len(), 2);

        run_chronicle(ChronicleArgs {
            command: ChronicleCommand::Export(ChronicleExportArgs {
                store: store_path.to_string_lossy().into_owned(),
                output: output_dir.clone(),
                format: ChronicleExportFormat::OpenaiMsg,
                force: false,
            }),
        })
        .await
        .unwrap();
        let recovered: serde_json::Value =
            serde_json::from_slice(&fs::read(output_dir.join("openai.json")).unwrap()).unwrap();
        assert_eq!(recovered, input);
    }
}
