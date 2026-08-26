//! Config-free HTTP ingestion Gateway for canonical trajectory events.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use persisting_events::{EventRecord, TrajectoryAppendResponse};
use persisting_pchronicle::storage::{
    raw_event_append_queue_with_manifest_write_mode, ObjectStoreManifestWriteMode,
    RawEventAppendOutcome, RawEventAppendSender, RawEventAppendWorker, StoryCoords,
};
use serde::{Deserialize, Serialize};

use crate::gateway_partition::{GatewayPartitionRouter, GatewaySplitTemplate};

const MAX_INGEST_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_INGEST_RECORDS: usize = 256;
const USER_HEADER: &str = "x-persisting-user-id";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GatewayIngestRequest {
    agent_id: String,
    session_id: String,
    #[serde(default)]
    root_session_id: Option<String>,
    records: Vec<EventRecord>,
}

#[derive(Debug, Serialize)]
struct GatewayStatus {
    status: &'static str,
    mode: &'static str,
    dataset: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    split: Option<String>,
}

#[derive(Debug, Serialize)]
struct GatewayError {
    error: &'static str,
    message: String,
}

struct IngestState {
    partitions: GatewayPartitionRouter,
    sender: RawEventAppendSender,
}

pub(crate) struct PreparedIngestGateway {
    listener: tokio::net::TcpListener,
    endpoint: String,
    state: Arc<IngestState>,
    worker: RawEventAppendWorker,
}

impl PreparedIngestGateway {
    pub(crate) async fn bind(
        listen: std::net::SocketAddr,
        dataset_uri: String,
        split: Option<GatewaySplitTemplate>,
        manifest_write_mode: ObjectStoreManifestWriteMode,
    ) -> Result<Self> {
        anyhow::ensure!(
            listen.ip().is_loopback(),
            "pChronicle ingest Gateway may only bind to a loopback address"
        );
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .with_context(|| format!("bind pChronicle ingest Gateway to {listen}"))?;
        let endpoint = listener
            .local_addr()
            .context("read pChronicle ingest Gateway listen address")?
            .to_string();
        let (sender, worker) =
            raw_event_append_queue_with_manifest_write_mode(manifest_write_mode)?;
        Ok(Self {
            listener,
            endpoint,
            state: Arc::new(IngestState {
                partitions: GatewayPartitionRouter::new(dataset_uri, split)?,
                sender,
            }),
            worker,
        })
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(crate) fn dataset_uri(&self) -> &str {
        self.state.partitions.dataset_uri()
    }

    pub(crate) fn split_source(&self) -> Option<&str> {
        self.state.partitions.split_source()
    }

    pub(crate) async fn serve(
        self,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        let Self {
            listener,
            state,
            worker,
            ..
        } = self;
        let app = Router::new()
            .route("/healthz", get(health))
            .route("/admin/status", get(health))
            .route("/v1/events", post(append_events))
            .route("/api/public/otel/v1/traces", post(append_langfuse_otel))
            .route("/api/public/otel/v1/traces/", post(append_langfuse_otel))
            .layer(DefaultBodyLimit::max(MAX_INGEST_BODY_BYTES))
            .with_state(state);
        let result = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
            .context("serve pChronicle ingest Gateway");
        worker
            .finish()
            .context("finish pChronicle ingest Gateway writer")?;
        result
    }
}

/// Langfuse's current ingestion endpoint is OTLP/HTTP. Langfuse sends either
/// OTLP JSON or protobuf; both encodings are normalized into the same
/// loss-aware canonical event representation.
async fn append_langfuse_otel(
    State(state): State<Arc<IngestState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next());
    let is_json = content_type
        .map(|value| value.eq_ignore_ascii_case("application/json") || value.ends_with("+json"))
        .unwrap_or_else(|| body.first().is_some_and(|byte| *byte == b'{'));
    let is_protobuf = content_type.is_some_and(|value| {
        value.eq_ignore_ascii_case("application/x-protobuf")
            || value.eq_ignore_ascii_case("application/protobuf")
            || value.eq_ignore_ascii_case("application/octet-stream")
    });
    if !is_json && !is_protobuf {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(GatewayError {
                error: "unsupported_content_type",
                message: "Langfuse OTLP Gateway accepts application/json or application/x-protobuf"
                    .into(),
            }),
        )
            .into_response();
    }
    let document = if is_json {
        match serde_json::from_slice(&body) {
            Ok(document) => document,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(GatewayError {
                        error: "invalid_otlp_json",
                        message: error.to_string(),
                    }),
                )
                    .into_response();
            }
        }
    } else {
        match protobuf_otlp_to_json(&body) {
            Ok(document) => document,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(GatewayError {
                        error: "invalid_otlp_protobuf",
                        message: error,
                    }),
                )
                    .into_response();
            }
        }
    };
    let records = persisting_pchronicle::document::langfuse_otlp_json_to_events(&document);
    match append_langfuse_inner(&state, &headers, records) {
        Ok(_) => otlp_success_response(is_json),
        Err((status, code, message)) => (
            status,
            Json(GatewayError {
                error: code,
                message,
            }),
        )
            .into_response(),
    }
}

fn otlp_success_response(json_encoding: bool) -> Response {
    // OTLP requires an ExportTraceServiceResponse with partial_success unset
    // for full success. Its protobuf encoding is an empty message (zero bytes).
    if json_encoding {
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            Json(serde_json::json!({})),
        )
            .into_response();
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/x-protobuf")],
        Bytes::new(),
    )
        .into_response()
}

fn append_langfuse_inner(
    state: &IngestState,
    headers: &HeaderMap,
    records: Vec<EventRecord>,
) -> std::result::Result<usize, (StatusCode, &'static str, String)> {
    let mut groups: BTreeMap<(String, String), Vec<EventRecord>> = BTreeMap::new();
    for record in records {
        let agent_id = record.agent_id.clone().unwrap_or_else(|| "langfuse".into());
        let session_id = record
            .session_id
            .clone()
            .or_else(|| record.trace_id.clone())
            .unwrap_or_else(|| format!("batch-{}", groups.len()));
        groups
            .entry((agent_id, session_id))
            .or_default()
            .push(record);
    }
    let mut accepted = 0;
    for ((agent_id, session_id), records) in groups {
        // The canonical endpoint deliberately caps one request at 256 records;
        // an official Codex rollout can contain thousands of OTLP spans, so
        // split one Langfuse batch into durable chunks without exposing that
        // implementation limit to the OTLP client.
        for chunk in records.chunks(MAX_INGEST_RECORDS) {
            accepted += append_events_inner(
                state,
                headers,
                GatewayIngestRequest {
                    agent_id: agent_id.clone(),
                    session_id: session_id.clone(),
                    root_session_id: None,
                    records: chunk.to_vec(),
                },
            )?
            .accepted_records;
        }
    }
    Ok(accepted)
}

#[derive(Debug)]
struct ProtoReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ProtoReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn next(&mut self) -> Result<Option<(u32, u8, &'a [u8])>, String> {
        if self.offset == self.bytes.len() {
            return Ok(None);
        }
        let tag = self.varint()?;
        let number = (tag >> 3) as u32;
        let wire = (tag & 7) as u8;
        if number == 0 {
            return Err("protobuf field number is zero".into());
        }
        let value = match wire {
            0 => {
                let start = self.offset;
                self.varint()?;
                &self.bytes[start..self.offset]
            }
            1 => self.take(8)?,
            2 => {
                let length = self.varint()? as usize;
                self.take(length)?
            }
            5 => self.take(4)?,
            _ => return Err(format!("unsupported protobuf wire type {wire}")),
        };
        Ok(Some((number, wire, value)))
    }

    fn varint(&mut self) -> Result<u64, String> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let byte = *self
                .bytes
                .get(self.offset)
                .ok_or_else(|| "truncated protobuf varint".to_string())?;
            self.offset += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("protobuf varint is too long".into())
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| "protobuf length overflow".to_string())?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| "truncated protobuf field".to_string())?;
        self.offset = end;
        Ok(value)
    }
}

fn protobuf_otlp_to_json(bytes: &[u8]) -> Result<serde_json::Value, String> {
    let mut resources = Vec::new();
    let mut reader = ProtoReader::new(bytes);
    while let Some((number, wire, value)) = reader.next()? {
        if number == 1 && wire == 2 {
            resources.push(protobuf_resource_spans(value)?);
        }
    }
    Ok(serde_json::json!({ "resourceSpans": resources }))
}

fn protobuf_resource_spans(bytes: &[u8]) -> Result<serde_json::Value, String> {
    let mut resource = serde_json::json!({ "attributes": [] });
    let mut scopes = Vec::new();
    let mut reader = ProtoReader::new(bytes);
    while let Some((number, wire, value)) = reader.next()? {
        match (number, wire) {
            (1, 2) => resource = protobuf_resource(value)?,
            (2, 2) => scopes.push(protobuf_scope_spans(value)?),
            _ => {}
        }
    }
    Ok(serde_json::json!({ "resource": resource, "scopeSpans": scopes }))
}

fn protobuf_resource(bytes: &[u8]) -> Result<serde_json::Value, String> {
    let mut attributes = Vec::new();
    let mut reader = ProtoReader::new(bytes);
    while let Some((number, wire, value)) = reader.next()? {
        if number == 1 && wire == 2 {
            attributes.push(protobuf_key_value(value)?);
        }
    }
    Ok(serde_json::json!({ "attributes": attributes }))
}

fn protobuf_scope_spans(bytes: &[u8]) -> Result<serde_json::Value, String> {
    let mut spans = Vec::new();
    let mut reader = ProtoReader::new(bytes);
    while let Some((number, wire, value)) = reader.next()? {
        if number == 2 && wire == 2 {
            spans.push(protobuf_span(value)?);
        }
    }
    Ok(serde_json::json!({ "spans": spans }))
}

fn protobuf_span(bytes: &[u8]) -> Result<serde_json::Value, String> {
    let mut span = serde_json::Map::new();
    let mut attributes = Vec::new();
    let mut reader = ProtoReader::new(bytes);
    while let Some((number, wire, value)) = reader.next()? {
        match (number, wire) {
            (1, 2) => {
                span.insert("traceId".into(), serde_json::Value::String(hex(value)));
            }
            (2, 2) => {
                span.insert("spanId".into(), serde_json::Value::String(hex(value)));
            }
            (4, 2) => {
                span.insert("parentSpanId".into(), serde_json::Value::String(hex(value)));
            }
            (5, 2) => {
                span.insert("name".into(), serde_json::Value::String(text(value)?));
            }
            (7, 1) => {
                span.insert(
                    "startTimeUnixNano".into(),
                    serde_json::Value::String(
                        u64::from_le_bytes(value.try_into().unwrap()).to_string(),
                    ),
                );
            }
            (8, 1) => {
                span.insert(
                    "endTimeUnixNano".into(),
                    serde_json::Value::String(
                        u64::from_le_bytes(value.try_into().unwrap()).to_string(),
                    ),
                );
            }
            (9, 2) => attributes.push(protobuf_key_value(value)?),
            _ => {}
        }
    }
    span.insert("attributes".into(), serde_json::Value::Array(attributes));
    Ok(serde_json::Value::Object(span))
}

fn protobuf_key_value(bytes: &[u8]) -> Result<serde_json::Value, String> {
    let mut key = String::new();
    let mut value = serde_json::Value::Null;
    let mut reader = ProtoReader::new(bytes);
    while let Some((number, wire, field)) = reader.next()? {
        match (number, wire) {
            (1, 2) => key = text(field)?,
            (2, 2) => value = protobuf_any_value(field)?,
            _ => {}
        }
    }
    Ok(serde_json::json!({ "key": key, "value": value }))
}

fn protobuf_any_value(bytes: &[u8]) -> Result<serde_json::Value, String> {
    let mut reader = ProtoReader::new(bytes);
    while let Some((number, wire, value)) = reader.next()? {
        let (name, json_value) = match (number, wire) {
            (1, 2) => ("stringValue", serde_json::Value::String(text(value)?)),
            (2, 0) => (
                "boolValue",
                serde_json::Value::Bool(value.iter().any(|byte| *byte != 0)),
            ),
            (3, 0) => (
                "intValue",
                serde_json::Value::String(decode_varint(value)?.to_string()),
            ),
            (4, 1) => (
                "doubleValue",
                serde_json::json!(f64::from_le_bytes(value.try_into().unwrap())),
            ),
            (7, 2) => ("bytesValue", serde_json::Value::String(hex(value))),
            _ => continue,
        };
        return Ok(serde_json::json!({ name: json_value }));
    }
    Ok(serde_json::json!({ "stringValue": "" }))
}

fn decode_varint(bytes: &[u8]) -> Result<u64, String> {
    let mut reader = ProtoReader::new(bytes);
    reader.varint()
}

fn text(bytes: &[u8]) -> Result<String, String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| "protobuf string is not valid UTF-8".into())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn health(State(state): State<Arc<IngestState>>) -> Json<GatewayStatus> {
    Json(GatewayStatus {
        status: "ok",
        mode: "ingest",
        dataset: state.partitions.dataset_uri().to_string(),
        split: state.partitions.split_source().map(str::to_string),
    })
}

async fn append_events(
    State(state): State<Arc<IngestState>>,
    headers: HeaderMap,
    Json(request): Json<GatewayIngestRequest>,
) -> Response {
    match append_events_inner(&state, &headers, request) {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err((status, code, message)) => (
            status,
            Json(GatewayError {
                error: code,
                message,
            }),
        )
            .into_response(),
    }
}

fn append_events_inner(
    state: &IngestState,
    headers: &HeaderMap,
    request: GatewayIngestRequest,
) -> std::result::Result<TrajectoryAppendResponse, (StatusCode, &'static str, String)> {
    let agent_id = validate_identity("agent_id", request.agent_id)?;
    let session_id = validate_identity("session_id", request.session_id)?;
    let root_session_id = request
        .root_session_id
        .map(|value| validate_identity("root_session_id", value))
        .transpose()?;
    if request.records.len() > MAX_INGEST_RECORDS {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_many_records",
            format!("one ingest request may contain at most {MAX_INGEST_RECORDS} records"),
        ));
    }
    for record in &request.records {
        record
            .validate()
            .map_err(|error| (StatusCode::BAD_REQUEST, "invalid_event", error.to_string()))?;
    }

    let route_key = format!(
        "{agent_id}|{}",
        root_session_id.as_deref().unwrap_or(&session_id)
    );
    let user = headers
        .get(USER_HEADER)
        .and_then(|value| value.to_str().ok());
    let storage = state.partitions.route(&route_key, user);
    let coords = StoryCoords::new(
        storage.clone(),
        agent_id.clone(),
        session_id.clone(),
        root_session_id,
    );
    let accepted_records = request.records.len();
    let outcome = state
        .sender
        .append_durable_batch(
            request
                .records
                .into_iter()
                .map(|record| (coords.clone(), record))
                .collect(),
        )
        .map_err(|error| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "append_failed",
                format!("append canonical event batch: {error:#}"),
            )
        })?;
    match outcome {
        RawEventAppendOutcome::Accepted => {}
        RawEventAppendOutcome::Full => {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                "capacity_exhausted",
                "pChronicle append capacity is exhausted".into(),
            ));
        }
        RawEventAppendOutcome::Unavailable => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "pChronicle append service is unavailable".into(),
            ));
        }
    }
    Ok(TrajectoryAppendResponse {
        storage: storage.clone(),
        agent_id,
        session_id,
        accepted_records,
        dataset: storage,
        status: "ok".into(),
        note: "canonical events are durably visible".into(),
    })
}

fn validate_identity(
    field: &'static str,
    value: String,
) -> std::result::Result<String, (StatusCode, &'static str, String)> {
    let value = value.trim();
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "invalid_identity",
            format!("{field} must be one non-empty path-safe segment"),
        ));
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use persisting_events::EventIdentity;

    fn record() -> EventRecord {
        EventRecord {
            identity: EventIdentity::default(),
            seq: 0,
            source: "test".into(),
            kind: "llm.request".into(),
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
            payload: serde_json::Value::Null,
        }
    }

    #[test]
    fn append_routes_to_user_partition_and_is_durable() {
        let temporary = tempfile::tempdir().unwrap();
        let (sender, worker) = raw_event_append_queue_with_manifest_write_mode(
            ObjectStoreManifestWriteMode::Conditional,
        )
        .unwrap();
        let state = IngestState {
            partitions: GatewayPartitionRouter::new(
                temporary.path().to_string_lossy(),
                Some(GatewaySplitTemplate::parse("{user}").unwrap()),
            )
            .unwrap(),
            sender,
        };
        let mut headers = HeaderMap::new();
        headers.insert(USER_HEADER, HeaderValue::from_static("alice"));
        let response = append_events_inner(
            &state,
            &headers,
            GatewayIngestRequest {
                agent_id: "agent".into(),
                session_id: "session".into(),
                root_session_id: None,
                records: vec![record()],
            },
        )
        .unwrap();
        assert!(response.storage.ends_with("/alice"));
        assert_eq!(response.accepted_records, 1);
        drop(state);
        worker.finish().unwrap();
    }
}
