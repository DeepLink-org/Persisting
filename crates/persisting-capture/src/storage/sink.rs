//! Capture sink: append trajectory records per session.
//!
//! **Proxy capture path:** [`crate::engine::actors::StoryActor`] is the sole writer — it calls
//! [`CaptureSink::append`] on `PersistRecord` (and uses [`CaptureSink::peek_next_seq`] for streaming drafts).
//! [`crate::engine::prepare::CapturePreparer`] only builds records and [`StoryCommand`]s.
//! Session lifecycle records may still append via [`super::lifecycle`].

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Result;
use serde_json::Value;

use super::markdown_pipeline::stamp_request_payload;
use super::record::{now_rfc3339, CaptureRecord};
use super::session::CaptureRoute;
use crate::config::CaptureLevel;
use crate::Call;

pub trait CaptureSink: Send + Sync {
    /// Assign session-local `seq` on `record`, then persist. Mutates `record.seq` in place.
    fn append(
        &self,
        route: &CaptureRoute,
        agent_id: &str,
        record: &mut CaptureRecord,
    ) -> Result<()>;

    /// Next `seq` that [`Self::append`] would assign (does not increment).
    /// Returns `None` when the sink cannot predict seq (draft markdown preview unsupported).
    fn peek_next_seq(&self, route: &CaptureRoute) -> Option<u64> {
        let _ = route;
        None
    }
}

/// Assigns monotonic `seq` per storage target without persisting (`-f md` capture path).
pub struct SeqOnlySink {
    next_seq: Mutex<HashMap<String, u64>>,
}

impl Default for SeqOnlySink {
    fn default() -> Self {
        Self::new()
    }
}

impl SeqOnlySink {
    pub fn new() -> Self {
        Self {
            next_seq: Mutex::new(HashMap::new()),
        }
    }

    fn assign_seq(&self, route: &CaptureRoute, record: &mut CaptureRecord) {
        let mut guard = self.next_seq.lock().unwrap();
        let seq = guard.entry(route.seq_key()).or_insert(0);
        record.seq = *seq;
        *seq += 1;
        record.session_id = Some(route.session_id.clone());
        record.subagent_id = route.subagent_id.clone();
    }
}

impl CaptureSink for SeqOnlySink {
    fn append(
        &self,
        route: &CaptureRoute,
        _agent_id: &str,
        record: &mut CaptureRecord,
    ) -> Result<()> {
        self.assign_seq(route, record);
        Ok(())
    }

    fn peek_next_seq(&self, route: &CaptureRoute) -> Option<u64> {
        Some(
            self.next_seq
                .lock()
                .unwrap()
                .get(&route.seq_key())
                .copied()
                .unwrap_or(0),
        )
    }
}

/// Assigns monotonic `seq` per storage target and forwards records (RON encoding deferred to consumer).
pub struct CallbackSink {
    agent_id: String,
    next_seq: Mutex<HashMap<String, u64>>,
    #[allow(clippy::type_complexity)]
    callback: Box<dyn Fn(&CaptureRoute, &str, CaptureRecord) -> Result<()> + Send + Sync>,
}

impl CallbackSink {
    pub fn new<F>(agent_id: impl Into<String>, callback: F) -> Self
    where
        F: Fn(&CaptureRoute, &str, CaptureRecord) -> Result<()> + Send + Sync + 'static,
    {
        Self {
            agent_id: agent_id.into(),
            next_seq: Mutex::new(HashMap::new()),
            callback: Box::new(callback),
        }
    }
}

impl CaptureSink for CallbackSink {
    fn append(
        &self,
        route: &CaptureRoute,
        agent_id: &str,
        record: &mut CaptureRecord,
    ) -> Result<()> {
        let mut guard = self.next_seq.lock().unwrap();
        let seq = guard.entry(route.seq_key()).or_insert(0);
        record.seq = *seq;
        *seq += 1;
        drop(guard);
        record.session_id = Some(route.session_id.clone());
        record.subagent_id = route.subagent_id.clone();
        let aid = if agent_id.is_empty() {
            self.agent_id.as_str()
        } else {
            agent_id
        };
        (self.callback)(route, aid, record.clone())?;
        Ok(())
    }

    fn peek_next_seq(&self, route: &CaptureRoute) -> Option<u64> {
        Some(
            self.next_seq
                .lock()
                .unwrap()
                .get(&route.seq_key())
                .copied()
                .unwrap_or(0),
        )
    }
}

/// Sensitive header names (lowercase) — values replaced with `<redacted>` when recorded.
const REDACT_HEADER_NAMES: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "api-key",
    "x-goog-api-key",
];

/// Infer keep-alive / persistent connection flags from request headers + HTTP version.
pub fn infer_connection_persistent(
    headers: &[(String, String)],
    http_version: Option<&str>,
) -> (bool, Option<String>, Option<String>, Option<String>) {
    let mut connection_header = None;
    let mut keep_alive = None;
    let mut upgrade = None;
    for (name, value) in headers {
        match name.to_ascii_lowercase().as_str() {
            "connection" => connection_header = Some(value.clone()),
            "keep-alive" => keep_alive = Some(value.clone()),
            "upgrade" => upgrade = Some(value.clone()),
            _ => {}
        }
    }
    let conn_l = connection_header
        .as_deref()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let persistent = if conn_l.split(',').any(|p| p.trim() == "close") {
        false
    } else if conn_l.split(',').any(|p| p.trim() == "keep-alive") {
        true
    } else {
        !matches!(http_version.unwrap_or("HTTP/1.1"), v if v.starts_with("HTTP/1.0"))
    };
    (persistent, connection_header, keep_alive, upgrade)
}

/// Attach `connection.*` and `client.*` onto the event payload.
pub fn attach_connection_and_client(
    payload: &mut Value,
    headers: &[(String, String)],
    http_version: Option<&str>,
    client_peer: Option<&str>,
    client_meta: Option<&crate::session_client::SessionClientMeta>,
) {
    let (persistent, connection_header, keep_alive, upgrade) =
        infer_connection_persistent(headers, http_version);
    let mut connection = serde_json::Map::new();
    if let Some(v) = http_version {
        connection.insert("http_version".into(), Value::String(v.to_string()));
    }
    connection.insert("persistent".into(), Value::Bool(persistent));
    if let Some(v) = connection_header {
        connection.insert("connection_header".into(), Value::String(v));
    }
    if let Some(v) = keep_alive {
        connection.insert("keep_alive".into(), Value::String(v));
    }
    if let Some(v) = upgrade {
        connection.insert("upgrade".into(), Value::String(v));
    }
    let Some(obj) = payload.as_object_mut() else {
        return;
    };
    obj.insert("connection".into(), Value::Object(connection));

    let mut client = serde_json::Map::new();
    if let Some(peer) = client_peer {
        client.insert("peer".into(), Value::String(peer.to_string()));
        if let Some((ip, port)) = peer.rsplit_once(':') {
            client.insert("peer_ip".into(), Value::String(ip.to_string()));
            if let Ok(p) = port.parse::<u16>() {
                client.insert("peer_port".into(), Value::Number(p.into()));
            }
        }
    }
    if let Some(meta) = client_meta {
        if client.get("peer").is_none() && !meta.peer.is_empty() {
            client.insert("peer".into(), Value::String(meta.peer.clone()));
        }
        if client.get("peer_port").is_none() {
            client.insert("peer_port".into(), Value::Number(meta.peer_port.into()));
        }
        if meta.pid > 0 {
            client.insert("pid".into(), Value::Number(meta.pid.into()));
        }
        if !meta.command.is_empty() {
            client.insert("command".into(), Value::String(meta.command.clone()));
        }
        if let Some(fp) = &meta.machine_fp {
            client.insert("machine_fp".into(), Value::String(fp.clone()));
        }
    }
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("user-agent") {
            client.insert("user_agent".into(), Value::String(value.clone()));
            break;
        }
    }
    if !client.is_empty() {
        obj.insert("client".into(), Value::Object(client));
    }
}

/// Dual-write RFC-0002 `payload.http.*` request wire fields (keeps flat compat keys).
pub fn attach_http_wire_request(
    payload: &mut Value,
    method: &str,
    path: &str,
    url: Option<&str>,
    body: Option<&Value>,
    body_present: bool,
) {
    let Some(obj) = payload.as_object_mut() else {
        return;
    };
    obj.insert("method".into(), Value::String(method.to_string()));
    if let Some(u) = url {
        obj.insert("url".into(), Value::String(u.to_string()));
    }
    let http = obj
        .entry("http".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Value::Object(http_obj) = http {
        http_obj.insert("method".into(), Value::String(method.to_string()));
        http_obj.insert("path".into(), Value::String(path.to_string()));
        if let Some(u) = url {
            http_obj.insert("url".into(), Value::String(u.to_string()));
        }
        if let Some(b) = body {
            http_obj.insert("request_body".into(), b.clone());
            http_obj.insert("body_encoding".into(), Value::String("json".into()));
        }
    }
    if !body_present {
        obj.insert("degraded".into(), Value::Bool(true));
    }
}

/// Dual-write RFC-0002 `payload.http.*` response wire fields.
pub fn attach_http_wire_response(
    payload: &mut Value,
    status: u16,
    url: Option<&str>,
    body: Option<&Value>,
    body_present: bool,
    streaming: bool,
    headers_present: bool,
) {
    let Some(obj) = payload.as_object_mut() else {
        return;
    };
    if let Some(u) = url {
        obj.insert("url".into(), Value::String(u.to_string()));
    }
    let http = obj
        .entry("http".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Value::Object(http_obj) = http {
        http_obj.insert("status".into(), Value::Number(status.into()));
        if let Some(u) = url {
            http_obj.insert("url".into(), Value::String(u.to_string()));
        }
        http_obj.insert("streaming".into(), Value::Bool(streaming));
        if let Some(b) = body {
            http_obj.insert("response_body".into(), b.clone());
            let enc = if streaming { "sse-wire" } else { "json" };
            http_obj.insert("body_encoding".into(), Value::String(enc.into()));
        }
    }
    if !body_present || !headers_present {
        obj.insert("degraded".into(), Value::Bool(true));
    }
}

/// Persist HTTP headers onto an event payload (flat `headers` + nested `http.headers`).
///
/// Sensitive values are replaced with `<redacted>` and `headers_redacted` is set.
/// Empty `headers` still writes an empty object so callers can tell "recorded empty"
/// from "not recorded" only if they omit this call entirely.
pub fn attach_recorded_headers(payload: &mut Value, headers: &[(String, String)]) {
    let mut map = serde_json::Map::new();
    let mut redacted = false;
    for (name, value) in headers {
        let key = name.to_ascii_lowercase();
        let out = if REDACT_HEADER_NAMES.iter().any(|n| *n == key) {
            redacted = true;
            "<redacted>".to_string()
        } else {
            value.clone()
        };
        map.insert(key, Value::String(out));
    }
    let headers_val = Value::Object(map);
    let Some(obj) = payload.as_object_mut() else {
        return;
    };
    obj.insert("headers".into(), headers_val.clone());
    let http = obj
        .entry("http".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Value::Object(http_obj) = http {
        http_obj.insert("headers".into(), headers_val);
        if redacted {
            http_obj.insert("headers_redacted".into(), Value::Bool(true));
        }
    }
    if redacted {
        obj.insert("headers_redacted".into(), Value::Bool(true));
    }
}

fn attach_call_context(rec: &mut CaptureRecord, call: &Call) {
    rec.trace_id = Some(call.trace_id.clone());
    rec.call_id = Some(call.call_id.clone());
}

#[allow(clippy::too_many_arguments)]
pub fn llm_request_summary_record(
    session_id: Option<String>,
    agent_id: Option<String>,
    model: &str,
    path: &str,
    body_bytes: usize,
    protocol: &str,
    provider: &str,
    user_content: Option<String>,
    forward_to: Option<&str>,
    call: &Call,
    level: CaptureLevel,
    body_json: Option<&Value>,
) -> CaptureRecord {
    let mut payload = serde_json::json!({
        "model": model,
        "path": path,
        "body_bytes": body_bytes,
        "protocol": protocol,
        "provider": provider,
    });
    if level.includes_user_text() {
        if let Some(content) = user_content.filter(|s| !s.is_empty()) {
            payload["user_content"] = serde_json::Value::String(content);
        }
    }
    if let Some(fwd) = forward_to.filter(|s| !s.is_empty() && *s != model) {
        payload["forward_to"] = serde_json::Value::String(fwd.to_string());
    }
    if let Some(body) = body_json {
        stamp_request_payload(&mut payload, Some(body));
        if level.includes_full_body() {
            payload["body"] = body.clone();
        }
    }
    let mut rec = CaptureRecord {
        seq: 0,
        source: "persisting-proxy".to_string(),
        kind: "llm.request".to_string(),
        timestamp: Some(call.started_at.clone()),
        session_id,
        agent_id,
        parent_uuid: None,
        trace_id: None,
        call_id: None,
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload,
    };
    attach_call_context(&mut rec, call);
    rec
}

/// Full request body in payload — tests and fixtures only; production uses [`llm_request_summary_record`].
#[doc(hidden)]
pub fn llm_request_record(
    session_id: Option<String>,
    agent_id: Option<String>,
    model: &str,
    path: &str,
    body: &serde_json::Value,
) -> CaptureRecord {
    CaptureRecord {
        seq: 0,
        source: "persisting-proxy".to_string(),
        kind: "llm.request".to_string(),
        timestamp: Some(now_rfc3339()),
        session_id,
        agent_id,
        parent_uuid: None,
        trace_id: None,
        call_id: None,
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: serde_json::json!({
            "model": model,
            "path": path,
            "body": body,
        }),
    }
}

pub fn llm_response_record(
    session_id: Option<String>,
    agent_id: Option<String>,
    status: u16,
    body: &serde_json::Value,
    streaming: bool,
    call: &Call,
) -> CaptureRecord {
    let mut rec = CaptureRecord {
        seq: 0,
        source: "persisting-proxy".to_string(),
        kind: if streaming {
            "llm.response.stream".to_string()
        } else {
            "llm.response".to_string()
        },
        timestamp: Some(now_rfc3339()),
        session_id,
        agent_id,
        parent_uuid: None,
        trace_id: None,
        call_id: None,
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: serde_json::json!({
            "status": status,
            "body": body,
        }),
    };
    attach_call_context(&mut rec, call);
    rec
}

#[allow(clippy::too_many_arguments)]
pub fn llm_response_record_with_content(
    session_id: Option<String>,
    agent_id: Option<String>,
    status: u16,
    payload: &serde_json::Value,
    streaming: bool,
    assistant_content: Option<String>,
    call: &Call,
    level: CaptureLevel,
) -> CaptureRecord {
    let mut payload = payload.clone();
    payload["status"] = serde_json::json!(status);
    if level.includes_assistant_text() {
        if let Some(content) = assistant_content.filter(|s| !s.is_empty()) {
            payload["assistant_content"] = serde_json::Value::String(content);
        }
    }
    let kind = if streaming {
        "llm.response.stream"
    } else {
        "llm.response"
    };
    let mut rec = CaptureRecord {
        seq: 0,
        source: "persisting-proxy".to_string(),
        kind: kind.to_string(),
        timestamp: Some(now_rfc3339()),
        session_id,
        agent_id,
        parent_uuid: None,
        trace_id: None,
        call_id: None,
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload,
    };
    attach_call_context(&mut rec, call);
    rec
}

#[cfg(test)]
mod header_tests {
    use super::attach_recorded_headers;
    use serde_json::json;

    #[test]
    fn infer_connection_persistent_http11_default() {
        let (p, _, _, _) = super::infer_connection_persistent(&[], Some("HTTP/1.1"));
        assert!(p);
        let (p, _, _, _) = super::infer_connection_persistent(
            &[("Connection".into(), "close".into())],
            Some("HTTP/1.1"),
        );
        assert!(!p);
    }

    #[test]
    fn attach_connection_and_client_writes_peer() {
        let mut payload = json!({});
        super::attach_connection_and_client(
            &mut payload,
            &[("Connection".into(), "keep-alive".into())],
            Some("HTTP/1.1"),
            Some("127.0.0.1:9"),
            None,
        );
        assert_eq!(payload["connection"]["persistent"], true);
        assert_eq!(payload["client"]["peer"], "127.0.0.1:9");
        assert_eq!(payload["client"]["peer_port"], 9);
    }

    #[test]
    fn attach_http_wire_request_sets_nested_fields() {
        let mut payload = json!({"path": "/v1/chat/completions"});
        let body = json!({"messages":[{"role":"user","content":"hi"}]});
        super::attach_http_wire_request(
            &mut payload,
            "POST",
            "/v1/chat/completions",
            Some("//localhost/v1/chat/completions"),
            Some(&body),
            true,
        );
        assert_eq!(payload["method"], "POST");
        assert_eq!(payload["http"]["method"], "POST");
        assert_eq!(payload["http"]["path"], "/v1/chat/completions");
        assert_eq!(payload["http"]["url"], "//localhost/v1/chat/completions");
        assert_eq!(
            payload["http"]["request_body"]["messages"][0]["content"],
            "hi"
        );
        assert!(payload.get("degraded").is_none());
    }

    #[test]
    fn attach_http_wire_marks_degraded_without_body() {
        let mut payload = json!({});
        super::attach_http_wire_request(&mut payload, "GET", "/v1/models", None, None, false);
        assert_eq!(payload["degraded"], true);
    }

    #[test]
    fn attach_recorded_headers_redacts_authorization() {
        let mut payload = json!({"path": "/v1/chat/completions"});
        attach_recorded_headers(
            &mut payload,
            &[
                ("Content-Type".into(), "application/json".into()),
                ("Authorization".into(), "Bearer secret".into()),
            ],
        );
        assert_eq!(payload["headers"]["content-type"], "application/json");
        assert_eq!(payload["headers"]["authorization"], "<redacted>");
        assert_eq!(payload["headers_redacted"], true);
        assert_eq!(payload["http"]["headers"]["authorization"], "<redacted>");
    }
}
