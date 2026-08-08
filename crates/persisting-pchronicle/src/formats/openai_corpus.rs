//! Lossless JSON-model import/export for OpenAI-message trajectory corpora.
//!
//! Unlike [`super::openai_msg`], which models one `session_steps.json`
//! document, this adapter accepts a top-level array containing rows from many
//! sessions.  The original row is retained in Storyline `extra`, so the
//! normalized three-table projection remains queryable without making the
//! reverse conversion depend on that projection.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use chrono::{SecondsFormat, TimeZone, Utc};
use serde_json::{json, Map, Value};

use crate::formats::storyline::{
    StorylineAgent, StorylineDocument, StorylineToolCall, StorylineTurn, STORYLINE_SCHEMA_VERSION,
};
use crate::{Error, Result};

const LOSSLESS_FILE_KEY: &str = "_pchronicle_openai_file";
const LOSSLESS_RECORD_KEY: &str = "_pchronicle_openai_record";
const LOSSLESS_VERSION: u64 = 1;

/// One source JSON file reconstructed from lossless OpenAI import metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveredOpenaiMsgFile {
    pub relative_path: PathBuf,
    pub document: Value,
}

/// Replayable reader that converts OpenAI corpus files into Storylines.
///
/// Regular JSON files may be a bare row array or a `session_steps` envelope.
/// Directories are traversed in stable relative-path order.
pub struct OpenaiMsgCorpusReader {
    stories: std::vec::IntoIter<StorylineDocument>,
}

impl OpenaiMsgCorpusReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let input = path.as_ref();
        let files = input_files(input)?;
        let directory_root = input.is_dir().then_some(input);
        let mut stories = Vec::new();
        let mut session_ids = HashSet::new();

        for file in files {
            let relative_path = source_relative_path(directory_root, &file)?;
            let text = fs::read_to_string(&file)?;
            let document: Value = serde_json::from_str(&text)?;
            for story in parse_openai_msg_corpus_value(&document, &relative_path)? {
                if !session_ids.insert(story.session_id.clone()) {
                    return Err(Error::DuplicateSession(story.session_id));
                }
                stories.push(story);
            }
        }

        if stories.is_empty() {
            return Err(Error::Other(format!(
                "OpenAI corpus requires at least one trajectory: {}",
                input.display()
            )));
        }
        Ok(Self {
            stories: stories.into_iter(),
        })
    }
}

impl Iterator for OpenaiMsgCorpusReader {
    type Item = Result<StorylineDocument>;

    fn next(&mut self) -> Option<Self::Item> {
        self.stories.next().map(Ok)
    }
}

/// Parse one OpenAI corpus JSON value into one Storyline per session.
pub fn parse_openai_msg_corpus_value(
    document: &Value,
    relative_path: impl AsRef<Path>,
) -> Result<Vec<StorylineDocument>> {
    let relative_path = validate_relative_path(relative_path.as_ref())?
        .to_string_lossy()
        .into_owned();
    let (kind, envelope, records) = match document {
        Value::Array(records) => ("array", None, records.clone()),
        Value::Object(root) => {
            let records = root
                .get("session_steps")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    Error::Other("OpenAI corpus object requires a session_steps array".to_string())
                })?
                .clone();
            let mut metadata = root.clone();
            metadata.remove("session_steps");
            ("envelope", Some(Value::Object(metadata)), records)
        }
        _ => {
            return Err(Error::Other(
                "OpenAI corpus must be a JSON array or session_steps object".to_string(),
            ))
        }
    };

    let file_metadata = json!({
        "version": LOSSLESS_VERSION,
        "relative_path": relative_path,
        "document_kind": kind,
        "envelope": envelope,
    });
    let mut groups: Vec<(String, Vec<(usize, Value)>)> = Vec::new();
    let mut group_indexes = HashMap::<String, usize>::new();
    for (ordinal, record) in records.into_iter().enumerate() {
        let object = record.as_object().ok_or_else(|| {
            Error::Other(format!(
                "OpenAI corpus {} row {} must be an object",
                relative_path, ordinal
            ))
        })?;
        let session_id = required_string(object, "session_id", &relative_path, ordinal)?;
        let index = if let Some(index) = group_indexes.get(&session_id) {
            *index
        } else {
            let index = groups.len();
            group_indexes.insert(session_id.clone(), index);
            groups.push((session_id, Vec::new()));
            index
        };
        groups[index].1.push((ordinal, record));
    }

    groups
        .into_iter()
        .map(|(session_id, records)| {
            rows_to_storyline(&session_id, records, &relative_path, &file_metadata)
        })
        .collect()
}

/// Recover original OpenAI files from Storylines produced by the corpus reader.
///
/// This is intentionally strict: Storylines without complete lossless metadata
/// are rejected instead of being silently synthesized from normalized fields.
pub fn recover_openai_msg_files(
    stories: &[StorylineDocument],
) -> Result<Vec<RecoveredOpenaiMsgFile>> {
    #[derive(Clone)]
    struct FileGroup {
        kind: String,
        envelope: Option<Value>,
        records: Vec<(u64, Value)>,
    }

    let mut files = HashMap::<PathBuf, FileGroup>::new();
    for story in stories {
        let file = story
            .extra
            .as_ref()
            .and_then(|extra| extra.get(LOSSLESS_FILE_KEY))
            .and_then(Value::as_object)
            .ok_or_else(|| {
                Error::Other(format!(
                    "Storyline '{}' has no lossless OpenAI file metadata",
                    story.session_id
                ))
            })?;
        validate_version(file, LOSSLESS_FILE_KEY)?;
        let relative_path = file
            .get("relative_path")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Other("OpenAI file metadata missing relative_path".into()))?;
        let relative_path = validate_relative_path(Path::new(relative_path))?;
        let kind = file
            .get("document_kind")
            .and_then(Value::as_str)
            .filter(|kind| matches!(*kind, "array" | "envelope"))
            .ok_or_else(|| Error::Other("invalid OpenAI document_kind".into()))?
            .to_string();
        let envelope = file.get("envelope").filter(|v| !v.is_null()).cloned();

        let group = files
            .entry(relative_path.clone())
            .or_insert_with(|| FileGroup {
                kind: kind.clone(),
                envelope: envelope.clone(),
                records: Vec::new(),
            });
        if group.kind != kind || group.envelope != envelope {
            return Err(Error::Other(format!(
                "conflicting OpenAI file metadata for {}",
                relative_path.display()
            )));
        }

        for turn in &story.turns {
            let record = turn
                .extra
                .as_ref()
                .and_then(|extra| extra.get(LOSSLESS_RECORD_KEY))
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    Error::Other(format!(
                        "Storyline '{}' step {} has no lossless OpenAI record",
                        story.session_id, turn.id
                    ))
                })?;
            validate_version(record, LOSSLESS_RECORD_KEY)?;
            let record_path = record
                .get("relative_path")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Other("OpenAI record missing relative_path".into()))?;
            if validate_relative_path(Path::new(record_path))? != relative_path {
                return Err(Error::Other(format!(
                    "OpenAI record path conflicts with Storyline '{}' file metadata",
                    story.session_id
                )));
            }
            let ordinal = record
                .get("ordinal")
                .and_then(Value::as_u64)
                .ok_or_else(|| Error::Other("OpenAI record missing ordinal".into()))?;
            let raw = record
                .get("value")
                .cloned()
                .ok_or_else(|| Error::Other("OpenAI record missing value".into()))?;
            group.records.push((ordinal, raw));
        }
    }

    let mut output = Vec::with_capacity(files.len());
    for (relative_path, mut group) in files {
        group.records.sort_by_key(|(ordinal, _)| *ordinal);
        for pair in group.records.windows(2) {
            if pair[0].0 == pair[1].0 {
                return Err(Error::Other(format!(
                    "duplicate OpenAI row ordinal {} in {}",
                    pair[0].0,
                    relative_path.display()
                )));
            }
        }
        for (expected, (actual, _)) in group.records.iter().enumerate() {
            if *actual != expected as u64 {
                return Err(Error::Other(format!(
                    "missing OpenAI row ordinal {} in {} (found {})",
                    expected,
                    relative_path.display(),
                    actual
                )));
            }
        }
        let records = group
            .records
            .into_iter()
            .map(|(_, record)| record)
            .collect::<Vec<_>>();
        let document = match group.kind.as_str() {
            "array" => Value::Array(records),
            "envelope" => {
                let mut envelope = group
                    .envelope
                    .and_then(|value| value.as_object().cloned())
                    .ok_or_else(|| {
                        Error::Other(format!(
                            "OpenAI envelope metadata missing for {}",
                            relative_path.display()
                        ))
                    })?;
                envelope.insert("session_steps".into(), Value::Array(records));
                Value::Object(envelope)
            }
            _ => unreachable!("document kind validated above"),
        };
        output.push(RecoveredOpenaiMsgFile {
            relative_path,
            document,
        });
    }
    output.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(output)
}

/// Whether a Storyline carries the versioned provenance required for strict
/// OpenAI corpus recovery.
pub fn is_lossless_openai_storyline(story: &StorylineDocument) -> bool {
    story
        .extra
        .as_ref()
        .and_then(|extra| extra.get(LOSSLESS_FILE_KEY))
        .and_then(Value::as_object)
        .is_some_and(|metadata| {
            metadata.get("version").and_then(Value::as_u64) == Some(LOSSLESS_VERSION)
        })
}

fn rows_to_storyline(
    session_id: &str,
    mut records: Vec<(usize, Value)>,
    relative_path: &str,
    file_metadata: &Value,
) -> Result<StorylineDocument> {
    records.sort_by_key(|(_, row)| row.get("step_id").and_then(Value::as_i64));
    let mut seen_steps = HashSet::new();
    let mut turns = Vec::with_capacity(records.len());
    let mut agent_source = None;
    let mut first_model: Option<String> = None;

    for (ordinal, raw) in records {
        let row = raw
            .as_object()
            .expect("rows were validated as objects during grouping");
        let step_id = row.get("step_id").and_then(Value::as_i64).ok_or_else(|| {
            Error::Other(format!(
                "OpenAI corpus {} row {} requires integer step_id",
                relative_path, ordinal
            ))
        })?;
        if !seen_steps.insert(step_id) {
            return Err(Error::DuplicateStep {
                session_id: session_id.to_string(),
                step_id,
            });
        }
        let meta = parsed_meta(row);
        let env_state = parsed_env_state(meta.as_ref());
        let model = row
            .get("agent_model")
            .or_else(|| row.get("llm_model"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if first_model.is_none() {
            first_model = model.clone();
        }
        if agent_source.is_none() {
            agent_source = meta
                .as_ref()
                .and_then(|value| value.get("source"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        }

        let output = select_output_message(row).ok_or_else(|| {
            Error::Other(format!(
                "OpenAI corpus {} row {} has no assistant output",
                relative_path, ordinal
            ))
        })?;
        let tool_calls = parse_tool_calls(output.get("tool_calls"));
        let message = output.get("content").cloned().unwrap_or(Value::Null);
        let metrics = normalized_metrics(row, env_state.as_ref());
        let timestamp = env_state
            .as_ref()
            .and_then(|state| state.get("created_at"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| row.get("created_at").and_then(normalize_timestamp));
        let latency_ms = env_state
            .as_ref()
            .and_then(|state| state.get("total_latency_ms"))
            .and_then(number_to_i64);
        let ttft_ms = env_state
            .as_ref()
            .and_then(|state| state.get("ttft_ms"))
            .and_then(number_to_i64);

        turns.push(StorylineTurn {
            id: step_id,
            kind: tool_calls.as_ref().map(|_| "autonomous".to_string()),
            timestamp,
            source: "agent".into(),
            message,
            reasoning_content: None,
            reasoning_effort: None,
            tool_calls,
            observation: None,
            metrics,
            model_name: model,
            llm_call_count: Some(1),
            is_copied_context: None,
            latency_ms,
            ttft_ms,
            extra: Some(json!({
                "_pchronicle_openai_record": {
                    "version": LOSSLESS_VERSION,
                    "relative_path": relative_path,
                    "ordinal": ordinal,
                    "value": raw,
                }
            })),
        });
    }

    let final_metrics = turns.last().and_then(|turn| turn.metrics.clone());
    let agent_id = agent_source.unwrap_or_else(|| "openai-import".into());
    Ok(StorylineDocument {
        schema_version: STORYLINE_SCHEMA_VERSION.into(),
        run_id: None,
        session_id: session_id.to_string(),
        agent: StorylineAgent {
            id: agent_id.clone(),
            name: Some(agent_id),
            version: Some("0".into()),
            model_name: first_model,
            tool_definitions: None,
            extra: None,
        },
        parent: None,
        child_session_ids: None,
        notes: None,
        final_metrics,
        continued_trajectory_ref: None,
        extra: Some(json!({ "_pchronicle_openai_file": file_metadata })),
        turns,
    })
}

fn input_files(input: &Path) -> Result<Vec<PathBuf>> {
    if input.is_file() {
        return Ok(vec![input.to_path_buf()]);
    }
    if !input.is_dir() {
        return Err(Error::Other(format!(
            "OpenAI corpus path does not exist: {}",
            input.display()
        )));
    }
    let mut files = fs::read_dir(input)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    files.retain(|path| path.extension().and_then(|value| value.to_str()) == Some("json"));
    files.sort();
    if files.is_empty() {
        return Err(Error::Other(format!(
            "OpenAI corpus directory contains no JSON files: {}",
            input.display()
        )));
    }
    Ok(files)
}

fn source_relative_path(root: Option<&Path>, file: &Path) -> Result<String> {
    let path = match root {
        Some(root) => file.strip_prefix(root).map_err(|_| {
            Error::Other(format!(
                "cannot make {} relative to {}",
                file.display(),
                root.display()
            ))
        })?,
        None => Path::new(file.file_name().ok_or_else(|| {
            Error::Other(format!("input file has no filename: {}", file.display()))
        })?),
    };
    validate_relative_path(path).map(|path| path.to_string_lossy().into_owned())
}

fn validate_relative_path(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(Error::Other(format!(
            "OpenAI source path must be non-empty and relative: {}",
            path.display()
        )));
    }
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::Other(format!(
            "OpenAI source path contains unsafe components: {}",
            path.display()
        )));
    }
    Ok(path.to_path_buf())
}

fn required_string(
    row: &Map<String, Value>,
    field: &str,
    path: &str,
    ordinal: usize,
) -> Result<String> {
    row.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            Error::Other(format!(
                "OpenAI corpus {} row {} requires non-empty {}",
                path, ordinal, field
            ))
        })
}

fn parsed_meta(row: &Map<String, Value>) -> Option<Value> {
    match row.get("meta_json")? {
        Value::String(value) => serde_json::from_str(value).ok(),
        value @ Value::Object(_) => Some(value.clone()),
        _ => None,
    }
}

fn parsed_env_state(meta: Option<&Value>) -> Option<Value> {
    match meta?.get("env_state")? {
        Value::String(value) => serde_json::from_str(value).ok(),
        value @ Value::Object(_) => Some(value.clone()),
        _ => None,
    }
}

fn select_output_message(row: &Map<String, Value>) -> Option<&Map<String, Value>> {
    let response = row.get("response").and_then(Value::as_object);
    if response.is_some_and(message_has_output) {
        return response;
    }
    row.get("messages")?
        .as_array()?
        .iter()
        .rev()
        .filter_map(Value::as_object)
        .find(|message| {
            message.get("role").and_then(Value::as_str) == Some("assistant")
                && message_has_output(message)
        })
}

fn message_has_output(message: &Map<String, Value>) -> bool {
    let has_tools = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty());
    has_tools || message.get("content").is_some_and(content_has_value)
}

fn content_has_value(content: &Value) -> bool {
    match content {
        Value::Null => false,
        Value::String(value) => !value.is_empty(),
        Value::Array(values) => values.iter().any(content_has_value),
        Value::Object(value) => value
            .get("text")
            .map_or(!value.is_empty(), content_has_value),
        _ => true,
    }
}

fn parse_tool_calls(value: Option<&Value>) -> Option<Vec<StorylineToolCall>> {
    let calls = value?.as_array()?;
    let parsed = calls
        .iter()
        .filter_map(|call| {
            let call = call.as_object()?;
            let function = call.get("function")?.as_object()?;
            let tool_call_id = call.get("id")?.as_str()?.to_string();
            let function_name = function.get("name")?.as_str()?.to_string();
            if tool_call_id.is_empty() || function_name.is_empty() {
                return None;
            }
            let arguments = function.get("arguments").cloned().unwrap_or(Value::Null);
            let arguments = match arguments {
                Value::String(ref text) => {
                    serde_json::from_str(text).unwrap_or_else(|_| arguments.clone())
                }
                _ => arguments,
            };
            Some(StorylineToolCall {
                tool_call_id,
                function_name,
                arguments,
                duration_ms: None,
                extra: Some(Value::Object(call.clone())),
            })
        })
        .collect::<Vec<_>>();
    (!parsed.is_empty()).then_some(parsed)
}

fn normalized_metrics(row: &Map<String, Value>, env_state: Option<&Value>) -> Option<Value> {
    const ROW_FIELDS: &[&str] = &[
        "reward",
        "step_reward",
        "is_terminal",
        "is_truncated",
        "is_session_completed",
        "is_trainable",
    ];
    const ENV_FIELDS: &[&str] = &[
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "finish_reason",
        "status_code",
        "retry_count",
        "upstream_latency_ms",
        "gateway_overhead_ms",
        "total_latency_ms",
        "ttft_ms",
    ];
    let mut metrics = Map::new();
    for field in ROW_FIELDS {
        if let Some(value) = row.get(*field) {
            metrics.insert((*field).to_string(), value.clone());
        }
    }
    if let Some(env_state) = env_state.and_then(Value::as_object) {
        for field in ENV_FIELDS {
            if let Some(value) = env_state.get(*field) {
                metrics.insert((*field).to_string(), value.clone());
            }
        }
    }
    (!metrics.is_empty()).then_some(Value::Object(metrics))
}

fn normalize_timestamp(value: &Value) -> Option<String> {
    if let Some(value) = value.as_str() {
        return Some(value.to_string());
    }
    let seconds = value.as_f64()?;
    let whole = seconds.trunc() as i64;
    let nanos = ((seconds.fract().abs()) * 1_000_000_000.0).round() as u32;
    Utc.timestamp_opt(whole, nanos)
        .single()
        .map(|timestamp| timestamp.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn number_to_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value as i64))
}

fn validate_version(metadata: &Map<String, Value>, label: &str) -> Result<()> {
    if metadata.get("version").and_then(Value::as_u64) != Some(LOSSLESS_VERSION) {
        return Err(Error::Other(format!(
            "unsupported or missing {label} version"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StorylineLanceStore;

    fn corpus() -> Value {
        json!([
            {
                "id": "evt-2",
                "session_id": "s-1",
                "step_id": 2,
                "agent_model": "gpt-test",
                "created_at": 1_700_000_001,
                "messages": [
                    {"role":"user","content":[{"type":"text","text":"next"}]},
                    {"role":"assistant","content":[{"type":"text","text":"world"}]}
                ],
                "response": {"role":"assistant","content":[]},
                "reward": 1.0,
                "unknown": null
            },
            {
                "id": "evt-other",
                "session_id": "s-2",
                "step_id": 1,
                "agent_model": "gpt-test",
                "messages": [
                    {"role":"user","content":"tool"},
                    {"role":"assistant","content":null,"tool_calls":[{
                        "id":"call-1","type":"function",
                        "function":{"name":"lookup","arguments":"{\"q\":1}"}
                    }]}
                ],
                "response": {"role":"assistant","content":""}
            },
            {
                "id": "evt-1",
                "session_id": "s-1",
                "step_id": 1,
                "agent_model": "gpt-test",
                "created_at": 1_700_000_000,
                "messages": [
                    {"role":"system","content":"system"},
                    {"role":"user","content":"hello"},
                    {"role":"assistant","content":"answer"}
                ],
                "response": {"role":"assistant","content":""},
                "meta_json": "{\"source\":\"fixture\",\"env_state\":\"{\\\"created_at\\\":\\\"2026-01-01T00:00:00Z\\\",\\\"total_tokens\\\":3}\"}"
            }
        ])
    }

    #[test]
    fn corpus_roundtrip_is_json_semantically_lossless() {
        let input = corpus();
        let stories = parse_openai_msg_corpus_value(&input, "corpus.json").unwrap();
        assert_eq!(stories.len(), 2);
        assert_eq!(stories[0].turns.len(), 2);
        assert_eq!(stories[0].turns[0].id, 1);
        assert_eq!(stories[0].turns[1].id, 2);
        assert_eq!(stories[0].turns[0].message, json!("answer"));
        assert_eq!(stories[1].turns[0].tool_calls.as_ref().unwrap().len(), 1);

        let recovered = recover_openai_msg_files(&stories).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].relative_path, PathBuf::from("corpus.json"));
        assert_eq!(recovered[0].document, input);
    }

    #[test]
    fn envelope_roundtrip_preserves_root_metadata() {
        let input = json!({
            "format_version": 1,
            "session_id": "s-1",
            "custom": null,
            "session_steps": [corpus()[0].clone()]
        });
        let stories = parse_openai_msg_corpus_value(&input, "session_steps.json").unwrap();
        let recovered = recover_openai_msg_files(&stories).unwrap();
        assert_eq!(recovered[0].document, input);
    }

    #[test]
    fn recovery_rejects_unsafe_paths() {
        let error = parse_openai_msg_corpus_value(&corpus(), "../escape.json").unwrap_err();
        assert!(error.to_string().contains("unsafe"));
    }

    #[tokio::test]
    async fn corpus_import_and_recovery_roundtrip_through_lance() {
        let input = corpus();
        let expected = parse_openai_msg_corpus_value(&input, "corpus.json").unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(temporary.path()).await.unwrap();
        store.replace_storylines(&expected).await.unwrap();

        let session_ids = expected
            .iter()
            .map(|story| story.session_id.clone())
            .collect::<Vec<_>>();
        let restored = store
            .get_storylines(&session_ids)
            .await
            .unwrap()
            .into_iter()
            .map(Option::unwrap)
            .collect::<Vec<_>>();
        let recovered = recover_openai_msg_files(&restored).unwrap();

        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].relative_path, PathBuf::from("corpus.json"));
        assert_eq!(recovered[0].document, input);
    }
}
