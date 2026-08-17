use anyhow::{Context, Result};
use persisting_pchronicle::model::{EventRecord, StorylineTurn};
use serde_json::{Map, Value};

use super::fields::role_and_body;

/// Project one capture event into the authoritative Storyline turn model.
pub fn capture_record_to_storyline_turn(rec: &EventRecord) -> Result<StorylineTurn> {
    let (role, body) = role_and_body(rec)?;
    let source = match role.as_str() {
        "user" => "user",
        "assistant" | "agent" => "agent",
        _ => "system",
    };
    let id = i64::try_from(rec.seq).context("event sequence exceeds Storyline turn id range")?;
    let metrics = rec
        .payload
        .get("body")
        .and_then(|body| body.get("usage"))
        .or_else(|| rec.payload.get("usage"))
        .cloned();
    let model_name = rec
        .payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let ttft_ms = rec.payload.get("ttft_ms").and_then(Value::as_i64);
    let latency_ms = rec.payload.get("latency_ms").and_then(Value::as_i64);

    let mut extra = Map::new();
    insert_string(&mut extra, "producer", Some(rec.source.as_str()));
    insert_string(&mut extra, "trace_id", rec.trace_id.as_deref());
    insert_string(&mut extra, "parent_uuid", rec.parent_uuid.as_deref());
    insert_string(&mut extra, "subagent_id", rec.subagent_id.as_deref());
    insert_string(
        &mut extra,
        "parent_agent_id",
        rec.parent_agent_id.as_deref(),
    );
    for key in [
        "path",
        "status",
        "draft",
        "refs_subagent_ids",
        "subagent_trajectories",
        "subagent_trajectory",
        "spawn_hints",
        "spawn_links",
    ] {
        if let Some(value) = rec.payload.get(key) {
            extra.insert(key.into(), value.clone());
        }
    }

    Ok(StorylineTurn {
        id,
        kind: Some(rec.kind.clone()),
        timestamp: rec.timestamp.clone(),
        source: source.into(),
        message: Value::String(body),
        reasoning_content: None,
        reasoning_effort: None,
        tool_calls: None,
        observation: None,
        metrics,
        model_name,
        llm_call_count: (source == "agent").then_some(1),
        is_copied_context: None,
        latency_ms,
        ttft_ms,
        extra: (!extra.is_empty()).then_some(Value::Object(extra)),
    })
}

fn insert_string(extra: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        extra.insert(key.into(), Value::String(value.into()));
    }
}
