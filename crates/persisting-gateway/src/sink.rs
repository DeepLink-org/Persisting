//! Gateway event sink: append captured trajectory records per session.
//!
//! **Proxy capture path:** the internal `StoryActor` is the sole writer — it calls
//! [`CaptureEventSink::append`] on `PersistRecord` (and uses [`CaptureEventSink::peek_next_seq`] for streaming drafts).
//! The internal `CapturePreparer` only builds records and story commands.
//! Session lifecycle records may still append via [`super::lifecycle`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::Value;

use super::record::{EventRecord, ensure_timestamp, now_rfc3339};
use crate::Call;
use crate::config::CaptureLevel;
use crate::projection::markdown_pipeline::stamp_request_payload;
use crate::session::storage::CaptureRoute;

pub trait CaptureEventSink: Send + Sync {
    /// Assign session-local `seq` on `record`, then persist. Mutates `record.seq` in place.
    fn append(&self, route: &CaptureRoute, agent_id: &str, record: &mut EventRecord) -> Result<()>;

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

    fn assign_seq(&self, route: &CaptureRoute, record: &mut EventRecord) {
        let mut guard = self.next_seq.lock().unwrap();
        let seq = guard.entry(route.seq_key()).or_insert(0);
        record.seq = *seq;
        *seq += 1;
        record.session_id = Some(route.session_id.clone());
        record.subagent_id = route.subagent_id.clone();
    }
}

impl CaptureEventSink for SeqOnlySink {
    fn append(
        &self,
        route: &CaptureRoute,
        _agent_id: &str,
        record: &mut EventRecord,
    ) -> Result<()> {
        ensure_timestamp(record);
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

/// Assigns monotonic `seq` per storage target and forwards typed records.
pub struct CallbackSink {
    agent_id: String,
    next_seq: Mutex<HashMap<String, Arc<Mutex<u64>>>>,
    #[allow(clippy::type_complexity)]
    callback: Box<dyn Fn(&CaptureRoute, &str, EventRecord) -> Result<()> + Send + Sync>,
}

impl CallbackSink {
    pub fn new<F>(agent_id: impl Into<String>, callback: F) -> Self
    where
        F: Fn(&CaptureRoute, &str, EventRecord) -> Result<()> + Send + Sync + 'static,
    {
        Self {
            agent_id: agent_id.into(),
            next_seq: Mutex::new(HashMap::new()),
            callback: Box::new(callback),
        }
    }
}

impl CaptureEventSink for CallbackSink {
    fn append(&self, route: &CaptureRoute, agent_id: &str, record: &mut EventRecord) -> Result<()> {
        ensure_timestamp(record);
        let sequence = {
            let mut guard = self.next_seq.lock().unwrap();
            Arc::clone(
                guard
                    .entry(route.seq_key())
                    .or_insert_with(|| Arc::new(Mutex::new(0))),
            )
        };
        // Serialize one storage target through persistence. The sequence is
        // advanced only after the callback accepts the record, so a rejected
        // append can be retried without leaving a permanent gap.
        let mut next_seq = sequence.lock().unwrap();
        record.seq = *next_seq;
        let following_seq = next_seq
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("capture sequence exhausted for {}", route.seq_key()))?;
        record.session_id = Some(route.session_id.clone());
        record.subagent_id = route.subagent_id.clone();
        let aid = if agent_id.is_empty() {
            self.agent_id.as_str()
        } else {
            agent_id
        };
        (self.callback)(route, aid, record.clone())?;
        *next_seq = following_seq;
        Ok(())
    }

    fn peek_next_seq(&self, route: &CaptureRoute) -> Option<u64> {
        let sequence = self.next_seq.lock().unwrap().get(&route.seq_key()).cloned();
        Some(sequence.map_or(0, |sequence| *sequence.lock().unwrap()))
    }
}

#[cfg(test)]
mod sequence_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn record() -> EventRecord {
        EventRecord {
            identity: Default::default(),
            seq: 99,
            source: "test".into(),
            kind: "test".into(),
            timestamp: None,
            session_id: None,
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: Value::Null,
        }
    }

    #[test]
    fn callback_rejection_does_not_advance_sequence() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let sink = CallbackSink::new("agent", {
            let attempts = Arc::clone(&attempts);
            let observed = Arc::clone(&observed);
            move |_, _, record| {
                observed.lock().unwrap().push(record.seq);
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    anyhow::bail!("reject first append");
                }
                Ok(())
            }
        });
        let route = CaptureRoute {
            root_session: Some("run".into()),
            session_id: "session".into(),
            storage_session_id: "session".into(),
            subagent_id: None,
        };

        let mut first = record();
        assert!(sink.append(&route, "agent", &mut first).is_err());
        assert_eq!(sink.peek_next_seq(&route), Some(0));

        let mut retry = record();
        sink.append(&route, "agent", &mut retry).unwrap();
        assert_eq!(retry.seq, 0);
        assert_eq!(sink.peek_next_seq(&route), Some(1));
        assert_eq!(*observed.lock().unwrap(), vec![0, 0]);
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

const REDACTED_VALUE: &str = "<redacted>";

fn is_sensitive_header_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    REDACT_HEADER_NAMES.contains(&name.as_str())
        || name.ends_with("-api-key")
        || name.ends_with("-token")
        || name.contains("secret-key")
}

fn is_sensitive_field_name(name: &str) -> bool {
    const SECRET_FIELDS: &[&str] = &[
        "apikey",
        "accesstoken",
        "refreshtoken",
        "authorization",
        "password",
        "secret",
        "clientsecret",
        "cookie",
        "setcookie",
        "token",
        "idtoken",
        "sessiontoken",
        "bearertoken",
        "secretaccesskey",
        "privatekey",
    ];
    let normalized = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect::<String>();
    SECRET_FIELDS.contains(&normalized.as_str())
}

fn is_sensitive_query_field_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("key") || is_sensitive_field_name(name)
}

/// Return a persistence-safe copy of HTTP headers without mutating the wire
/// request. Header names and duplicates are retained for replay diagnostics.
pub(crate) fn redact_sensitive_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            let value = if is_sensitive_header_name(name) {
                REDACTED_VALUE.to_string()
            } else {
                value.clone()
            };
            (name.clone(), value)
        })
        .collect()
}

/// Redact credentials carried in URL user-info or well-known query fields.
/// Invalid/relative URL syntax is retained, with its query sanitized when possible.
pub(crate) fn redact_sensitive_url(value: &str) -> String {
    let (mut parsed, scheme_relative) = if value.starts_with("//") {
        match url::Url::parse(&format!("http:{value}")) {
            Ok(url) => (Some(url), true),
            Err(_) => (None, false),
        }
    } else {
        (url::Url::parse(value).ok(), false)
    };

    let value = if let Some(url) = parsed.as_mut() {
        if !url.username().is_empty() {
            let _ = url.set_username(REDACTED_VALUE);
        }
        if url.password().is_some() {
            let _ = url.set_password(Some(REDACTED_VALUE));
        }
        let rendered = url.to_string();
        if scheme_relative {
            rendered
                .strip_prefix("http:")
                .unwrap_or(&rendered)
                .to_string()
        } else {
            rendered
        }
    } else {
        value.to_string()
    };

    let Some((prefix, query_and_fragment)) = value.split_once('?') else {
        return value;
    };
    let (query, fragment) = query_and_fragment
        .split_once('#')
        .map_or((query_and_fragment, None), |(query, fragment)| {
            (query, Some(fragment))
        });
    let pairs = url::form_urlencoded::parse(query.as_bytes()).collect::<Vec<_>>();
    if !pairs
        .iter()
        .any(|(name, _)| is_sensitive_query_field_name(name))
    {
        return value;
    }
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, field_value) in pairs {
        serializer.append_pair(
            &name,
            if is_sensitive_query_field_name(&name) {
                REDACTED_VALUE
            } else {
                &field_value
            },
        );
    }
    let mut redacted_url = format!("{prefix}?{}", serializer.finish());
    if let Some(fragment) = fragment {
        redacted_url.push('#');
        redacted_url.push_str(fragment);
    }
    redacted_url
}

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
    client_meta: Option<&crate::session::client::SessionClientMeta>,
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
    if let Some(peer) = client_peer {
        // The accepted socket's peer address is stable for the lifetime of a
        // keep-alive connection and does not expose credentials.
        connection.insert("id".into(), Value::String(format!("client:{peer}")));
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
            http_obj.insert("request_body".into(), redact_sensitive_body(b));
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
            http_obj.insert("response_body".into(), redact_sensitive_body(b));
            let enc = if streaming { "sse-wire" } else { "json" };
            http_obj.insert("body_encoding".into(), Value::String(enc.into()));
        }
    }
    if !body_present || !headers_present {
        obj.insert("degraded".into(), Value::Bool(true));
    }
}

/// Redact common credential fields recursively before a JSON body reaches the
/// canonical store. Provider-specific policies can pre-redact additional
/// fields; this is the non-disableable safety floor.
pub fn redact_sensitive_body(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let value = if is_sensitive_field_name(key) {
                        Value::String(REDACTED_VALUE.into())
                    } else {
                        redact_sensitive_body(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_sensitive_body).collect()),
        _ => value.clone(),
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
    for ((name, _), (_, value)) in headers.iter().zip(redact_sensitive_headers(headers)) {
        let key = name.to_ascii_lowercase();
        if is_sensitive_header_name(name) {
            redacted = true;
        }
        map.insert(key, Value::String(value));
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

fn attach_call_context(rec: &mut EventRecord, call: &Call) {
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
) -> EventRecord {
    let mut payload = serde_json::json!({
        "model": model,
        "path": path,
        "body_bytes": body_bytes,
        "protocol": protocol,
        "provider": provider,
    });
    if level.includes_user_text()
        && let Some(content) = user_content.filter(|s| !s.is_empty())
    {
        payload["user_content"] = serde_json::Value::String(content);
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
    let mut rec = EventRecord {
        identity: persisting_events::EventIdentity::default(),
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
) -> EventRecord {
    EventRecord {
        identity: persisting_events::EventIdentity::default(),
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
) -> EventRecord {
    let mut rec = EventRecord {
        identity: persisting_events::EventIdentity::default(),
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
) -> EventRecord {
    let mut payload = payload.clone();
    payload["status"] = serde_json::json!(status);
    if level.includes_assistant_text()
        && let Some(content) = assistant_content.filter(|s| !s.is_empty())
    {
        payload["assistant_content"] = serde_json::Value::String(content);
    }
    let kind = if streaming {
        "llm.response.stream"
    } else {
        "llm.response"
    };
    let mut rec = EventRecord {
        identity: persisting_events::EventIdentity::default(),
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
    use super::{attach_recorded_headers, redact_sensitive_headers, redact_sensitive_url};
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
    fn nested_body_credentials_are_redacted() {
        let body = serde_json::json!({
            "request": {
                "api_key": "sk-live",
                "clientSecret": "client-live",
                "session-token": "session-live",
                "safe": "x"
            }
        });
        let redacted = super::redact_sensitive_body(&body);
        assert_eq!(redacted["request"]["api_key"], "<redacted>");
        assert_eq!(redacted["request"]["clientSecret"], "<redacted>");
        assert_eq!(redacted["request"]["session-token"], "<redacted>");
        assert_eq!(redacted["request"]["safe"], "x");
    }

    #[test]
    fn persistence_header_copy_redacts_common_and_vendor_credentials() {
        let headers = vec![
            ("Authorization".into(), "Bearer live".into()),
            ("x-goog-api-key".into(), "google-live".into()),
            ("x-vendor-access-token".into(), "vendor-live".into()),
            ("x-request-id".into(), "req-1".into()),
        ];
        let redacted = redact_sensitive_headers(&headers);
        assert_eq!(redacted[0].1, "<redacted>");
        assert_eq!(redacted[1].1, "<redacted>");
        assert_eq!(redacted[2].1, "<redacted>");
        assert_eq!(redacted[3].1, "req-1");
        assert_eq!(
            headers[0].1, "Bearer live",
            "wire headers must be untouched"
        );
    }

    #[test]
    fn persisted_urls_redact_userinfo_and_sensitive_query_fields() {
        let redacted = redact_sensitive_url(
            "https://live-user:live-password@example.com/v1/models?key=live-key&alt=sse",
        );
        assert!(!redacted.contains("live-user"));
        assert!(!redacted.contains("live-password"));
        assert!(!redacted.contains("live-key"));
        assert!(redacted.contains("alt=sse"));

        assert_eq!(
            redact_sensitive_url("//example.com/v1/models?api_key=live-key&safe=kept"),
            "//example.com/v1/models?api_key=%3Credacted%3E&safe=kept"
        );
        assert_eq!(
            redact_sensitive_url("/v1/models?safe=kept"),
            "/v1/models?safe=kept"
        );
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
