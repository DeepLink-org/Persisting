//! `trajectory` 子命令成功响应在 CLI **stdout** 上以 **TOML** 打印（与默认写入格式一致）。

use anyhow::{Context, Result};
use persisting_proto::{
    TrajectoryAppendResponse, TrajectoryReplayResponse, TrajectoryStatsResponse,
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
    root.insert("status".into(), toml::Value::String(resp.status.clone()));
    root.insert("note".into(), toml::Value::String(resp.note.clone()));
    emit_root(&root, "stats")
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
