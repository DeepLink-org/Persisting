//! Lance v1 event row ↔ [`CaptureRecord`].

use anyhow::{Context, Result};

use super::record::{engine_line_to_record, CaptureRecord};

/// One row in the raw Lance event log (canonical trajectory store).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanceEventRow {
    pub seq: i64,
    pub timestamp: Option<String>,
    pub kind: String,
    pub source: String,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub call_id: Option<String>,
    pub trace_id: Option<String>,
    pub parent_call_id: Option<String>,
    pub model: Option<String>,
    /// Full [`CaptureRecord`] JSON; indexed columns are denormalized for filtering.
    pub payload_json: String,
}

fn index_model(rec: &CaptureRecord) -> Option<String> {
    rec.payload
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

pub fn capture_record_to_event_row(rec: &CaptureRecord, seq: i64) -> Result<LanceEventRow> {
    Ok(LanceEventRow {
        seq,
        timestamp: rec.timestamp.clone(),
        kind: rec.kind.clone(),
        source: rec.source.clone(),
        agent_id: rec.agent_id.clone(),
        session_id: rec.session_id.clone(),
        call_id: rec.call_id.clone(),
        trace_id: rec.trace_id.clone(),
        parent_call_id: rec.parent_call_id.clone(),
        model: index_model(rec),
        payload_json: serde_json::to_string(rec).context("encode CaptureRecord JSON")?,
    })
}

pub fn engine_line_to_event_row(line: &str, seq: i64) -> Result<LanceEventRow> {
    capture_record_to_event_row(&engine_line_to_record(line)?, seq)
}

pub fn event_row_to_capture_record(row: &LanceEventRow) -> Result<CaptureRecord> {
    let mut rec: CaptureRecord =
        serde_json::from_str(&row.payload_json).context("decode CaptureRecord JSON")?;
    rec.seq = u64::try_from(row.seq).context("seq out of range for CaptureRecord")?;
    Ok(rec)
}

pub fn event_row_to_replay_json(row: &LanceEventRow) -> Result<String> {
    let rec = event_row_to_capture_record(row)?;
    serde_json::to_string(&rec).context("encode replay JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::{session_ended_record, session_started_record, CaptureMode};
    use crate::sink::{llm_request_record, llm_response_record};
    use crate::Call;

    fn test_call() -> Call {
        Call {
            call_id: "call-a".into(),
            trace_id: "trace-a".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn llm_and_lifecycle_roundtrip() {
        let req = llm_request_record(
            Some("sess".into()),
            Some("agent".into()),
            "deepseek-chat",
            "/v1/chat/completions",
            &serde_json::json!({"messages":[{"role":"user","content":"你好"}]}),
        );
        let resp = llm_response_record(
            Some("sess".into()),
            Some("agent".into()),
            200,
            &serde_json::json!({
                "choices":[{"message":{"role":"assistant","content":"你好！"}}],
            }),
            false,
            &test_call(),
        );
        let started = session_started_record(
            Some("sess".into()),
            Some("agent".into()),
            CaptureMode::Run,
            Some("127.0.0.1:8080"),
            Some("claude"),
        );

        let req_row = capture_record_to_event_row(&req, 1).unwrap();
        assert_eq!(req_row.kind, "llm.request");
        assert_eq!(req_row.model.as_deref(), Some("deepseek-chat"));
        assert!(req_row.trace_id.is_none());

        let resp_row = capture_record_to_event_row(&resp, 2).unwrap();
        assert_eq!(resp_row.trace_id.as_deref(), Some("trace-a"));
        assert_eq!(resp_row.call_id.as_deref(), Some("call-a"));

        let start_row = capture_record_to_event_row(&started, 0).unwrap();
        assert_eq!(start_row.kind, "session.started");
        assert!(start_row.timestamp.is_some());

        let ended = session_ended_record(
            Some("sess".into()),
            Some("agent".into()),
            CaptureMode::Run,
            "child_exit",
            Some(0),
            Some(1_000),
        );
        let end_row = capture_record_to_event_row(&ended, 3).unwrap();
        assert_eq!(end_row.kind, "session.ended");

        let back = event_row_to_capture_record(&resp_row).unwrap();
        assert_eq!(back.kind, "llm.response");
        assert_eq!(back.trace_id.as_deref(), Some("trace-a"));
    }
}
