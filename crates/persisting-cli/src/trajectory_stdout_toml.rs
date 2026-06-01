//! `trajectory` 子命令成功响应在 CLI **stdout** 上以 **TOML** 打印（与默认写入格式一致）。

use anyhow::{Context, Result};
use persisting_proto::{
    JudgeMethod, JudgeScope, SessionJudgeStats, TrajectoryAppendResponse,
    TrajectoryExtractResponse, TrajectoryJudgeResponse, TrajectoryJudgeStatsResponse,
    TrajectoryMaterializeResponse, TrajectoryReplayResponse, TrajectoryStatsResponse,
    TrajectoryTruncateResponse,
};
use serde_json::Value as Json;

pub fn print_trajectory_append_as_toml(resp: &TrajectoryAppendResponse) -> Result<()> {
    let mut root = toml::map::Map::new();
    root.insert("storage".into(), toml::Value::String(resp.storage.clone()));
    root.insert(
        "agent_id".into(),
        toml::Value::String(resp.agent_id.clone()),
    );
    root.insert(
        "session_id".into(),
        toml::Value::String(resp.session_id.clone()),
    );
    root.insert(
        "accepted_records".into(),
        toml::Value::Integer(i64::try_from(resp.accepted_records).unwrap_or(i64::MAX)),
    );
    root.insert("dataset".into(), toml::Value::String(resp.dataset.clone()));
    root.insert("status".into(), toml::Value::String(resp.status.clone()));
    root.insert("note".into(), toml::Value::String(resp.note.clone()));
    emit_root(&root, "append")
}

pub fn print_trajectory_stats_as_toml(resp: &TrajectoryStatsResponse) -> Result<()> {
    emit_root(&stats_response_to_map(resp), "stats")
}

fn stats_response_to_map(resp: &TrajectoryStatsResponse) -> toml::map::Map<String, toml::Value> {
    let mut root = toml::map::Map::new();
    root.insert("storage".into(), toml::Value::String(resp.storage.clone()));
    root.insert(
        "agent_id".into(),
        toml::Value::String(resp.agent_id.clone()),
    );
    root.insert(
        "session_id".into(),
        toml::Value::String(resp.session_id.clone()),
    );
    root.insert("dataset".into(), toml::Value::String(resp.dataset.clone()));
    root.insert(
        "row_count".into(),
        toml::Value::Integer(i64::try_from(resp.row_count).unwrap_or(i64::MAX)),
    );
    if let Some(v) = resp.manifest_version {
        root.insert(
            "manifest_version".into(),
            toml::Value::Integer(i64::try_from(v).unwrap_or(i64::MAX)),
        );
    }
    if let Some(j) = &resp.judge {
        if j.judgment_count > 0 {
            insert_session_judge_fields(&mut root, j);
        }
    }
    root.insert("status".into(), toml::Value::String(resp.status.clone()));
    root.insert("note".into(), toml::Value::String(resp.note.clone()));
    root
}

fn insert_session_judge_fields(m: &mut toml::map::Map<String, toml::Value>, j: &SessionJudgeStats) {
    m.insert(
        "judgment_count".into(),
        toml::Value::Integer(i64::try_from(j.judgment_count).unwrap_or(i64::MAX)),
    );
    m.insert(
        "turn_judgments".into(),
        toml::Value::Integer(i64::try_from(j.turn_judgments).unwrap_or(i64::MAX)),
    );
    m.insert(
        "story_judgments".into(),
        toml::Value::Integer(i64::try_from(j.story_judgments).unwrap_or(i64::MAX)),
    );
    m.insert(
        "rubric_ids".into(),
        toml::Value::Array(
            j.rubric_ids
                .iter()
                .map(|r| toml::Value::String(r.clone()))
                .collect(),
        ),
    );
    if let Some(avg) = j.avg_score {
        m.insert("avg_score".into(), toml::Value::Float(avg));
    }
    m.insert(
        "verdict_pass".into(),
        toml::Value::Integer(i64::try_from(j.verdict_pass).unwrap_or(i64::MAX)),
    );
    m.insert(
        "verdict_partial".into(),
        toml::Value::Integer(i64::try_from(j.verdict_partial).unwrap_or(i64::MAX)),
    );
    m.insert(
        "verdict_fail".into(),
        toml::Value::Integer(i64::try_from(j.verdict_fail).unwrap_or(i64::MAX)),
    );
    m.insert(
        "manual_count".into(),
        toml::Value::Integer(i64::try_from(j.manual_count).unwrap_or(i64::MAX)),
    );
    if !j.layers_path.is_empty() {
        m.insert(
            "layers_path".into(),
            toml::Value::String(j.layers_path.clone()),
        );
    }
    m.insert("judge_status".into(), toml::Value::String(j.status.clone()));
}

pub fn print_trajectory_stats_as_json(resp: &TrajectoryStatsResponse) -> Result<()> {
    let body = serde_json::to_string_pretty(resp).context("encode stats JSON")?;
    println!("{body}");
    Ok(())
}

pub fn print_trajectory_stats_list_as_json(
    storage: &str,
    sessions: &[TrajectoryStatsResponse],
    judge: Option<&TrajectoryJudgeStatsResponse>,
) -> Result<()> {
    let mut root = serde_json::Map::new();
    root.insert("storage".into(), storage.into());
    root.insert(
        "session_count".into(),
        serde_json::Value::Number(sessions.len().into()),
    );
    if let Some(j) = judge {
        insert_judge_aggregate_json(&mut root, j);
        if !j.rubrics.is_empty() {
            root.insert(
                "rubrics".into(),
                serde_json::to_value(&j.rubrics).context("encode rubrics JSON")?,
            );
        }
    }
    let slim: Vec<serde_json::Value> = sessions
        .iter()
        .map(|s| {
            let mut m = stats_response_to_map(s);
            m.remove("storage");
            serde_json::Value::Object(
                m.into_iter()
                    .map(|(k, v)| (k, toml_value_to_json(v)))
                    .collect(),
            )
        })
        .collect();
    root.insert("sessions".into(), serde_json::Value::Array(slim));
    let body = serde_json::to_string_pretty(&serde_json::Value::Object(root))
        .context("encode stats list JSON")?;
    println!("{body}");
    Ok(())
}

fn insert_judge_aggregate_json(
    root: &mut serde_json::Map<String, serde_json::Value>,
    j: &TrajectoryJudgeStatsResponse,
) {
    if j.judgment_count == 0 {
        return;
    }
    root.insert(
        "judged_session_count".into(),
        serde_json::json!(j.judged_session_count),
    );
    root.insert("judgment_count".into(), serde_json::json!(j.judgment_count));
    root.insert("rubric_count".into(), serde_json::json!(j.rubric_count));
    root.insert("judge_status".into(), serde_json::json!(j.status));
}

fn toml_value_to_json(v: toml::Value) -> serde_json::Value {
    match v {
        toml::Value::String(s) => s.into(),
        toml::Value::Integer(n) => n.into(),
        toml::Value::Float(f) => serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        toml::Value::Boolean(b) => b.into(),
        toml::Value::Array(a) => {
            serde_json::Value::Array(a.into_iter().map(toml_value_to_json).collect())
        }
        toml::Value::Table(t) => serde_json::Value::Object(
            t.into_iter()
                .map(|(k, v)| (k, toml_value_to_json(v)))
                .collect(),
        ),
        toml::Value::Datetime(d) => d.to_string().into(),
    }
}

pub fn print_trajectory_stats_list_as_toml(
    storage: &str,
    sessions: &[TrajectoryStatsResponse],
    judge: Option<&TrajectoryJudgeStatsResponse>,
) -> Result<()> {
    let mut root = toml::map::Map::new();
    root.insert("storage".into(), toml::Value::String(storage.to_string()));
    root.insert(
        "session_count".into(),
        toml::Value::Integer(i64::try_from(sessions.len()).unwrap_or(i64::MAX)),
    );
    if let Some(j) = judge {
        insert_judge_aggregate_fields(&mut root, j);
        if j.judgment_count > 0 {
            let rubrics: Vec<toml::Value> =
                j.rubrics.iter().map(judge_rubric_summary_to_toml).collect();
            root.insert("rubrics".into(), toml::Value::Array(rubrics));
        }
    } else {
        insert_judge_aggregate_from_sessions(&mut root, sessions);
    }
    let tables = sessions
        .iter()
        .map(|s| {
            let mut m = stats_response_to_map(s);
            m.remove("storage");
            toml::Value::Table(m)
        })
        .collect();
    root.insert("sessions".into(), toml::Value::Array(tables));
    emit_root(&root, "stats list")
}

fn insert_judge_aggregate_fields(
    root: &mut toml::map::Map<String, toml::Value>,
    j: &TrajectoryJudgeStatsResponse,
) {
    if j.judgment_count == 0 {
        return;
    }
    root.insert(
        "judged_session_count".into(),
        toml::Value::Integer(i64::try_from(j.judged_session_count).unwrap_or(i64::MAX)),
    );
    root.insert(
        "judgment_count".into(),
        toml::Value::Integer(i64::try_from(j.judgment_count).unwrap_or(i64::MAX)),
    );
    root.insert(
        "rubric_count".into(),
        toml::Value::Integer(i64::try_from(j.rubric_count).unwrap_or(i64::MAX)),
    );
    if j.judgment_count > 0 {
        root.insert("judge_status".into(), toml::Value::String(j.status.clone()));
    }
}

fn insert_judge_aggregate_from_sessions(
    root: &mut toml::map::Map<String, toml::Value>,
    sessions: &[TrajectoryStatsResponse],
) {
    let mut judged_session_count = 0usize;
    let mut judgment_count = 0usize;
    let mut rubric_ids = std::collections::BTreeSet::new();
    for s in sessions {
        if let Some(j) = &s.judge {
            if j.judgment_count > 0 {
                judged_session_count += 1;
            }
            judgment_count += j.judgment_count;
            rubric_ids.extend(j.rubric_ids.iter().cloned());
        }
    }
    if judgment_count == 0 {
        return;
    }
    root.insert(
        "judged_session_count".into(),
        toml::Value::Integer(i64::try_from(judged_session_count).unwrap_or(i64::MAX)),
    );
    root.insert(
        "judgment_count".into(),
        toml::Value::Integer(i64::try_from(judgment_count).unwrap_or(i64::MAX)),
    );
    root.insert(
        "rubric_count".into(),
        toml::Value::Integer(i64::try_from(rubric_ids.len()).unwrap_or(i64::MAX)),
    );
    root.insert("judge_status".into(), toml::Value::String("ok".into()));
}

pub fn print_trajectory_truncate_as_toml(resp: &TrajectoryTruncateResponse) -> Result<()> {
    let mut root = toml::map::Map::new();
    root.insert("storage".into(), toml::Value::String(resp.storage.clone()));
    root.insert(
        "agent_id".into(),
        toml::Value::String(resp.agent_id.clone()),
    );
    root.insert(
        "session_id".into(),
        toml::Value::String(resp.session_id.clone()),
    );
    root.insert(
        "kept_rows".into(),
        toml::Value::Integer(i64::try_from(resp.kept_rows).unwrap_or(i64::MAX)),
    );
    root.insert(
        "removed_rows".into(),
        toml::Value::Integer(i64::try_from(resp.removed_rows).unwrap_or(i64::MAX)),
    );
    root.insert("status".into(), toml::Value::String(resp.status.clone()));
    root.insert("note".into(), toml::Value::String(resp.note.clone()));
    emit_root(&root, "truncate")
}

pub fn print_trajectory_extract_as_toml(resp: &TrajectoryExtractResponse) -> Result<()> {
    let mut root = toml::map::Map::new();
    root.insert("storage".into(), toml::Value::String(resp.storage.clone()));
    root.insert(
        "agent_id".into(),
        toml::Value::String(resp.agent_id.clone()),
    );
    root.insert(
        "session_id".into(),
        toml::Value::String(resp.session_id.clone()),
    );
    root.insert("out_dir".into(), toml::Value::String(resp.out_dir.clone()));
    root.insert(
        "files_copied".into(),
        toml::Value::Integer(i64::try_from(resp.files_copied).unwrap_or(i64::MAX)),
    );
    root.insert("status".into(), toml::Value::String(resp.status.clone()));
    root.insert("note".into(), toml::Value::String(resp.note.clone()));
    emit_root(&root, "extract")
}

pub fn print_trajectory_materialize_as_toml(resp: &TrajectoryMaterializeResponse) -> Result<()> {
    let mut root = toml::map::Map::new();
    root.insert("storage".into(), toml::Value::String(resp.storage.clone()));
    root.insert(
        "agent_id".into(),
        toml::Value::String(resp.agent_id.clone()),
    );
    root.insert(
        "session_id".into(),
        toml::Value::String(resp.session_id.clone()),
    );
    root.insert(
        "markdown_path".into(),
        toml::Value::String(resp.markdown_path.clone()),
    );
    root.insert(
        "event_rows".into(),
        toml::Value::Integer(i64::try_from(resp.event_rows).unwrap_or(i64::MAX)),
    );
    root.insert(
        "markdown_blocks".into(),
        toml::Value::Integer(i64::try_from(resp.markdown_blocks).unwrap_or(i64::MAX)),
    );
    root.insert(
        "skipped_events".into(),
        toml::Value::Integer(i64::try_from(resp.skipped_events).unwrap_or(i64::MAX)),
    );
    root.insert("status".into(), toml::Value::String(resp.status.clone()));
    root.insert("note".into(), toml::Value::String(resp.note.clone()));
    emit_root(&root, "materialize")
}

pub fn print_trajectory_judge_as_toml(resp: &TrajectoryJudgeResponse) -> Result<()> {
    let mut root = toml::map::Map::new();
    root.insert("storage".into(), toml::Value::String(resp.storage.clone()));
    root.insert(
        "agent_id".into(),
        toml::Value::String(resp.agent_id.clone()),
    );
    root.insert(
        "session_id".into(),
        toml::Value::String(resp.session_id.clone()),
    );
    root.insert(
        "rubric_id".into(),
        toml::Value::String(resp.rubric_id.clone()),
    );
    root.insert(
        "rubric_ids".into(),
        toml::Value::Array(
            resp.rubric_ids
                .iter()
                .map(|r| toml::Value::String(r.clone()))
                .collect(),
        ),
    );
    root.insert(
        "scope".into(),
        toml::Value::String(
            match resp.scope {
                JudgeScope::Turn => "turn",
                JudgeScope::Story => "story",
            }
            .into(),
        ),
    );
    root.insert(
        "method".into(),
        toml::Value::String(
            match resp.method {
                JudgeMethod::Llm => "llm",
                JudgeMethod::Manual => "manual",
            }
            .into(),
        ),
    );
    root.insert(
        "layer_name".into(),
        toml::Value::String(resp.layer_name.clone()),
    );
    root.insert(
        "sidecar_path".into(),
        toml::Value::String(resp.sidecar_path.clone()),
    );
    root.insert(
        "judged_calls".into(),
        toml::Value::Integer(i64::try_from(resp.judged_calls).unwrap_or(i64::MAX)),
    );
    root.insert(
        "skipped_calls".into(),
        toml::Value::Integer(i64::try_from(resp.skipped_calls).unwrap_or(i64::MAX)),
    );
    root.insert("status".into(), toml::Value::String(resp.status.clone()));
    root.insert("note".into(), toml::Value::String(resp.note.clone()));
    emit_root(&root, "judge")
}

pub fn print_trajectory_judge_stats_as_toml(resp: &TrajectoryJudgeStatsResponse) -> Result<()> {
    let mut root = toml::map::Map::new();
    root.insert("storage".into(), toml::Value::String(resp.storage.clone()));
    root.insert(
        "session_count".into(),
        toml::Value::Integer(i64::try_from(resp.session_count).unwrap_or(i64::MAX)),
    );
    root.insert(
        "judged_session_count".into(),
        toml::Value::Integer(i64::try_from(resp.judged_session_count).unwrap_or(i64::MAX)),
    );
    root.insert(
        "judgment_count".into(),
        toml::Value::Integer(i64::try_from(resp.judgment_count).unwrap_or(i64::MAX)),
    );
    root.insert(
        "rubric_count".into(),
        toml::Value::Integer(i64::try_from(resp.rubric_count).unwrap_or(i64::MAX)),
    );
    root.insert("status".into(), toml::Value::String(resp.status.clone()));
    root.insert("note".into(), toml::Value::String(resp.note.clone()));

    let sessions: Vec<toml::Value> = resp
        .sessions
        .iter()
        .map(judge_stats_session_to_toml)
        .collect();
    root.insert("sessions".into(), toml::Value::Array(sessions));

    let rubrics: Vec<toml::Value> = resp
        .rubrics
        .iter()
        .map(judge_rubric_summary_to_toml)
        .collect();
    root.insert("rubrics".into(), toml::Value::Array(rubrics));

    emit_root(&root, "judge stats")
}

fn judge_stats_session_to_toml(s: &persisting_proto::JudgeStatsSession) -> toml::Value {
    let mut m = toml::map::Map::new();
    m.insert("storage".into(), toml::Value::String(s.storage.clone()));
    m.insert("agent_id".into(), toml::Value::String(s.agent_id.clone()));
    m.insert(
        "session_id".into(),
        toml::Value::String(s.session_id.clone()),
    );
    if let Some(root) = &s.root_session_id {
        m.insert("root_session_id".into(), toml::Value::String(root.clone()));
    }
    m.insert(
        "judgment_count".into(),
        toml::Value::Integer(i64::try_from(s.judgment_count).unwrap_or(i64::MAX)),
    );
    m.insert(
        "turn_judgments".into(),
        toml::Value::Integer(i64::try_from(s.turn_judgments).unwrap_or(i64::MAX)),
    );
    m.insert(
        "story_judgments".into(),
        toml::Value::Integer(i64::try_from(s.story_judgments).unwrap_or(i64::MAX)),
    );
    m.insert(
        "rubric_ids".into(),
        toml::Value::Array(
            s.rubric_ids
                .iter()
                .map(|r| toml::Value::String(r.clone()))
                .collect(),
        ),
    );
    if let Some(avg) = s.avg_score {
        m.insert("avg_score".into(), toml::Value::Float(avg));
    }
    m.insert(
        "verdict_pass".into(),
        toml::Value::Integer(i64::try_from(s.verdict_pass).unwrap_or(i64::MAX)),
    );
    m.insert(
        "verdict_partial".into(),
        toml::Value::Integer(i64::try_from(s.verdict_partial).unwrap_or(i64::MAX)),
    );
    m.insert(
        "verdict_fail".into(),
        toml::Value::Integer(i64::try_from(s.verdict_fail).unwrap_or(i64::MAX)),
    );
    m.insert(
        "manual_count".into(),
        toml::Value::Integer(i64::try_from(s.manual_count).unwrap_or(i64::MAX)),
    );
    m.insert(
        "layers_path".into(),
        toml::Value::String(s.layers_path.clone()),
    );
    m.insert("status".into(), toml::Value::String(s.status.clone()));
    toml::Value::Table(m)
}

fn judge_rubric_summary_to_toml(r: &persisting_proto::JudgeRubricSummary) -> toml::Value {
    let mut m = toml::map::Map::new();
    m.insert("rubric_id".into(), toml::Value::String(r.rubric_id.clone()));
    m.insert(
        "judgment_count".into(),
        toml::Value::Integer(i64::try_from(r.judgment_count).unwrap_or(i64::MAX)),
    );
    m.insert("avg_score".into(), toml::Value::Float(r.avg_score));
    m.insert(
        "verdict_pass".into(),
        toml::Value::Integer(i64::try_from(r.verdict_pass).unwrap_or(i64::MAX)),
    );
    m.insert(
        "verdict_partial".into(),
        toml::Value::Integer(i64::try_from(r.verdict_partial).unwrap_or(i64::MAX)),
    );
    m.insert(
        "verdict_fail".into(),
        toml::Value::Integer(i64::try_from(r.verdict_fail).unwrap_or(i64::MAX)),
    );
    m.insert(
        "manual_count".into(),
        toml::Value::Integer(i64::try_from(r.manual_count).unwrap_or(i64::MAX)),
    );
    toml::Value::Table(m)
}

pub fn print_trajectory_replay_as_toml(resp: &TrajectoryReplayResponse) -> Result<()> {
    let mut root = toml::map::Map::new();
    root.insert("storage".into(), toml::Value::String(resp.storage.clone()));
    root.insert(
        "agent_id".into(),
        toml::Value::String(resp.agent_id.clone()),
    );
    root.insert(
        "session_id".into(),
        toml::Value::String(resp.session_id.clone()),
    );
    root.insert("status".into(), toml::Value::String(resp.status.clone()));
    root.insert("note".into(), toml::Value::String(resp.note.clone()));

    let mut tables = Vec::with_capacity(resp.records.len());
    for (i, line) in resp.records.iter().enumerate() {
        let row = parse_record_line(line).with_context(|| format!("replay records[{i}]"))?;
        tables.push(toml::Value::Table(row));
    }
    root.insert("records".into(), toml::Value::Array(tables));
    emit_root(&root, "replay")
}

fn emit_root(root: &toml::map::Map<String, toml::Value>, ctx: &str) -> Result<()> {
    let doc = toml::Value::Table(root.clone());
    println!(
        "{}",
        toml::to_string_pretty(&doc)
            .map_err(|e| anyhow::anyhow!("TOML serialize trajectory {ctx}: {e}"))?
    );
    Ok(())
}

fn parse_record_line(line: &str) -> Result<toml::map::Map<String, toml::Value>> {
    let line = line.trim();
    if line.is_empty() {
        anyhow::bail!("empty record line");
    }
    let j: Json = if line.starts_with('{') {
        serde_json::from_str(line).context("record as JSON object")?
    } else {
        anyhow::bail!("replay record must be a JSON object line");
    };
    json_object_to_toml_table(&j)
}

fn json_object_to_toml_table(v: &Json) -> Result<toml::map::Map<String, toml::Value>> {
    match v {
        Json::Object(o) => {
            let mut t = toml::map::Map::with_capacity(o.len());
            for (k, val) in o {
                t.insert(k.clone(), json_to_toml(val)?);
            }
            Ok(t)
        }
        _ => anyhow::bail!("expected JSON object at top level"),
    }
}

fn json_to_toml(v: &Json) -> Result<toml::Value> {
    Ok(match v {
        Json::Null => toml::Value::String(String::new()),
        Json::Bool(b) => toml::Value::Boolean(*b),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml::Value::Integer(i)
            } else if let Some(u) = n.as_u64() {
                if u <= i64::MAX as u64 {
                    toml::Value::Integer(u as i64)
                } else {
                    toml::Value::String(n.to_string())
                }
            } else if let Some(f) = n.as_f64() {
                toml::Value::Float(f)
            } else {
                toml::Value::String(n.to_string())
            }
        }
        Json::String(s) => toml::Value::String(s.clone()),
        Json::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for x in arr {
                out.push(json_to_toml(x)?);
            }
            toml::Value::Array(out)
        }
        Json::Object(o) => {
            let mut t = toml::map::Map::with_capacity(o.len());
            for (k, val) in o {
                t.insert(k.clone(), json_to_toml(val)?);
            }
            toml::Value::Table(t)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_toml_contains_records_array_of_tables() {
        let resp = TrajectoryReplayResponse {
            storage: "/tmp/s".into(),
            agent_id: "a".into(),
            session_id: "sess".into(),
            records: vec![
                r#"{"type":"markdown","role":"user","kind":"llm.request","content":"你好"}"#.into(),
            ],
            status: "ok".into(),
            note: "n".into(),
        };
        let mut root = toml::map::Map::new();
        root.insert("storage".into(), toml::Value::String(resp.storage.clone()));
        root.insert(
            "agent_id".into(),
            toml::Value::String(resp.agent_id.clone()),
        );
        root.insert(
            "session_id".into(),
            toml::Value::String(resp.session_id.clone()),
        );
        root.insert("status".into(), toml::Value::String(resp.status.clone()));
        root.insert("note".into(), toml::Value::String(resp.note.clone()));
        let row = parse_record_line(&resp.records[0]).unwrap();
        root.insert(
            "records".into(),
            toml::Value::Array(vec![toml::Value::Table(row)]),
        );
        let s = toml::to_string_pretty(&toml::Value::Table(root)).unwrap();
        assert!(s.contains("[[records]]"), "{s}");
        assert!(s.contains("status = \"ok\""), "{s}");
        assert!(s.contains("content = \"你好\""), "{s}");
    }

    #[test]
    fn append_and_stats_toml_status() {
        let app = TrajectoryAppendResponse {
            storage: "/s".into(),
            agent_id: "a".into(),
            session_id: "z".into(),
            accepted_records: 3,
            dataset: "/s/a/z".into(),
            status: "ok".into(),
            note: "n".into(),
        };
        let mut r = toml::map::Map::new();
        r.insert("storage".into(), toml::Value::String(app.storage.clone()));
        r.insert("agent_id".into(), toml::Value::String(app.agent_id.clone()));
        r.insert(
            "session_id".into(),
            toml::Value::String(app.session_id.clone()),
        );
        r.insert("accepted_records".into(), toml::Value::Integer(3));
        r.insert("dataset".into(), toml::Value::String(app.dataset.clone()));
        r.insert("status".into(), toml::Value::String(app.status.clone()));
        r.insert("note".into(), toml::Value::String(app.note.clone()));
        let s = toml::to_string_pretty(&toml::Value::Table(r)).unwrap();
        assert!(s.contains("accepted_records = 3"), "{s}");
        assert!(s.contains("status = \"ok\""), "{s}");

        let st = TrajectoryStatsResponse {
            storage: "/s".into(),
            agent_id: "a".into(),
            session_id: "z".into(),
            dataset: "/p".into(),
            row_count: 10,
            manifest_version: Some(7),
            judge: None,
            status: "ok".into(),
            note: "n".into(),
        };
        let mut r = toml::map::Map::new();
        r.insert("row_count".into(), toml::Value::Integer(10));
        r.insert("manifest_version".into(), toml::Value::Integer(7));
        r.insert("status".into(), toml::Value::String(st.status.clone()));
        let s = toml::to_string_pretty(&toml::Value::Table(r)).unwrap();
        assert!(s.contains("manifest_version = 7"), "{s}");
    }
}
