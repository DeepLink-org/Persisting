//! agentgateway telemetry: JSONL export (OTLP logs or Persisting envelope lines).

use std::io::BufRead;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::capture::record::{PendingRecord, SortKey};

pub fn collect_from_reader<R: BufRead>(reader: R) -> Result<Vec<PendingRecord>> {
    let mut out = Vec::new();
    for (line_no, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("gateway jsonl line {}", line_no + 1))?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .with_context(|| format!("gateway jsonl line {}", line_no + 1))?;
        out.extend(parse_value(v, line_no as u64)?);
    }
    Ok(out)
}

fn parse_value(v: Value, line_no: u64) -> Result<Vec<PendingRecord>> {
    if let Some(records) = try_persisting_envelope(&v, line_no) {
        return Ok(records);
    }
    if v.get("resourceLogs").is_some() {
        return Ok(parse_otlp_export(&v, line_no));
    }
    Ok(vec![generic_gateway_line(v, line_no)])
}

/// `{"source":"agentgateway", ...}` or already a capture-shaped object.
fn try_persisting_envelope(v: &Value, line_no: u64) -> Option<Vec<PendingRecord>> {
    let source = v.get("source").and_then(|s| s.as_str())?;
    if source != "agentgateway" && source != "gateway" {
        return None;
    }
    let kind = v
        .get("kind")
        .and_then(|k| k.as_str())
        .unwrap_or("gateway.event")
        .to_string();
    let timestamp = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .map(str::to_string);
    let ts_nanos = timestamp
        .as_deref()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|dt| dt.timestamp_nanos_opt().unwrap_or(0) as u64)
        .unwrap_or(u64::MAX);
    Some(vec![PendingRecord {
        sort_key: SortKey {
            ts_nanos,
            file_order: line_no,
            line_no: 0,
        },
        source: "agentgateway".to_string(),
        kind,
        timestamp,
        session_id: v
            .get("session_id")
            .or_else(|| v.get("sessionId"))
            .and_then(|s| s.as_str())
            .map(str::to_string),
        agent_id: v
            .get("agent_id")
            .or_else(|| v.get("agentId"))
            .and_then(|s| s.as_str())
            .map(str::to_string),
        parent_uuid: None,
        trace_id: v
            .get("trace_id")
            .or_else(|| v.get("traceId"))
            .and_then(|s| s.as_str())
            .map(str::to_string),
        payload: v.get("payload").cloned().unwrap_or_else(|| v.clone()),
    }])
}

fn parse_otlp_export(v: &Value, line_no: u64) -> Vec<PendingRecord> {
    let mut out = Vec::new();
    let Some(resource_logs) = v.get("resourceLogs").and_then(|r| r.as_array()) else {
        return out;
    };
    for rl in resource_logs {
        let scope_logs = rl
            .get("scopeLogs")
            .or_else(|| rl.get("scope_logs"))
            .and_then(|s| s.as_array());
        let Some(scope_logs) = scope_logs else {
            continue;
        };
        for sl in scope_logs {
            let log_records = sl
                .get("logRecords")
                .or_else(|| sl.get("log_records"))
                .and_then(|l| l.as_array());
            let Some(log_records) = log_records else {
                continue;
            };
            for lr in log_records {
                out.push(otlp_log_record_to_pending(lr, line_no, out.len() as u64));
            }
        }
    }
    out
}

fn otlp_log_record_to_pending(lr: &Value, file_order: u64, sub_line: u64) -> PendingRecord {
    let mut attrs = serde_json::Map::new();
    if let Some(arr) = lr
        .get("attributes")
        .or_else(|| lr.get("attribute"))
        .and_then(|a| a.as_array())
    {
        for kv in arr {
            let key = kv.get("key").and_then(|k| k.as_str()).unwrap_or("");
            let val = kv
                .get("value")
                .and_then(|v| v.get("stringValue"))
                .or_else(|| kv.get("value").and_then(|v| v.get("string_value")))
                .and_then(|s| s.as_str())
                .map(|s| Value::String(s.to_string()))
                .unwrap_or_else(|| kv.get("value").cloned().unwrap_or(Value::Null));
            if !key.is_empty() {
                attrs.insert(key.to_string(), val);
            }
        }
    }
    let body = lr
        .get("body")
        .and_then(|b| b.get("stringValue"))
        .or_else(|| lr.get("body").and_then(|b| b.get("string_value")))
        .and_then(|s| s.as_str())
        .map(str::to_string);
    if let Some(ref b) = body {
        attrs.insert("body".to_string(), Value::String(b.clone()));
    }

    let time_unix_nano = lr
        .get("timeUnixNano")
        .or_else(|| lr.get("time_unix_nano"))
        .and_then(|t| {
            t.as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| t.as_u64())
        });
    let ts_nanos = time_unix_nano.unwrap_or(u64::MAX);

    let trace_id = lr
        .get("traceId")
        .or_else(|| lr.get("trace_id"))
        .and_then(|t| t.as_str())
        .map(str::to_string);
    let session_id = attrs
        .get("session_id")
        .or_else(|| attrs.get("mcp.sessionId"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let kind =
        if attrs.contains_key("gen_ai.request.model") || attrs.contains_key("llm.requestModel") {
            "llm.request".to_string()
        } else if attrs.get("mcp.methodName").is_some() {
            "mcp.call".to_string()
        } else {
            "otel.access_log".to_string()
        };

    PendingRecord {
        sort_key: SortKey {
            ts_nanos,
            file_order,
            line_no: sub_line,
        },
        source: "agentgateway".to_string(),
        kind,
        timestamp: None,
        session_id,
        agent_id: None,
        parent_uuid: None,
        trace_id,
        payload: Value::Object(attrs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_line() {
        let v: Value = serde_json::from_str(
            r#"{"source":"agentgateway","kind":"llm.request","timestamp":"2026-05-20T12:00:00+00:00","session_id":"s1","payload":{"model":"x"}}"#,
        )
        .unwrap();
        let recs = parse_value(v, 0).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].source, "agentgateway");
        assert_eq!(recs[0].session_id.as_deref(), Some("s1"));
    }
}

fn generic_gateway_line(v: Value, line_no: u64) -> PendingRecord {
    PendingRecord {
        sort_key: SortKey {
            ts_nanos: u64::MAX,
            file_order: line_no,
            line_no: 0,
        },
        source: "agentgateway".to_string(),
        kind: "gateway.raw".to_string(),
        timestamp: None,
        session_id: None,
        agent_id: None,
        parent_uuid: None,
        trace_id: None,
        payload: v,
    }
}
