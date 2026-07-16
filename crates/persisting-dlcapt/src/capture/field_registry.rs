use crate::capture::event::{CaptureEvent, FieldSink};
use crate::capture::step_record::StepRecord;
use crate::config::{ExportConfig, ExportDefaults};
use crate::dialogue::InferenceEndpoint;
use chrono::Utc;
use serde_json::{Value, json};

pub struct FieldRegistry {
    defaults: ExportDefaults,
    max_steps_per_session: u64,
}

impl FieldRegistry {
    pub fn from_export(export: &ExportConfig) -> Self {
        Self {
            defaults: export.defaults.clone(),
            max_steps_per_session: export.max_steps_per_session,
        }
    }
}

pub fn materialize_session_step(
    event: &CaptureEvent,
    registry: &FieldRegistry,
    session_metadata: &serde_json::Map<String, Value>,
    run_bucket: &str,
) -> StepRecord {
    let defaults = &registry.defaults;
    let step_id = event.step_id as i64;

    let mut extensions = serde_json::Map::new();
    for (key, value) in session_metadata {
        extensions.insert(key.clone(), value.clone());
    }
    for (key, value) in &event.metadata {
        extensions.insert(key.clone(), value.clone());
    }
    for (key, patch) in &event.field_patches {
        if patch.sink == FieldSink::Extensions {
            extensions.insert(key.clone(), patch.value.clone());
        }
    }

    let mut capture_obj = serde_json::Map::new();
    capture_obj.insert("call_id".to_string(), json!(event.call_id));
    if let Some(reason) = &event.capture_meta.finish_reason {
        capture_obj.insert("finish_reason".to_string(), json!(reason));
    }
    if let Some(usage) = &event.capture_meta.usage {
        capture_obj.insert("usage".to_string(), usage.clone());
    }
    if let Some(kind) = &event.capture_meta.segment_kind {
        capture_obj.insert("segment_kind".to_string(), json!(kind));
    }
    for (key, patch) in &event.field_patches {
        if patch.sink == FieldSink::Capture {
            capture_obj.insert(key.clone(), patch.value.clone());
        }
    }

    let messages = normalize_messages(event.endpoint, &event.request);
    let response = build_response(event);

    let mut group_id = defaults.group_id.clone();
    let mut step_reward = defaults.step_reward;
    let mut reward = defaults.reward;
    let mut is_terminal = defaults.is_terminal;
    let mut is_trainable = defaults.is_trainable;
    let mut env_name = defaults.env_name.clone();
    let mut job_id = defaults.job_id.clone();

    for (key, patch) in &event.field_patches {
        if patch.sink != FieldSink::TopLevel {
            continue;
        }
        match key.as_str() {
            "group_id" if patch.value.is_string() => {
                group_id = patch.value.as_str().unwrap_or_default().to_string();
            }
            "step_reward" if patch.value.is_number() => {
                step_reward = patch.value.as_f64().unwrap_or(step_reward);
            }
            "reward" if patch.value.is_number() => {
                reward = patch.value.as_f64().unwrap_or(reward);
            }
            "is_terminal" if patch.value.is_boolean() => {
                is_terminal = patch.value.as_bool().unwrap_or(is_terminal);
            }
            "is_trainable" if patch.value.is_boolean() => {
                is_trainable = patch.value.as_bool().unwrap_or(is_trainable);
            }
            "env_name" if patch.value.is_string() => {
                env_name = patch.value.as_str().unwrap_or_default().to_string();
            }
            "job_id" if patch.value.is_string() => {
                job_id = patch.value.as_str().unwrap_or_default().to_string();
            }
            _ => {}
        }
    }

    let is_truncated = step_id as u64 >= registry.max_steps_per_session;
    let is_session_completed = is_terminal || is_truncated;
    let created_at = format_gateway_timestamp(event.completed_at);
    let id = format!("{job_id}:{}:{step_id}", event.session_id);

    let extensions_json = if extensions.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&Value::Object(extensions)).unwrap_or_else(|_| "{}".to_string()))
    };
    let capture_json = if capture_obj.is_empty() {
        None
    } else {
        Some(
            serde_json::to_string(&Value::Object(capture_obj)).unwrap_or_else(|_| "{}".to_string()),
        )
    };

    StepRecord {
        id,
        session_id: event.session_id.clone(),
        step_id,
        job_id,
        agent_id: event.agent_id.clone(),
        group_id,
        env_name,
        llm_model: event.model.clone(),
        step_reward,
        reward,
        is_terminal,
        is_truncated,
        is_session_completed,
        is_trainable,
        created_at,
        messages_json: serde_json::to_string(&messages).unwrap_or_else(|_| "[]".to_string()),
        response_json: serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
        env_state_json: serde_json::to_string(&defaults.env_state)
            .unwrap_or_else(|_| "{}".to_string()),
        extensions_json,
        capture_json,
        run_bucket: run_bucket.to_string(),
        call_id: event.call_id.clone(),
        source_export_id: None,
    }
}

fn normalize_messages(endpoint: InferenceEndpoint, request: &Value) -> Value {
    match endpoint {
        InferenceEndpoint::ChatCompletions => request
            .get("messages")
            .cloned()
            .unwrap_or_else(|| json!([])),
        InferenceEndpoint::Responses => {
            if let Some(input) = request.get("input") {
                json!([{"role": "user", "content": input}])
            } else {
                json!([])
            }
        }
    }
}

fn build_response(event: &CaptureEvent) -> Value {
    match event.endpoint {
        InferenceEndpoint::ChatCompletions => event
            .response_raw
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .cloned()
            .unwrap_or_else(|| {
                if let Some(text) = &event.response_text {
                    json!({"role": "assistant", "content": text})
                } else {
                    json!({"role": "assistant", "content": null})
                }
            }),
        InferenceEndpoint::Responses => {
            if let Some(text) = &event.response_text {
                json!({"role": "assistant", "content": text})
            } else {
                json!({"role": "assistant", "content": null})
            }
        }
    }
}

fn format_gateway_timestamp(dt: chrono::DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S%.6f%:z").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::event::CaptureMeta;
    use crate::config::ExportConfig;
    use std::collections::BTreeMap;

    fn sample_event() -> CaptureEvent {
        CaptureEvent {
            call_id: "call-1".to_string(),
            session_id: "sess-1".to_string(),
            agent_id: "openclaw".to_string(),
            step_id: 1,
            turn: 1,
            endpoint: InferenceEndpoint::ChatCompletions,
            request_path: "/v1/chat/completions".to_string(),
            model: "kimi".to_string(),
            request: json!({"messages": [{"role": "user", "content": "hi"}]}),
            request_headers: BTreeMap::new(),
            response_raw: json!({
                "choices": [{"message": {"role": "assistant", "content": "hello"}}]
            }),
            response_text: Some("hello".to_string()),
            stream: false,
            status_code: 200,
            completed_at: Utc::now(),
            metadata: BTreeMap::new(),
            field_patches: BTreeMap::new(),
            capture_meta: CaptureMeta {
                finish_reason: Some("stop".to_string()),
                usage: Some(json!({"total_tokens": 10})),
                segment_kind: None,
            },
            user_seq: 0,
            assistant_seq: 1,
        }
    }

    #[test]
    fn materialize_produces_composite_id() {
        let registry = FieldRegistry::from_export(&ExportConfig::default());
        let record = materialize_session_step(
            &sample_event(),
            &registry,
            &serde_json::Map::new(),
            "2026-06-16",
        );
        assert_eq!(record.id, "dlcapt:sess-1:1");
        assert_eq!(record.step_id, 1);
        assert!(record.messages_json.contains("hi"));
        assert!(record.response_json.contains("hello"));
    }
}
