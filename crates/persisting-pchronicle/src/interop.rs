//! Loss-aware interoperability views for observability and HTTP tooling.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::{EventIdentity, EventRecord};

fn wire(record: &EventRecord) -> &Value {
    record.payload.get("http").unwrap_or(&record.payload)
}

fn string(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Export request/response pairs as HAR 1.2. Streaming and provenance details
/// live under the namespaced `_pchronicle` extension.
pub fn events_to_har(records: &[EventRecord]) -> Value {
    let mut calls: BTreeMap<String, (Option<&EventRecord>, Option<&EventRecord>)> = BTreeMap::new();
    for record in records {
        let key = record
            .call_id
            .clone()
            .unwrap_or_else(|| format!("seq-{}", record.seq));
        let pair = calls.entry(key).or_default();
        if record.kind.contains("request") {
            pair.0 = Some(record);
        } else if record.kind.contains("response") {
            pair.1 = Some(record);
        }
    }
    let entries = calls
        .into_iter()
        .filter_map(|(call_id, (request, response))| {
            let request = request?;
            let req = wire(request);
            let resp = response.map(wire);
            Some(json!({
                "startedDateTime": request.timestamp,
                "time": resp.and_then(|v| v.get("duration_ms")).and_then(Value::as_f64).unwrap_or(0.0),
                "request": {
                    "method": string(req.get("method")),
                    "url": string(req.get("url").or_else(|| req.get("path"))),
                    "httpVersion": string(req.get("version")),
                    "headers": har_headers(req.get("headers")),
                    "queryString": [],
                    "postData": { "mimeType": "application/json", "text": body_text(req) },
                    "headersSize": -1,
                    "bodySize": -1
                },
                "response": {
                    "status": resp.and_then(|v| v.get("status")).and_then(Value::as_u64).unwrap_or(0),
                    "statusText": "",
                    "httpVersion": resp.map(|v| string(v.get("version"))).unwrap_or_default(),
                    "headers": resp.map(|v| har_headers(v.get("headers"))).unwrap_or_default(),
                    "content": { "size": -1, "mimeType": "application/json", "text": resp.map(body_text).unwrap_or_default() },
                    "redirectURL": "", "headersSize": -1, "bodySize": -1
                },
                "cache": {}, "timings": { "send": -1, "wait": -1, "receive": -1 },
                "_pchronicle": { "call_id": call_id, "trace_id": request.trace_id, "degraded": request.payload.get("degraded").and_then(Value::as_bool).unwrap_or(false) }
            }))
        })
        .collect::<Vec<_>>();
    json!({"log":{"version":"1.2","creator":{"name":"pChronicle","version":env!("CARGO_PKG_VERSION")},"entries":entries}})
}

fn har_headers(value: Option<&Value>) -> Vec<Value> {
    value
        .and_then(Value::as_object)
        .map(|headers| headers.iter().map(|(name, value)| json!({"name":name,"value":value.as_str().unwrap_or("<redacted>")})).collect())
        .unwrap_or_default()
}

fn body_text(value: &Value) -> String {
    value
        .get("body")
        .or_else(|| value.get("request_body"))
        .or_else(|| value.get("response_body"))
        .map(|body| {
            body.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| body.to_string())
        })
        .unwrap_or_default()
}

/// Export one OTel span per correlated call. Wire-only fields remain in a
/// pChronicle attribute so standard backends can coexist with lossless storage.
pub fn events_to_otlp_json(records: &[EventRecord]) -> Value {
    let spans = records
        .iter()
        .map(|record| {
            json!({
                "traceId": record.trace_id,
                "spanId": record.call_id,
                "name": record.kind,
                "startTimeUnixNano": "0",
                "endTimeUnixNano": "0",
                "attributes": [
                    {"key":"pchronicle.session_id","value":{"stringValue":record.session_id}},
                    {"key":"pchronicle.event_id","value":{"stringValue":record.identity.event_id}},
                    {"key":"pchronicle.seq","value":{"intValue":record.seq.to_string()}},
                    {"key":"pchronicle.payload","value":{"stringValue":record.payload.to_string()}}
                ]
            })
        })
        .collect::<Vec<_>>();
    json!({"resourceSpans":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"pchronicle"}}]},"scopeSpans":[{"scope":{"name":"persisting-pchronicle"},"spans":spans}]}]})
}

/// Import OTLP JSON spans as explicitly degraded, non-replayable events.
pub fn otlp_json_to_events(document: &Value) -> Vec<EventRecord> {
    let mut records = Vec::new();
    for resource in document
        .get("resourceSpans")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for scope in resource
            .get("scopeSpans")
            .or_else(|| resource.get("scope_spans"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            for span in scope
                .get("spans")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let attrs = otlp_attributes(span.get("attributes"));
                records.push(EventRecord {
                    identity: EventIdentity {
                        producer: Some("otel-import".into()),
                        ..EventIdentity::default()
                    },
                    seq: records.len() as u64,
                    source: "otel".into(),
                    kind: "otel.span".into(),
                    timestamp: None,
                    session_id: attrs.get("pchronicle.session_id").and_then(Value::as_str).map(str::to_string),
                    agent_id: None,
                    parent_uuid: None,
                    trace_id: span.get("traceId").or_else(|| span.get("trace_id")).and_then(Value::as_str).map(str::to_string),
                    call_id: span.get("spanId").or_else(|| span.get("span_id")).and_then(Value::as_str).map(str::to_string),
                    subagent_id: None, parent_agent_id: None, branch: None, parent_call_id: None,
                    payload: json!({"degraded":true,"replayable":false,"otel":{"name":span.get("name"),"attributes":attrs}}),
                });
            }
        }
    }
    records
}

/// Import Langfuse's OTLP/HTTP JSON representation without discarding the
/// resource and span attributes used for trace/session/user correlation.
///
/// The generic [`otlp_json_to_events`] importer intentionally marks records as
/// degraded for export tooling. Gateway ingestion uses this loss-aware view so
/// Langfuse fields remain queryable and replay consumers can inspect the exact
/// original span under `payload.otel`.
pub fn langfuse_otlp_json_to_events(document: &Value) -> Vec<EventRecord> {
    let mut records = Vec::new();
    for resource in document
        .get("resourceSpans")
        .or_else(|| document.get("resource_spans"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let resource_attrs = otlp_attributes(
            resource
                .get("resource")
                .and_then(|resource| resource.get("attributes")),
        );
        let service_name = resource_attrs
            .get("service.name")
            .and_then(Value::as_str)
            .unwrap_or("langfuse")
            .to_string();
        for scope in resource
            .get("scopeSpans")
            .or_else(|| resource.get("scope_spans"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            for span in scope
                .get("spans")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let attrs = otlp_attributes(span.get("attributes"));
                let trace_id = span
                    .get("traceId")
                    .or_else(|| span.get("trace_id"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let call_id = span
                    .get("spanId")
                    .or_else(|| span.get("span_id"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let session_id = first_string_attr(
                    &attrs,
                    &["langfuse.session.id", "session.id", "pchronicle.session_id"],
                )
                .or_else(|| trace_id.clone());
                let user_id = first_string_attr(&attrs, &["langfuse.user.id", "user.id"]);
                let parent_call_id = span
                    .get("parentSpanId")
                    .or_else(|| span.get("parent_span_id"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let event_id = match (trace_id.as_deref(), call_id.as_deref()) {
                    (Some(trace_id), Some(call_id)) => Some(format!("otel:{trace_id}:{call_id}")),
                    _ => None,
                };
                let kind = first_string_attr(&attrs, &["langfuse.observation.type"])
                    .map(|value| format!("otel.{value}"))
                    .unwrap_or_else(|| "otel.span".into());
                let model = first_string_attr(
                    &attrs,
                    &[
                        "langfuse.observation.model.name",
                        "gen_ai.request.model",
                        "gen_ai.response.model",
                    ],
                );
                let timestamp = otlp_timestamp(
                    span.get("startTimeUnixNano")
                        .or_else(|| span.get("start_time_unix_nano")),
                );
                records.push(EventRecord {
                    identity: EventIdentity {
                        event_id,
                        producer: Some("langfuse-otel".into()),
                        timestamp_unix_ms: timestamp.as_deref().and_then(timestamp_unix_ms),
                        ..EventIdentity::default()
                    },
                    seq: records.len() as u64,
                    source: "langfuse.otel".into(),
                    kind,
                    timestamp,
                    session_id,
                    agent_id: Some(service_name.clone()),
                    parent_uuid: None,
                    trace_id,
                    call_id,
                    subagent_id: None,
                    parent_agent_id: None,
                    branch: None,
                    parent_call_id,
                    payload: json!({
                        "model": model,
                        "langfuse": {
                            "user_id": user_id,
                            "attributes": attrs,
                            "resource": resource_attrs,
                        },
                        "otel": {
                            "span": span,
                            "resource": resource,
                            "replayable": false,
                        }
                    }),
                });
            }
        }
    }
    records
}

fn first_string_attr(attrs: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| attrs.get(*key).and_then(Value::as_str).map(str::to_string))
}

fn otlp_timestamp(value: Option<&Value>) -> Option<String> {
    let nanos = value.and_then(|value| {
        value
            .as_str()
            .and_then(|value| value.parse::<i128>().ok())
            .or_else(|| value.as_u64().map(i128::from))
    })?;
    let seconds = nanos.div_euclid(1_000_000_000);
    let subsec = nanos.rem_euclid(1_000_000_000) as u32;
    chrono::DateTime::<chrono::Utc>::from_timestamp(seconds as i64, subsec)
        .map(|value| value.to_rfc3339())
}

fn timestamp_unix_ms(value: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.timestamp_millis().max(0) as u64)
}

fn otlp_attributes(value: Option<&Value>) -> Value {
    let mut out = Map::new();
    for attr in value.and_then(Value::as_array).into_iter().flatten() {
        let Some(key) = attr.get("key").and_then(Value::as_str) else {
            continue;
        };
        let value = attr
            .get("value")
            .and_then(Value::as_object)
            .and_then(|v| v.values().next())
            .cloned()
            .unwrap_or(Value::Null);
        out.insert(key.to_string(), value);
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn otlp_import_is_explicitly_degraded() {
        let events = otlp_json_to_events(
            &json!({"resourceSpans":[{"scopeSpans":[{"spans":[{"traceId":"t","spanId":"s","name":"chat","attributes":[]}]}]}]}),
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["degraded"], true);
        assert_eq!(events[0].trace_id.as_deref(), Some("t"));
    }

    #[test]
    fn langfuse_import_preserves_session_user_and_attributes() {
        let events = langfuse_otlp_json_to_events(&json!({
            "resourceSpans": [{
                "resource": {"attributes": [{"key":"service.name","value":{"stringValue":"agent"}}]},
                "scopeSpans": [{"spans": [{
                    "traceId":"t", "spanId":"s", "name":"chat",
                    "startTimeUnixNano":"1700000000000000000",
                    "attributes": [
                        {"key":"langfuse.session.id","value":{"stringValue":"session"}},
                        {"key":"langfuse.user.id","value":{"stringValue":"user"}},
                        {"key":"langfuse.observation.model.name","value":{"stringValue":"gpt-test"}},
                        {"key":"langfuse.observation.type","value":{"stringValue":"generation"}}
                    ]
                }]}]
            }]
        }));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session_id.as_deref(), Some("session"));
        assert_eq!(events[0].agent_id.as_deref(), Some("agent"));
        assert_eq!(events[0].kind, "otel.generation");
        assert_eq!(events[0].identity.event_id.as_deref(), Some("otel:t:s"));
        assert_eq!(events[0].payload["model"], "gpt-test");
        assert_eq!(events[0].payload["langfuse"]["user_id"], "user");
    }
}
