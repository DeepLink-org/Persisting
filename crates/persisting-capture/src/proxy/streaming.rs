//! SSE streaming upstream response: translate, forward, and capture drafts/final.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use serde_json::Value;
use tokio_stream::wrappers::ReceiverStream;

use super::common::attach_capture_headers;
use super::http_headers::skip_response_header_when_body_changed;
use super::state::ProxyState;
use crate::conversion::{ProtocolBridge, StreamTranslator};
use crate::debug;
use crate::engine::{CallContext, CancelEvent, CaptureEngine, CompleteEvent, DraftEvent, Event};

const STREAM_DRAFT_MD_INTERVAL: Duration = Duration::from_millis(150);
/// Bounded queue between upstream reader and client SSE writer (backpressure).
const STREAM_CLIENT_QUEUE: usize = 256;

pub(super) fn request_wants_stream(body: &Bytes) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(false)
}

pub(super) fn should_stream_to_client(headers: &HeaderMap, request_body: &Bytes) -> bool {
    if request_wants_stream(request_body) {
        return true;
    }
    headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/event-stream"))
        .unwrap_or(false)
}

pub(super) async fn streaming_llm_response(
    upstream_resp: reqwest::Response,
    state: ProxyState,
    ctx: CallContext,
    bridge: ProtocolBridge,
) -> anyhow::Result<Response> {
    let status = upstream_resp.status();
    let resp_headers = upstream_resp.headers().clone();
    let translate = bridge.needs_response_translation();
    let session_id = ctx.route().session_id.clone();
    let agent_id = ctx.agent_id().to_string();
    let client_model = ctx.client_model.clone();
    let debug_on = ctx.debug_on;

    if debug_on {
        debug::log_llm_stream_start(
            state.storage.as_path(),
            &session_id,
            &agent_id,
            &client_model,
            status.as_u16(),
        );
    }

    let byte_stream = upstream_resp.bytes_stream();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, String>>(STREAM_CLIENT_QUEUE);

    let capture_engine = state.capture_engine.clone();
    // Share a single Arc<CallContext> across the streaming task and every emitted
    // draft / final / cancel event, so the per-chunk `spawn_apply` only does an
    // `Arc::clone` (refcount bump) instead of a deep `CallContext::clone` (Vec<(String,String)>
    // headers + several String fields).
    let ctx_bg: Arc<CallContext> = Arc::new(ctx.clone());
    let storage = Arc::clone(&state.storage);
    let reasoning_cache = Arc::clone(&state.reasoning_cache);

    tokio::spawn(async move {
        let mut buf = BytesMut::new();
        let mut translator = StreamTranslator::new(bridge, &ctx_bg.client_model);
        let mut last_draft_at = std::time::Instant::now();
        let mut last_draft_content = String::new();
        let mut client_disconnected = false;
        let mut stream = byte_stream;
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    buf.extend_from_slice(&chunk);
                    let out = if let Some(t) = translator.as_mut() {
                        match t.push_chunk(&chunk) {
                            Ok(s) if !s.is_empty() => Bytes::from(s),
                            Ok(_) => {
                                maybe_emit_stream_draft(
                                    &capture_engine,
                                    &ctx_bg,
                                    status.as_u16(),
                                    t,
                                    &mut last_draft_at,
                                    &mut last_draft_content,
                                );
                                continue;
                            }
                            Err(e) => {
                                tracing::warn!("stream translate: {e:#}");
                                chunk
                            }
                        }
                    } else {
                        chunk
                    };
                    if let Some(t) = translator.as_ref() {
                        maybe_emit_stream_draft(
                            &capture_engine,
                            &ctx_bg,
                            status.as_u16(),
                            t,
                            &mut last_draft_at,
                            &mut last_draft_content,
                        );
                    }
                    if tx.send(Ok(out)).await.is_err() {
                        client_disconnected = true;
                        break;
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    let _ = tx.send(Err(msg.clone()));
                    if debug_on {
                        debug::log_llm_upstream_error(
                            storage.as_path(),
                            &ctx_bg.route().session_id,
                            ctx_bg.agent_id(),
                            &ctx_bg.client_model,
                            "stream",
                            &msg,
                        );
                    }
                    break;
                }
            }
        }

        if client_disconnected {
            capture_engine.spawn_apply(
                Arc::clone(&ctx_bg),
                Event::Cancelled(CancelEvent {
                    status: status.as_u16(),
                    bytes_received: buf.len(),
                    streaming: true,
                }),
            );
            return;
        }

        let stream_metrics = translator.as_ref().map(|t| t.metrics().clone());
        if let Some(t) = translator.as_mut() {
            if let Ok(tail) = t.finish_stream() {
                let _ = tx.send(Ok(Bytes::from(tail))).await;
            }
            let (tool_ids, reasoning) = t.drain_reasoning_snapshot();
            if !reasoning.is_empty() {
                reasoning_cache.remember(&tool_ids, &reasoning);
            }
        }
        let resp_bytes = if translate {
            Bytes::from(
                translator
                    .as_ref()
                    .map(|t| t.upstream_snapshot().to_string())
                    .unwrap_or_default(),
            )
        } else {
            buf.freeze()
        };
        let stream_assistant_text = translator
            .as_ref()
            .and_then(|t| t.streaming_capture_snapshot());
        capture_engine.spawn_apply(
            ctx_bg,
            Event::ResponseComplete(CompleteEvent {
                status: status.as_u16(),
                resp_bytes,
                streaming: true,
                stream_metrics,
                assistant_content: stream_assistant_text,
            }),
        );
    });

    let body_stream = ReceiverStream::new(rx).map(|item| item.map_err(std::io::Error::other));

    let mut builder = Response::builder().status(status);
    // SSE streams already strip body-sensitive headers (`content-length` /
    // `transfer-encoding` always apply per-chunk, not per-body) but bridges that
    // rewrite the body still need a fresh `content-type` and must not forward stale
    // `content-encoding`. The shared filter handles both cases consistently with
    // the non-streaming path.
    for (name, value) in resp_headers.iter() {
        if translate && skip_response_header_when_body_changed(name.as_str()) {
            continue;
        }
        builder = builder.header(name, value);
    }
    if translate {
        builder = builder.header("content-type", "text/event-stream");
    }
    builder = attach_capture_headers(builder, &ctx.call);
    Ok(builder
        .body(Body::from_stream(body_stream))
        .map_err(|e| anyhow::anyhow!("response body: {e}"))?
        .into_response())
}

fn maybe_emit_stream_draft(
    engine: &CaptureEngine,
    ctx: &Arc<CallContext>,
    status: u16,
    translator: &StreamTranslator,
    last_draft_at: &mut std::time::Instant,
    last_draft_content: &mut String,
) {
    let Some(snapshot) = translator.streaming_capture_snapshot() else {
        return;
    };
    if snapshot == *last_draft_content {
        return;
    }
    if !last_draft_content.is_empty() && last_draft_at.elapsed() < STREAM_DRAFT_MD_INTERVAL {
        return;
    }
    // `Arc::clone` only bumps the refcount — no deep clone of headers/strings.
    engine.spawn_apply(
        Arc::clone(ctx),
        Event::ResponseDraft(DraftEvent {
            status,
            assistant_content: snapshot.clone(),
        }),
    );
    *last_draft_content = snapshot;
    *last_draft_at = std::time::Instant::now();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_request_enables_passthrough() {
        let body = Bytes::from_static(br#"{"model":"m","stream":true,"messages":[]}"#);
        assert!(should_stream_to_client(&HeaderMap::new(), &body));
    }

    #[test]
    fn sse_response_enables_passthrough() {
        let mut h = HeaderMap::new();
        h.insert("content-type", "text/event-stream".parse().unwrap());
        let body = Bytes::from_static(b"{}");
        assert!(should_stream_to_client(&h, &body));
    }
}
