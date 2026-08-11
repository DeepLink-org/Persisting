use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use persisting_pchronicle::convert::storyline_to_events;
use persisting_pchronicle::{
    actf_to_storylines, ActfDocument, ChronicleQueryEngine, EventIdentity, EventRecord,
    FileTrajectoryDataSource, StorylineDataFusionTableNames, StorylineTurn,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{RunSummary, SessionQuery, TrajectoryTurnView, WireToolCall};

#[derive(Clone, Debug)]
pub(crate) struct DatasetStore {
    runs: Vec<DatasetRun>,
}

#[derive(Clone, Debug)]
struct DatasetRun {
    summary: RunSummary,
    path: PathBuf,
    source: DatasetSource,
}

#[derive(Clone, Copy, Debug)]
enum DatasetSource {
    Gateway,
    Actf,
}

#[derive(Debug)]
pub(crate) struct LoadedDatasetRun {
    pub(crate) summary: RunSummary,
    pub(crate) records: Vec<EventRecord>,
    pub(crate) turns: Vec<TrajectoryTurnView>,
}

#[derive(Deserialize)]
struct GatewayCatalogRow {
    session_id: String,
    #[serde(default)]
    agent_model: String,
    #[serde(default)]
    job_id: String,
    #[serde(default)]
    is_session_completed: bool,
    #[serde(default)]
    is_terminal: bool,
}

#[derive(Deserialize)]
struct ActfCatalog {
    task_id: String,
    attempts: BTreeMap<String, ActfCatalogAttempt>,
}

#[derive(Deserialize)]
struct ActfCatalogAttempt {
    #[serde(default)]
    correct: bool,
    #[serde(default)]
    status: String,
    trajectory: ActfCatalogTrajectory,
}

#[derive(Deserialize)]
struct ActfCatalogTrajectory {
    steps: Vec<serde::de::IgnoredAny>,
}

impl DatasetStore {
    pub(crate) fn discover(storage: &str) -> anyhow::Result<Option<Self>> {
        let root = Path::new(storage);
        if !root.is_dir() {
            return Ok(None);
        }
        let mut paths = fs::read_dir(root)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
            })
            .collect::<Vec<_>>();
        paths.sort();

        let mut runs = Vec::new();
        for path in paths {
            let input = fs::read(&path)?;
            let first = input
                .iter()
                .copied()
                .find(|byte| !byte.is_ascii_whitespace());
            match first {
                Some(b'[') => discover_gateway_file(&path, &input, &mut runs)?,
                Some(b'{') => discover_actf_file(&path, &input, &mut runs)?,
                _ => {}
            }
        }
        if runs.is_empty() {
            Ok(None)
        } else {
            runs.sort_by(|left, right| {
                left.summary
                    .agent_id
                    .cmp(&right.summary.agent_id)
                    .then_with(|| left.summary.session_id.cmp(&right.summary.session_id))
            });
            Ok(Some(Self { runs }))
        }
    }

    pub(crate) fn summaries(&self) -> Vec<RunSummary> {
        self.runs.iter().map(|run| run.summary.clone()).collect()
    }

    pub(crate) fn contains(&self, query: &SessionQuery) -> bool {
        self.find(query).is_some()
    }

    pub(crate) fn summary(&self, query: &SessionQuery) -> Option<RunSummary> {
        self.find(query).map(|run| run.summary.clone())
    }

    pub(crate) fn fingerprint(&self, query: &SessionQuery) -> Option<String> {
        let run = self.find(query)?;
        let metadata = fs::metadata(&run.path).ok()?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |value| value.as_nanos());
        Some(format!(
            "{}:{}:{modified}",
            run.path.display(),
            metadata.len()
        ))
    }

    pub(crate) fn load(&self, query: &SessionQuery) -> anyhow::Result<LoadedDatasetRun> {
        let run = self
            .find(query)
            .ok_or_else(|| anyhow::anyhow!("dataset run was not found"))?;
        match run.source {
            DatasetSource::Gateway => load_gateway(run),
            DatasetSource::Actf => load_actf(run),
        }
    }

    pub(crate) fn query_engine(&self) -> anyhow::Result<(ChronicleQueryEngine, Vec<String>)> {
        let mut seen = HashSet::new();
        let mut inputs = self
            .runs
            .iter()
            .filter(|run| seen.insert(run.path.clone()))
            .map(|run| (run.path.clone(), run.source))
            .collect::<Vec<_>>();
        inputs.sort_by(|left, right| left.0.cmp(&right.0));
        let (first_path, first_source) = inputs
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("dataset contains no queryable files"))?;
        let first = file_query_source(&first_path, first_source)?;
        let engine = ChronicleQueryEngine::from_file_trajectory_source(first)?;
        let mut suffixes = vec![String::new()];
        for (index, (path, source)) in inputs.into_iter().skip(1).enumerate() {
            let suffix = format!("__source_{}", index + 1);
            let names = StorylineDataFusionTableNames {
                runs: format!("runs{suffix}"),
                steps: format!("steps{suffix}"),
                tool_calls: format!("tool_calls{suffix}"),
            };
            file_query_source(&path, source)?.register_as(engine.context(), &names)?;
            suffixes.push(suffix);
        }
        Ok((engine, suffixes))
    }

    fn find(&self, query: &SessionQuery) -> Option<&DatasetRun> {
        self.runs.iter().find(|run| {
            run.summary.agent_id == query.agent_id
                && run.summary.session_id == query.session_id
                && run.summary.root_session_id == query.root_session_id
        })
    }
}

fn file_query_source(
    path: &Path,
    source: DatasetSource,
) -> anyhow::Result<FileTrajectoryDataSource> {
    match source {
        DatasetSource::Gateway => FileTrajectoryDataSource::open_openai_msg(path),
        DatasetSource::Actf => FileTrajectoryDataSource::open_actf(path),
    }
}

fn discover_gateway_file(
    path: &Path,
    input: &[u8],
    runs: &mut Vec<DatasetRun>,
) -> anyhow::Result<()> {
    let Ok(rows) = serde_json::from_slice::<Vec<GatewayCatalogRow>>(input) else {
        return Ok(());
    };
    let mut sessions = HashMap::<String, (String, String, usize, bool)>::new();
    for row in rows {
        let entry = sessions.entry(row.session_id).or_insert_with(|| {
            (
                value_or(&row.agent_model, "probing-agent"),
                value_or(&row.job_id, file_stem(path)),
                0,
                false,
            )
        });
        entry.2 += 1;
        entry.3 |= row.is_session_completed || row.is_terminal;
    }
    for (session_id, (agent_id, job_id, steps, completed)) in sessions {
        let logical_path = dataset_run_path(path, &session_id, Some(&job_id));
        runs.push(DatasetRun {
            summary: RunSummary {
                model_name: Some(agent_id.clone()),
                agent_id,
                session_id,
                root_session_id: Some(job_id),
                path: logical_path,
                row_count: steps * 2,
                duplicate_event_ids: 0,
                status: if completed { "completed" } else { "active" }.into(),
            },
            path: path.to_path_buf(),
            source: DatasetSource::Gateway,
        });
    }
    Ok(())
}

fn discover_actf_file(path: &Path, input: &[u8], runs: &mut Vec<DatasetRun>) -> anyhow::Result<()> {
    let Ok(document) = serde_json::from_slice::<ActfCatalog>(input) else {
        return Ok(());
    };
    let multiple = document.attempts.len() > 1;
    for (attempt_id, attempt) in document.attempts {
        let session_id = if multiple {
            format!("{}#attempt-{attempt_id}", document.task_id)
        } else {
            document.task_id.clone()
        };
        let logical_path = dataset_run_path(path, &session_id, None);
        runs.push(DatasetRun {
            summary: RunSummary {
                agent_id: "actf-agent".into(),
                model_name: None,
                session_id,
                root_session_id: None,
                path: logical_path,
                row_count: attempt.trajectory.steps.len(),
                duplicate_event_ids: 0,
                status: if attempt.status.is_empty() {
                    if attempt.correct {
                        "completed"
                    } else {
                        "failed"
                    }
                    .into()
                } else {
                    attempt.status
                },
            },
            path: path.to_path_buf(),
            source: DatasetSource::Actf,
        });
    }
    Ok(())
}

fn load_gateway(run: &DatasetRun) -> anyhow::Result<LoadedDatasetRun> {
    let rows: Vec<Value> = serde_json::from_slice(&fs::read(&run.path)?)?;
    let mut records = Vec::new();
    let mut turns = Vec::new();
    for row in rows.into_iter().filter(|row| {
        row.get("session_id").and_then(Value::as_str) == Some(&run.summary.session_id)
    }) {
        let step = row.get("step_id").and_then(Value::as_u64).unwrap_or(0);
        let call_id = row
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("step-{step}"));
        let timestamp = gateway_timestamp(&row);
        let messages = row.get("messages").cloned().unwrap_or_else(|| json!([]));
        let captured_response = row.get("response").cloned().unwrap_or(Value::Null);
        let response = effective_agent_response(&messages, captured_response);
        let request_seq = records.len() as u64;
        let response_seq = request_seq + 1;
        let request_payload = json!({
            "model": row.get("agent_model"),
            "messages": messages.clone(),
            "step_id": step,
        });
        let response_payload = json!({
            "content": message_content(&response),
            "message": response,
            "usage": gateway_usage(&row),
            "reward": row.get("reward"),
            "step_reward": row.get("step_reward"),
        });
        records.push(dataset_event(
            &run.summary,
            request_seq,
            "llm.request",
            &call_id,
            timestamp.clone(),
            request_payload,
        ));
        records.push(dataset_event(
            &run.summary,
            response_seq,
            "llm.response",
            &call_id,
            timestamp.clone(),
            response_payload,
        ));

        let user_message = last_message_content(Some(&messages));
        turns.push(dataset_turn(
            turns.len() as i64 + 1,
            "user",
            "llm.request",
            user_message,
            timestamp.clone(),
            &call_id,
            request_seq,
            Vec::new(),
        ));
        let mut wire_tool_calls = Vec::new();
        crate::collect_wire_tool_calls(&response, &mut wire_tool_calls);
        if wire_tool_calls.is_empty() {
            if let Some(call) = embedded_tool_call(&message_content(&response)) {
                wire_tool_calls.push(call);
            }
        }
        let mut agent_turn = dataset_turn(
            turns.len() as i64 + 1,
            "agent",
            "llm.response",
            message_content(&response),
            timestamp,
            &call_id,
            response_seq,
            wire_tool_calls,
        );
        agent_turn.turn.model_name = row
            .get("agent_model")
            .and_then(Value::as_str)
            .map(str::to_owned);
        turns.push(agent_turn);
    }
    Ok(LoadedDatasetRun {
        summary: run.summary.clone(),
        records,
        turns,
    })
}

fn load_actf(run: &DatasetRun) -> anyhow::Result<LoadedDatasetRun> {
    let document: ActfDocument = serde_json::from_slice(&fs::read(&run.path)?)?;
    let story = actf_to_storylines(&document)?
        .into_iter()
        .find(|story| story.session_id == run.summary.session_id)
        .ok_or_else(|| anyhow::anyhow!("ACTF attempt was not found"))?;
    let records = storyline_to_events(&story)?.events;
    let turns = story
        .turns
        .into_iter()
        .enumerate()
        .map(|(index, mut turn)| {
            // ACTF provenance contains a lossless copy of the whole step. The UI already
            // exposes its projected fields, so do not send that large duplicate to browsers.
            turn.extra = None;
            let mut wire_tool_calls = Vec::new();
            if let Some(tool_calls) = &turn.tool_calls {
                for tool_call in tool_calls {
                    wire_tool_calls.push(WireToolCall {
                        id: Some(tool_call.tool_call_id.clone()),
                        name: tool_call.function_name.clone(),
                        arguments: tool_call.arguments.clone(),
                    });
                }
            }
            TrajectoryTurnView {
                event_seqs: records
                    .get(index)
                    .map(|event| vec![event.seq])
                    .unwrap_or_default(),
                call_id: None,
                turn,
                wire_tool_calls,
            }
        })
        .collect();
    Ok(LoadedDatasetRun {
        summary: run.summary.clone(),
        records,
        turns,
    })
}

fn dataset_event(
    run: &RunSummary,
    seq: u64,
    kind: &str,
    call_id: &str,
    timestamp: Option<String>,
    payload: Value,
) -> EventRecord {
    EventRecord {
        identity: EventIdentity {
            event_id: Some(format!("dataset:{}:{seq}", run.session_id)),
            producer: Some("probing-dataset".into()),
            ..EventIdentity::default()
        },
        seq,
        source: "probing".into(),
        kind: kind.into(),
        timestamp,
        session_id: Some(run.session_id.clone()),
        agent_id: Some(run.agent_id.clone()),
        parent_uuid: None,
        trace_id: None,
        call_id: Some(call_id.into()),
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload,
    }
}

#[allow(clippy::too_many_arguments)]
fn dataset_turn(
    id: i64,
    source: &str,
    kind: &str,
    message: Value,
    timestamp: Option<String>,
    call_id: &str,
    seq: u64,
    wire_tool_calls: Vec<WireToolCall>,
) -> TrajectoryTurnView {
    TrajectoryTurnView {
        turn: StorylineTurn {
            id,
            kind: Some(kind.into()),
            timestamp,
            source: source.into(),
            message,
            reasoning_content: None,
            reasoning_effort: None,
            tool_calls: None,
            observation: None,
            metrics: None,
            model_name: None,
            llm_call_count: (source == "agent").then_some(1),
            is_copied_context: None,
            latency_ms: None,
            ttft_ms: None,
            extra: Some(json!({"call_id": call_id, "seq": seq})),
        },
        call_id: Some(call_id.into()),
        event_seqs: vec![seq],
        wire_tool_calls,
    }
}

fn last_message_content(messages: Option<&Value>) -> Value {
    messages
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages
                .iter()
                .rev()
                .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
                .or_else(|| messages.last())
        })
        .map(message_content)
        .unwrap_or(Value::Null)
}

fn effective_agent_response(messages: &Value, captured: Value) -> Value {
    if !message_content(&captured)
        .as_str()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return captured;
    }
    messages
        .as_array()
        .and_then(|messages| {
            messages.iter().rev().find(|message| {
                message.get("role").and_then(Value::as_str) == Some("assistant")
                    && !message_content(message)
                        .as_str()
                        .unwrap_or_default()
                        .trim()
                        .is_empty()
            })
        })
        .cloned()
        .unwrap_or(captured)
}

fn embedded_tool_call(content: &Value) -> Option<WireToolCall> {
    let text = content.as_str()?;
    let name = extract_after(text, "<tool_call>")
        .or_else(|| extract_after(text, "<function="))
        .or_else(|| extract_between(text, "<function>", "</function>"))?;
    let name = name.trim().split(['>', '\n', '<']).next()?.trim();
    if name.is_empty() {
        return None;
    }
    let arguments = embedded_parameters(text);
    Some(WireToolCall {
        id: None,
        name: name.to_string(),
        arguments: Value::Object(arguments),
    })
}

fn embedded_parameters(text: &str) -> serde_json::Map<String, Value> {
    let mut arguments = serde_json::Map::new();
    let mut remaining = text;
    while let Some((_, after_marker)) = remaining.split_once("<parameter=") {
        let Some((key, after_opening)) = after_marker.split_once('>') else {
            break;
        };
        let key = key.trim();
        if key.is_empty() {
            remaining = after_opening;
            continue;
        }
        let (value, rest) = after_opening
            .split_once("</parameter>")
            .unwrap_or((after_opening, ""));
        arguments.insert(key.to_string(), Value::String(value.trim().to_string()));
        remaining = rest;
    }
    arguments
}

fn extract_after<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    text.split_once(marker).map(|(_, value)| value)
}

fn extract_between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let value = extract_after(text, start)?;
    Some(value.split_once(end).map_or(value, |(value, _)| value))
}

fn message_content(message: &Value) -> Value {
    let content = message.get("content").unwrap_or(message);
    match content {
        Value::Array(parts) => {
            let text_parts = parts
                .iter()
                .filter_map(|part| {
                    part.get("text")
                        .and_then(Value::as_str)
                        .or_else(|| part.as_str())
                })
                .collect::<Vec<_>>();
            let text = text_parts
                .iter()
                .copied()
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                Value::String(text)
            } else if parts.iter().any(|part| {
                part.get("image_url").is_some_and(|value| !value.is_null())
                    || part
                        .get("image_bytes")
                        .is_some_and(|value| !value.is_null())
            }) {
                Value::String("[Image response]".into())
            } else if !text_parts.is_empty() {
                Value::String(String::new())
            } else {
                content.clone()
            }
        }
        _ => content.clone(),
    }
}

fn gateway_timestamp(row: &Value) -> Option<String> {
    row.get("created_at").and_then(|value| {
        value
            .as_str()
            .map(str::to_owned)
            .or_else(|| value.as_i64().map(|epoch| epoch.to_string()))
    })
}

fn gateway_usage(row: &Value) -> Value {
    let meta = row
        .get("meta_json")
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    json!({
        "prompt_tokens": metadata_value(row, meta.as_ref(), "prompt_tokens"),
        "completion_tokens": metadata_value(row, meta.as_ref(), "completion_tokens"),
        "total_tokens": metadata_value(row, meta.as_ref(), "total_tokens"),
    })
}

fn metadata_value(row: &Value, meta: Option<&Value>, key: &str) -> Value {
    row.get(key)
        .or_else(|| meta.and_then(|value| value.get(key)))
        .cloned()
        .unwrap_or(Value::Null)
}

fn value_or(value: &str, fallback: impl Into<String>) -> String {
    if value.is_empty() {
        fallback.into()
    } else {
        value.into()
    }
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("probing")
        .to_string()
}

fn dataset_run_path(path: &Path, session_id: &str, root_session_id: Option<&str>) -> String {
    let source = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dataset");
    match root_session_id {
        Some(root) if root != session_id => format!("{source}/{root}/{session_id}"),
        _ => format!("{source}/{session_id}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_and_loads_gateway_dataset_arrays() {
        let root = std::env::temp_dir().join(format!(
            "persisting-pchronicle-dataset-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("gateway.json"),
            serde_json::to_vec(&json!([{
                "id":"evt-1", "session_id":"session-1", "agent_model":"model-1",
                "job_id":"job-1", "step_id":1, "is_terminal":true,
                "messages":[{"role":"user","content":[{"type":"text","text":"hello"}]}],
                "response":{"role":"assistant","content":[{"type":"text","text":"world"}]}
            }]))
            .unwrap(),
        )
        .unwrap();

        let store = DatasetStore::discover(root.to_str().unwrap())
            .unwrap()
            .unwrap();
        let summaries = store.summaries();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].row_count, 2);
        assert_eq!(summaries[0].path, "gateway.json/job-1/session-1");
        let query = SessionQuery {
            agent_id: "model-1".into(),
            session_id: "session-1".into(),
            root_session_id: Some("job-1".into()),
            offset: None,
            limit: None,
        };
        let loaded = store.load(&query).unwrap();
        assert_eq!(loaded.records.len(), 2);
        assert_eq!(loaded.turns.len(), 2);
        assert_eq!(loaded.turns[0].turn.message, "hello");
        assert_eq!(loaded.turns[1].turn.message, "world");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_multimodal_text_is_not_exposed_as_protocol_json() {
        let message = json!({
            "role":"assistant",
            "content":[{
                "type":"text", "text":"", "image_url":null, "image_bytes":null,
                "input_audio":null, "media_type":null
            }]
        });
        assert_eq!(message_content(&message), Value::String(String::new()));
    }

    #[test]
    fn gateway_falls_back_to_assistant_message_and_extracts_embedded_tool() {
        let messages = json!([
            {"role":"user","content":[{"type":"text","text":"result"}]},
            {"role":"assistant","content":[{"type":"text","text":"<function=execute_bash>\n<parameter=command>pwd</parameter>"}]}
        ]);
        let captured = json!({"role":"assistant","content":[{"type":"text","text":""}]});
        let response = effective_agent_response(&messages, captured);
        assert!(message_content(&response)
            .as_str()
            .unwrap()
            .contains("execute_bash"));
        let call = embedded_tool_call(&message_content(&response)).unwrap();
        assert_eq!(call.name, "execute_bash");
        assert_eq!(call.arguments["command"], "pwd");
    }

    #[test]
    fn embedded_tool_extracts_arbitrary_multiline_parameters() {
        let content = Value::String(
            "<tool_call>execute_ipython_cell\n<parameter=code>import os\nprint(os.getcwd())</parameter>\n<parameter=timeout>30</parameter>"
                .into(),
        );
        let call = embedded_tool_call(&content).unwrap();
        assert_eq!(call.name, "execute_ipython_cell");
        assert_eq!(
            call.arguments["code"],
            Value::String("import os\nprint(os.getcwd())".into())
        );
        assert_eq!(call.arguments["timeout"], "30");
    }
}
