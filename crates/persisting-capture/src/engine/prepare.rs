//! Prepare phase — pure capture logic before session-local I/O.
//!
//! Runs on the runtime thread; may `ask` the registry actor but never touches sink/md directly.

use anyhow::{Context, Result};
use pulsing_actor::ActorRef;
use serde_json::Value;

use super::wire::{registry_enrich, SessionCommand, SessionScope};
use super::{
    CaptureEvent, CaptureInvocation, LlmCallCancelled, LlmRequestCaptured, LlmResponseCompleted,
    LlmResponseDraftUpdated,
};
use crate::debug;
use crate::dialogue::draft_stream_assistant_block;
use crate::dialogue_extract::{extract_assistant_text_from_json, extract_assistant_turn_from_sse};
use crate::sink::{llm_request_summary_record, llm_response_record_with_content, CaptureSink};
use crate::subagent_link::SpawnLinkBackfill;
use crate::usage::{
    estimate_cost_usd, extract_usage_from_response, extract_usage_from_sse, StreamMetrics,
    TokenUsage,
};

/// Index + config for the prepare phase (registry accessed via wire client, not held here).
pub(crate) struct CapturePreparer {
    pub sink: std::sync::Arc<dyn CaptureSink>,
    pub index: crate::session_index::SessionIndexHandle,
    pub storage: std::sync::Arc<std::path::PathBuf>,
    pub stream_markdown: bool,
}

/// Outcome of prepare — at most one session command to dispatch.
pub(crate) struct PreparedCapture {
    pub inv: CaptureInvocation,
    pub backfills: Vec<SpawnLinkBackfill>,
    session_cmd: Option<SessionCommand>,
}

impl CapturePreparer {
    pub async fn prepare(
        &self,
        registry: &ActorRef,
        inv: &CaptureInvocation,
        event: CaptureEvent,
    ) -> Result<PreparedCapture> {
        match event {
            CaptureEvent::LlmRequest(e) => self.prepare_request(registry, inv, e).await,
            CaptureEvent::LlmResponseDraftUpdated(e) => self.prepare_draft(inv, e).await,
            CaptureEvent::LlmResponseCompleted(e) => self.prepare_completed(registry, inv, e).await,
            CaptureEvent::LlmCallCancelled(e) => self.prepare_cancelled(inv, e).await,
        }
    }

    async fn prepare_request(
        &self,
        registry: &ActorRef,
        inv: &CaptureInvocation,
        event: LlmRequestCaptured,
    ) -> Result<PreparedCapture> {
        self.index.record_request(
            &inv.agent_id,
            &inv.route.session_id,
            inv.provider,
            inv.protocol.as_str(),
            &inv.client_model,
        );
        let _ = self.index.flush();

        let forward_to = event.model_rewritten.then_some(inv.upstream_model.as_str());
        let mut rec = llm_request_summary_record(
            Some(inv.route.session_id.clone()),
            Some(inv.agent_id.clone()),
            &inv.client_model,
            &event.path,
            event.body_bytes,
            inv.protocol.as_str(),
            inv.provider.as_str(),
            event.user_content,
            forward_to,
            &inv.call,
            inv.level,
            event.body_json.as_ref(),
        );
        let backfills =
            registry_enrich(registry, &mut rec, inv, event.body_json.as_ref(), None).await?;
        let scope = SessionScope::from_invocation(inv);
        let session_cmd = Some(SessionCommand::persist_record(
            scope,
            serde_json::to_string(&rec)?,
        ));
        Ok(PreparedCapture {
            inv: inv.clone(),
            backfills,
            session_cmd,
        })
    }

    async fn prepare_draft(
        &self,
        inv: &CaptureInvocation,
        event: LlmResponseDraftUpdated,
    ) -> Result<PreparedCapture> {
        if !self.stream_markdown || !inv.level.includes_assistant_text() {
            return Ok(PreparedCapture {
                inv: inv.clone(),
                backfills: vec![],
                session_cmd: None,
            });
        }
        let mut rec = llm_response_record_with_content(
            Some(inv.route.session_id.clone()),
            Some(inv.agent_id.clone()),
            event.status,
            &serde_json::json!({ "status": event.status, "draft": true }),
            true,
            Some(event.assistant_content.clone()),
            &inv.call,
            inv.level,
        );
        // Draft is ephemeral markdown only — no registry enrich or spawn-link side effects.
        rec.seq = self
            .sink
            .peek_next_seq(&inv.route)
            .context("draft markdown requires sink peek_next_seq")?;
        let session_cmd = match draft_stream_assistant_block(&rec, &event.assistant_content)? {
            Some((header, body)) => Some(SessionCommand::upsert_draft(
                SessionScope::from_invocation(inv),
                &inv.call.call_id,
                header,
                body,
            )?),
            None => None,
        };
        Ok(PreparedCapture {
            inv: inv.clone(),
            backfills: vec![],
            session_cmd,
        })
    }

    async fn prepare_completed(
        &self,
        registry: &ActorRef,
        inv: &CaptureInvocation,
        event: LlmResponseCompleted,
    ) -> Result<PreparedCapture> {
        let resp_text = std::str::from_utf8(&event.resp_bytes).unwrap_or("<non-utf8>");
        let usage =
            resolve_response_usage(event.streaming, event.stream_metrics.as_ref(), resp_text);
        let resp_json = if event.streaming {
            Value::String(resp_text.to_string())
        } else {
            serde_json::from_slice(&event.resp_bytes)
                .unwrap_or_else(|_| Value::String(resp_text.to_string()))
        };

        let cost = estimate_cost_usd(&inv.upstream_model, inv.provider, &usage);
        self.index.record_response(
            &inv.agent_id,
            &inv.route.session_id,
            inv.provider,
            &inv.client_model,
            &usage,
            cost,
        );
        let _ = self.index.flush();

        if inv.debug_on {
            debug::log_llm_response(
                self.storage.as_path(),
                &inv.route.session_id,
                &inv.agent_id,
                &inv.client_model,
                event.status,
                usage.total_tokens,
                resp_text,
            );
        }

        let mut resp_payload = serde_json::json!({
            "status": event.status,
            "protocol": inv.protocol.as_str(),
            "provider": inv.provider.as_str(),
            "usage": usage,
            "estimated_cost_usd": cost,
        });
        if inv.upstream_model != inv.client_model {
            resp_payload["forward_to"] = Value::String(inv.upstream_model.clone());
        }
        if inv.level.includes_full_body() {
            resp_payload["body"] = resp_json.clone();
        }
        if let Some(m) = event.stream_metrics.as_ref() {
            if let Some(ttft) = m.ttft_ms {
                resp_payload["ttft_ms"] = Value::Number(ttft.into());
            }
            if m.usage.reasoning_tokens > 0 {
                resp_payload["reasoning_tokens"] = Value::Number(m.usage.reasoning_tokens.into());
            }
        }

        let assistant_content = if inv.level.includes_assistant_text() {
            event.assistant_content.or_else(|| {
                if event.streaming {
                    Some(extract_assistant_turn_from_sse(resp_text))
                } else {
                    extract_assistant_text_from_json(&resp_json)
                }
            })
        } else {
            None
        };

        let mut rec = llm_response_record_with_content(
            Some(inv.route.session_id.clone()),
            Some(inv.agent_id.clone()),
            event.status,
            &resp_payload,
            event.streaming,
            assistant_content.clone(),
            &inv.call,
            inv.level,
        );
        let backfills =
            registry_enrich(registry, &mut rec, inv, None, assistant_content.as_deref()).await?;
        let scope = SessionScope::from_invocation(inv);
        let session_cmd = Some(SessionCommand::persist_record(
            scope,
            serde_json::to_string(&rec)?,
        ));
        Ok(PreparedCapture {
            inv: inv.clone(),
            backfills,
            session_cmd,
        })
    }

    async fn prepare_cancelled(
        &self,
        inv: &CaptureInvocation,
        event: LlmCallCancelled,
    ) -> Result<PreparedCapture> {
        let rec = crate::record::CaptureRecord {
            seq: 0,
            source: "capture".into(),
            kind: "llm.call.cancelled".into(),
            timestamp: Some(crate::record::now_rfc3339()),
            session_id: Some(inv.route.session_id.clone()),
            agent_id: Some(inv.agent_id.clone()),
            parent_uuid: None,
            trace_id: Some(inv.call.trace_id.clone()),
            call_id: Some(inv.call.call_id.clone()),
            subagent_id: inv.route.subagent_id.clone(),
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({
                "status": event.status,
                "streaming": event.streaming,
                "bytes_received": event.bytes_received,
            }),
        };
        let session_cmd = Some(SessionCommand::persist_record(
            SessionScope::from_invocation(inv),
            serde_json::to_string(&rec)?,
        ));
        Ok(PreparedCapture {
            inv: inv.clone(),
            backfills: vec![],
            session_cmd,
        })
    }
}

impl PreparedCapture {
    pub fn take_session_command(&mut self) -> Option<SessionCommand> {
        self.session_cmd.take()
    }
}

fn resolve_response_usage(
    streaming: bool,
    stream_metrics: Option<&StreamMetrics>,
    resp_text: &str,
) -> TokenUsage {
    if streaming {
        stream_metrics
            .map(|m| m.usage.clone())
            .filter(|u| u.total_tokens > 0 || u.input_tokens > 0 || u.output_tokens > 0)
            .unwrap_or_else(|| extract_usage_from_sse(resp_text))
    } else {
        let resp_json: Value = serde_json::from_str(resp_text)
            .unwrap_or_else(|_| Value::String(resp_text.to_string()));
        extract_usage_from_response(&resp_json)
    }
}
