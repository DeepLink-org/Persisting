//! Loss-aware interoperability views for observability and HTTP tooling.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::{EventIdentity, EventRecord, EVENT_SCHEMA_VERSION};

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
                        schema_version: EVENT_SCHEMA_VERSION,
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
}
