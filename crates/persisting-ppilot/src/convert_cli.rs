//! Dedicated pPilot trajectory format conversion command.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use persisting_pchronicle::convert::{
    actf_to_storylines, atif_to_storyline, from_storyline, into_storyline, storylines_to_actf,
};
use persisting_pchronicle::{
    detect_format, is_actf_storyline, is_lossless_openai_storyline, recover_openai_msg_files,
    sanitize_session_filename, ActfDocument, AtifReader, ChronicleFormat, OpenaiMsgCorpusReader,
    StorylineDocument, StorylineLanceStore,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ConvertInputFormat {
    Auto,
    Atif,
    Actf,
    #[value(name = "openai_msg")]
    OpenaiMsg,
    Storyline,
    Agenticmd,
    Lance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ConvertOutputFormat {
    Atif,
    Actf,
    #[value(name = "openai_msg")]
    OpenaiMsg,
    Storyline,
    Agenticmd,
    Lance,
}

#[derive(Debug, Args)]
pub struct ConvertArgs {
    /// Input file, corpus directory, Storyline Lance root, or object-store URI.
    #[arg(value_name = "INPUT")]
    pub input: String,

    /// Output directory, Storyline Lance root, or object-store URI.
    #[arg(value_name = "OUTPUT")]
    pub output: String,

    /// Input format; auto detects local inputs and Storyline Lance roots.
    #[arg(long, value_enum, default_value_t = ConvertInputFormat::Auto)]
    pub from: ConvertInputFormat,

    /// Output format.
    #[arg(long, value_enum)]
    pub to: ConvertOutputFormat,

    /// Overwrite document files that already exist.
    #[arg(long)]
    pub force: bool,
}

pub async fn run_convert(args: ConvertArgs) -> Result<()> {
    let from = match args.from {
        ConvertInputFormat::Auto => detect_input_format(&args.input)?,
        format => format,
    };
    anyhow::ensure!(
        !(from == ConvertInputFormat::Lance && args.to == ConvertOutputFormat::Lance),
        "Lance-to-Lance conversion is not supported"
    );

    let stories = load_storylines(&args.input, from).await?;
    anyhow::ensure!(
        !stories.is_empty(),
        "conversion input contains no trajectories"
    );
    let story_count = stories.len();

    let output_count = match args.to {
        ConvertOutputFormat::Lance => {
            let store = StorylineLanceStore::open_uri(&args.output)
                .await
                .with_context(|| format!("open Storyline Lance output {}", args.output))?;
            let report = store
                .replace_storyline_stream(
                    stories
                        .into_iter()
                        .map(Ok::<StorylineDocument, anyhow::Error>),
                )
                .await?;
            report.storylines
        }
        format => write_documents(&args.output, format, &stories, args.force)?,
    };

    println!(
        "converted_trajectories={} output_artifacts={} from={} to={} output={}",
        story_count,
        output_count,
        input_format_name(from),
        output_format_name(args.to),
        args.output
    );
    Ok(())
}

async fn load_storylines(
    input: &str,
    format: ConvertInputFormat,
) -> Result<Vec<StorylineDocument>> {
    match format {
        ConvertInputFormat::Lance => {
            let store = StorylineLanceStore::open_uri(input)
                .await
                .with_context(|| format!("open Storyline Lance input {input}"))?;
            let session_ids = store
                .list_runs()
                .await?
                .into_iter()
                .map(|run| run.session_id)
                .collect::<Vec<_>>();
            store
                .get_storylines(&session_ids)
                .await?
                .into_iter()
                .zip(session_ids)
                .map(|(story, session_id)| {
                    story.with_context(|| format!("missing Storyline for session {session_id}"))
                })
                .collect()
        }
        ConvertInputFormat::Atif => AtifReader::open(input)
            .with_context(|| format!("open ATIF conversion input {input}"))?
            .map(|trajectory| {
                let trajectory = trajectory?;
                atif_to_storyline(&trajectory).map_err(anyhow::Error::from)
            })
            .collect(),
        ConvertInputFormat::Actf => {
            let mut stories = Vec::new();
            for path in read_document_files(Path::new(input), format)? {
                let text = fs::read_to_string(&path)
                    .with_context(|| format!("read ACTF conversion input {}", path.display()))?;
                let document = ActfDocument::from_json_str(&text)
                    .with_context(|| format!("parse ACTF document {}", path.display()))?;
                stories.extend(actf_to_storylines(&document).map_err(anyhow::Error::from)?);
            }
            Ok(stories)
        }
        ConvertInputFormat::OpenaiMsg => OpenaiMsgCorpusReader::open(input)
            .with_context(|| format!("open OpenAI conversion input {input}"))?
            .map(|story| story.map_err(anyhow::Error::from))
            .collect(),
        ConvertInputFormat::Storyline | ConvertInputFormat::Agenticmd => {
            let chronicle_format = match format {
                ConvertInputFormat::Storyline => ChronicleFormat::Storyline,
                ConvertInputFormat::Agenticmd => ChronicleFormat::Agenticmd,
                _ => unreachable!(),
            };
            read_document_files(Path::new(input), format)?
                .into_iter()
                .map(|path| {
                    let text = fs::read_to_string(&path)
                        .with_context(|| format!("read conversion input {}", path.display()))?;
                    into_storyline(chronicle_format, &text).map_err(anyhow::Error::from)
                })
                .collect()
        }
        ConvertInputFormat::Auto => unreachable!("auto input format was resolved above"),
    }
}

fn write_documents(
    output: &str,
    format: ConvertOutputFormat,
    stories: &[StorylineDocument],
    force: bool,
) -> Result<usize> {
    anyhow::ensure!(
        !is_object_store_uri(output),
        "object-store output URIs are only supported with --to lance"
    );
    let root = Path::new(output);
    prepare_output_directory(root)?;

    if format == ConvertOutputFormat::OpenaiMsg {
        let lossless_count = stories
            .iter()
            .filter(|story| is_lossless_openai_storyline(story))
            .count();
        if lossless_count == stories.len() {
            let recovered = recover_openai_msg_files(stories).map_err(anyhow::Error::from)?;
            for file in &recovered {
                let text = serde_json::to_string_pretty(&file.document)
                    .context("encode recovered OpenAI JSON")?;
                write_document_file(root, &file.relative_path, &text, force)?;
            }
            return Ok(recovered.len());
        }
        anyhow::ensure!(
            lossless_count == 0,
            "cannot mix lossless OpenAI and unrelated Storylines in one OpenAI conversion"
        );
    }

    if format == ConvertOutputFormat::Actf {
        let lossless_count = stories
            .iter()
            .filter(|story| is_actf_storyline(story))
            .count();
        anyhow::ensure!(
            lossless_count == 0 || lossless_count == stories.len(),
            "cannot mix lossless ACTF and unrelated Storylines in one ACTF conversion"
        );
        if lossless_count == stories.len() {
            let mut groups: BTreeMap<String, Vec<StorylineDocument>> = BTreeMap::new();
            for story in stories {
                groups
                    .entry(
                        story
                            .run_id
                            .clone()
                            .unwrap_or_else(|| story.session_id.clone()),
                    )
                    .or_default()
                    .push(story.clone());
            }
            for (group_id, group) in &groups {
                let document = storylines_to_actf(group).map_err(anyhow::Error::from)?;
                let relative =
                    PathBuf::from(format!("{}.actf.json", sanitize_session_filename(group_id)));
                write_document_file(root, &relative, &document.to_json_string_pretty()?, force)?;
            }
            return Ok(groups.len());
        }
    }

    let mut destinations = HashSet::new();
    for story in stories {
        let (chronicle_format, suffix) = match format {
            ConvertOutputFormat::Atif => (ChronicleFormat::Atif, ".atif.json"),
            ConvertOutputFormat::Actf => (ChronicleFormat::Actf, ".actf.json"),
            ConvertOutputFormat::OpenaiMsg => (ChronicleFormat::OpenaiMsg, ".openai.json"),
            ConvertOutputFormat::Storyline => (ChronicleFormat::Storyline, ".storyline.json"),
            ConvertOutputFormat::Agenticmd => (ChronicleFormat::Agenticmd, ".md"),
            ConvertOutputFormat::Lance => unreachable!("Lance output handled by caller"),
        };
        let relative = PathBuf::from(format!(
            "{}{}",
            sanitize_session_filename(&story.session_id),
            suffix
        ));
        anyhow::ensure!(
            destinations.insert(relative.clone()),
            "multiple sessions map to output filename {}",
            relative.display()
        );
        let text = from_storyline(chronicle_format, story).map_err(anyhow::Error::from)?;
        write_document_file(root, &relative, &text, force)?;
    }
    Ok(stories.len())
}

fn prepare_output_directory(root: &Path) -> Result<()> {
    if root.exists() {
        anyhow::ensure!(
            root.is_dir(),
            "conversion output is not a directory: {}",
            root.display()
        );
    } else {
        fs::create_dir_all(root)
            .with_context(|| format!("create conversion output {}", root.display()))?;
    }
    Ok(())
}

fn write_document_file(root: &Path, relative: &Path, text: &str, force: bool) -> Result<()> {
    let destination = root.join(relative);
    if destination.exists() && !force {
        anyhow::bail!(
            "refusing to overwrite {}; pass --force to replace it",
            destination.display()
        );
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create conversion output {}", parent.display()))?;
    }
    let mut bytes = text.as_bytes().to_vec();
    if !text.ends_with('\n') {
        bytes.push(b'\n');
    }
    fs::write(&destination, bytes)
        .with_context(|| format!("write converted document {}", destination.display()))
}

fn read_document_files(input: &Path, format: ConvertInputFormat) -> Result<Vec<PathBuf>> {
    if input.is_file() {
        return Ok(vec![input.to_path_buf()]);
    }
    anyhow::ensure!(
        input.is_dir(),
        "conversion input does not exist: {}",
        input.display()
    );
    let mut files = fs::read_dir(input)
        .with_context(|| format!("read conversion input {}", input.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    files.retain(|path| match format {
        ConvertInputFormat::Storyline => path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".storyline.json")),
        ConvertInputFormat::Agenticmd => {
            path.extension().and_then(|value| value.to_str()) == Some("md")
        }
        ConvertInputFormat::Actf => path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".json")),
        _ => false,
    });
    files.sort();
    anyhow::ensure!(
        !files.is_empty(),
        "conversion input {} contains no {} documents",
        input.display(),
        input_format_name(format)
    );
    Ok(files)
}

fn detect_input_format(input: &str) -> Result<ConvertInputFormat> {
    if is_object_store_uri(input) {
        return Ok(ConvertInputFormat::Lance);
    }
    let path = Path::new(input);
    if path.is_dir() && path.join("CURRENT").is_file() {
        return Ok(ConvertInputFormat::Lance);
    }
    if path.is_file() {
        let text = fs::read_to_string(path)
            .with_context(|| format!("read conversion input {}", path.display()))?;
        return detected_chronicle_format(path, &text)?.with_context(|| {
            format!(
                "cannot detect conversion input format for {}",
                path.display()
            )
        });
    }
    if path.is_dir() {
        let mut detected = None;
        for entry in fs::read_dir(path)
            .with_context(|| format!("read conversion input {}", path.display()))?
        {
            let file = entry?.path();
            if !file.is_file() {
                continue;
            }
            let text = fs::read_to_string(&file)
                .with_context(|| format!("read conversion input {}", file.display()))?;
            let Some(format) = detected_chronicle_format(&file, &text)? else {
                continue;
            };
            if let Some(previous) = detected {
                anyhow::ensure!(
                    previous == format,
                    "mixed conversion input formats in {}",
                    path.display()
                );
            } else {
                detected = Some(format);
            }
        }
        return detected.with_context(|| {
            format!(
                "cannot detect conversion input format in {}",
                path.display()
            )
        });
    }
    anyhow::bail!("conversion input does not exist: {input}")
}

fn detected_chronicle_format(path: &Path, text: &str) -> Result<Option<ConvertInputFormat>> {
    let content = if matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("jsonl" | "ndjson")
    ) {
        text.lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
    } else {
        text
    };
    Ok(
        match detect_format(Some(path), Some(content)).map_err(anyhow::Error::from)? {
            Some(ChronicleFormat::Atif) => Some(ConvertInputFormat::Atif),
            Some(ChronicleFormat::Actf) => Some(ConvertInputFormat::Actf),
            Some(ChronicleFormat::OpenaiMsg) => Some(ConvertInputFormat::OpenaiMsg),
            Some(ChronicleFormat::Storyline) => Some(ConvertInputFormat::Storyline),
            Some(ChronicleFormat::Agenticmd) => Some(ConvertInputFormat::Agenticmd),
            Some(ChronicleFormat::Events) => None,
            None => None,
        },
    )
}

fn is_object_store_uri(input: &str) -> bool {
    ["s3://", "az://", "gs://"]
        .iter()
        .any(|prefix| input.starts_with(prefix))
}

fn input_format_name(format: ConvertInputFormat) -> &'static str {
    match format {
        ConvertInputFormat::Auto => "auto",
        ConvertInputFormat::Atif => "atif",
        ConvertInputFormat::Actf => "actf",
        ConvertInputFormat::OpenaiMsg => "openai_msg",
        ConvertInputFormat::Storyline => "storyline",
        ConvertInputFormat::Agenticmd => "agenticmd",
        ConvertInputFormat::Lance => "lance",
    }
}

fn output_format_name(format: ConvertOutputFormat) -> &'static str {
    match format {
        ConvertOutputFormat::Atif => "atif",
        ConvertOutputFormat::Actf => "actf",
        ConvertOutputFormat::OpenaiMsg => "openai_msg",
        ConvertOutputFormat::Storyline => "storyline",
        ConvertOutputFormat::Agenticmd => "agenticmd",
        ConvertOutputFormat::Lance => "lance",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn openai_lance_openai_conversion_is_lossless() {
        let temporary = tempfile::tempdir().unwrap();
        let input = temporary.path().join("input.json");
        let lance = temporary.path().join("store");
        let output = temporary.path().join("output");
        let value = serde_json::json!([
            {
                "id":"event-1","session_id":"session-1","step_id":1,
                "agent_model":"gpt-test","unknown":null,
                "messages":[
                    {"role":"user","content":"hello"},
                    {"role":"assistant","content":"world"}
                ],
                "response":{"role":"assistant","content":""}
            }
        ]);
        fs::write(&input, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

        run_convert(ConvertArgs {
            input: input.to_string_lossy().into_owned(),
            output: lance.to_string_lossy().into_owned(),
            from: ConvertInputFormat::Auto,
            to: ConvertOutputFormat::Lance,
            force: false,
        })
        .await
        .unwrap();
        run_convert(ConvertArgs {
            input: lance.to_string_lossy().into_owned(),
            output: output.to_string_lossy().into_owned(),
            from: ConvertInputFormat::Auto,
            to: ConvertOutputFormat::OpenaiMsg,
            force: false,
        })
        .await
        .unwrap();

        let recovered: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("input.json")).unwrap()).unwrap();
        assert_eq!(recovered, value);
    }

    #[tokio::test]
    async fn actf_lance_actf_conversion_is_lossless() {
        let temporary = tempfile::tempdir().unwrap();
        let input = temporary.path().join("task.json");
        let lance = temporary.path().join("store");
        let output = temporary.path().join("output");
        let value = serde_json::json!({
            "task_id":"actf-task","category":"software-engineering","k":1,
            "correct":false,"attempts_tried":1,"solved_at":null,
            "custom_root":{"preserved":true},
            "attempts":{"1":{
                "correct":false,"final_answer":null,"ground_truth":"expected",
                "trajectory":{
                    "schema_version":"ACTF_v1.0",
                    "steps":[{
                        "step_id":1,
                        "assistant_content":{
                            "content":"done","reasoning_content":"inspect",
                            "tool_calls":[{"type":"tool_use","id":"call-1","name":"Bash","input":{"command":"pwd"}}]
                        },
                        "metric":{"prompt_tokens_len":2,"completion_tokens_len":3,"llm_infer_ms":null,"env_action_ms":4,"stop_reason":null},
                        "system_prompt":"system","user_content":"task",
                        "tools":[{"type":"tool_use","id":"call-1","name":"Bash","input":{"command":"pwd"}}],
                        "observation":[{"tool_use_id":"call-1","type":"tool_result","content":[{"text":"/app"}],"is_error":false}],
                        "started_at":"2026-01-01 00:00:00+00:00","finished_at":"2026-01-01 00:00:01+00:00"
                    }],
                    "started_at":"2026-01-01 00:00:00+00:00","finished_at":"2026-01-01 00:00:01+00:00"
                },
                "status":"completed","score":null,"error":"","artifacts":{},
                "extra":{},"analysis_result":{},"meta":{}
            }}
        });
        fs::write(&input, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

        run_convert(ConvertArgs {
            input: input.to_string_lossy().into_owned(),
            output: lance.to_string_lossy().into_owned(),
            from: ConvertInputFormat::Auto,
            to: ConvertOutputFormat::Lance,
            force: false,
        })
        .await
        .unwrap();
        run_convert(ConvertArgs {
            input: lance.to_string_lossy().into_owned(),
            output: output.to_string_lossy().into_owned(),
            from: ConvertInputFormat::Lance,
            to: ConvertOutputFormat::Actf,
            force: false,
        })
        .await
        .unwrap();

        let recovered: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("actf-task.actf.json")).unwrap()).unwrap();
        assert_eq!(recovered, value);
    }

    #[test]
    fn detects_openai_array() {
        let temporary = tempfile::tempdir().unwrap();
        let input = temporary.path().join("input.json");
        fs::write(&input, r#"[{"session_id":"s","step_id":1,"messages":[]}]"#).unwrap();
        assert_eq!(
            detect_input_format(input.to_str().unwrap()).unwrap(),
            ConvertInputFormat::OpenaiMsg
        );
    }

    #[tokio::test]
    async fn converts_atif_corpus_to_storyline_documents() {
        let input = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../persisting-pchronicle/tests/fixtures/atif/dialogue_10.json");
        let temporary = tempfile::tempdir().unwrap();
        let output = temporary.path().join("storylines");
        run_convert(ConvertArgs {
            input: input.to_string_lossy().into_owned(),
            output: output.to_string_lossy().into_owned(),
            from: ConvertInputFormat::Auto,
            to: ConvertOutputFormat::Storyline,
            force: false,
        })
        .await
        .unwrap();

        let converted = output.join("fixture-dialogue_10.storyline.json");
        let story = fs::read_to_string(converted).unwrap();
        let parsed = into_storyline(ChronicleFormat::Storyline, &story).unwrap();
        assert_eq!(parsed.session_id, "fixture-dialogue_10");
        assert_eq!(parsed.turns.len(), 10);
    }
}
