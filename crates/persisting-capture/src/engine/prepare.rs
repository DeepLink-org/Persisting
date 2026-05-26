//! Prepare phase — pure capture logic before story-local I/O.
//!
//! Runs on the runtime thread; may `ask` the run actor but never touches sink/md directly.

use anyhow::{Context, Result};
use pulsing_actor::ActorRef;
use serde_json::Value;

use super::wire::{run_enrich, StoryCommand, StoryScope};
use super::{CallContext, CancelEvent, CompleteEvent, DraftEvent, Event, RequestEvent};
use crate::debug;
use crate::dialogue::draft_stream_assistant_block;
use crate::dialogue_extract::{extract_assistant_text_from_json, extract_assistant_turn_from_sse};
use crate::sink::{llm_request_summary_record, llm_response_record_with_content, CaptureSink};
use crate::subagent_link::SpawnLinkBackfill;
use crate::usage::{
    estimate_cost_usd, extract_usage_from_response, extract_usage_from_sse, StreamMetrics,
    TokenUsage,
};

/// Index + config for the prepare phase (run actor accessed via wire client, not held here).
pub(crate) struct CapturePreparer {
    pub sink: std::sync::Arc<dyn CaptureSink>,
    pub index: crate::session_index::SessionIndexHandle,
    pub storage: std::sync::Arc<std::path::PathBuf>,
    pub stream_markdown: bool,
}

/// Outcome of prepare — at most one story command to dispatch.
pub(crate) struct PreparedCapture {
    pub ctx: CallContext,
    pub backfills: Vec<SpawnLinkBackfill>,
    story_cmd: Option<StoryCommand>,
}

impl CapturePreparer {
    pub async fn prepare(
        &self,
        run: &ActorRef,
        ctx: &CallContext,
        event: Event,
    ) -> Result<PreparedCapture> {
        match event {
            Event::Request(e) => self.prepare_request(run, ctx, e).await,
            Event::ResponseDraft(e) => self.prepare_draft(ctx, e).await,
            Event::ResponseComplete(e) => self.prepare_completed(run, ctx, e).await,
            Event::Cancelled(e) => self.prepare_cancelled(ctx, e).await,
        }
    }

    async fn prepare_request(
        &self,
        run: &ActorRef,
        ctx: &CallContext,
        event: RequestEvent,
    ) -> Result<PreparedCapture> {
        self.index.record_request(
            ctx.agent_id(),
            &ctx.route().session_id,
            ctx.provider,
            ctx.protocol.as_str(),
            &ctx.client_model,
        );
        let _ = self.index.flush();

        let forward_to = event.model_rewritten.then_some(ctx.upstream_model.as_str());
        let mut rec = llm_request_summary_record(
            Some(ctx.route().session_id.clone()),
            Some(ctx.agent_id().to_string()),
            &ctx.client_model,
            &event.path,
            event.body_bytes,
            ctx.protocol.as_str(),
            ctx.provider.as_str(),
            event.user_content,
            forward_to,
            &ctx.call,
            ctx.level,
            event.body_json.as_ref(),
        );
        let backfills = run_enrich(run, &mut rec, ctx, event.body_json.as_ref(), None).await?;
        let scope = StoryScope::from_context(ctx);
        let story_cmd = Some(StoryCommand::persist_record(
            scope,
            serde_json::to_string(&rec)?,
        ));
        Ok(PreparedCapture {
            ctx: ctx.clone(),
            backfills,
            story_cmd,
        })
    }

    async fn prepare_draft(&self, ctx: &CallContext, event: DraftEvent) -> Result<PreparedCapture> {
        if !self.stream_markdown || !ctx.level.includes_assistant_text() {
            return Ok(PreparedCapture {
                ctx: ctx.clone(),
                backfills: vec![],
                story_cmd: None,
            });
        }
        let mut rec = llm_response_record_with_content(
            Some(ctx.route().session_id.clone()),
            Some(ctx.agent_id().to_string()),
            event.status,
            &serde_json::json!({ "status": event.status, "draft": true }),
            true,
            Some(event.assistant_content.clone()),
            &ctx.call,
            ctx.level,
        );
        rec.seq = self
            .sink
            .peek_next_seq(ctx.route())
            .context("draft markdown requires sink peek_next_seq")?;
        let story_cmd = match draft_stream_assistant_block(&rec, &event.assistant_content)? {
            Some((header, body)) => Some(StoryCommand::upsert_draft(
                StoryScope::from_context(ctx),
                &ctx.call.call_id,
                header,
                body,
            )?),
            None => None,
        };
        Ok(PreparedCapture {
            ctx: ctx.clone(),
            backfills: vec![],
            story_cmd,
        })
    }

    async fn prepare_completed(
        &self,
        run: &ActorRef,
        ctx: &CallContext,
        event: CompleteEvent,
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

        let cost = estimate_cost_usd(&ctx.upstream_model, ctx.provider, &usage);
        self.index.record_response(
            ctx.agent_id(),
            &ctx.route().session_id,
            ctx.provider,
            &ctx.client_model,
            &usage,
            cost,
        );
        let _ = self.index.flush();

        if ctx.debug_on {
            debug::log_llm_response(
                self.storage.as_path(),
                &ctx.route().session_id,
                ctx.agent_id(),
                &ctx.client_model,
                event.status,
                usage.total_tokens,
                resp_text,
            );
        }

        let mut resp_payload = serde_json::json!({
            "status": event.status,
            "protocol": ctx.protocol.as_str(),
            "provider": ctx.provider.as_str(),
            "usage": usage,
            "estimated_cost_usd": cost,
        });
        if ctx.upstream_model != ctx.client_model {
            resp_payload["forward_to"] = Value::String(ctx.upstream_model.clone());
        }
        if ctx.level.includes_full_body() {
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

        let assistant_content = if ctx.level.includes_assistant_text() {
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
            Some(ctx.route().session_id.clone()),
            Some(ctx.agent_id().to_string()),
            event.status,
            &resp_payload,
            event.streaming,
            assistant_content.clone(),
            &ctx.call,
            ctx.level,
        );
        let backfills = run_enrich(run, &mut rec, ctx, None, assistant_content.as_deref()).await?;
        let scope = StoryScope::from_context(ctx);
        let story_cmd = Some(StoryCommand::persist_record(
            scope,
            serde_json::to_string(&rec)?,
        ));
        Ok(PreparedCapture {
            ctx: ctx.clone(),
            backfills,
            story_cmd,
        })
    }

    async fn prepare_cancelled(
        &self,
        ctx: &CallContext,
        event: CancelEvent,
    ) -> Result<PreparedCapture> {
        let rec = crate::record::CaptureRecord {
            seq: 0,
            source: "capture".into(),
            kind: "llm.call.cancelled".into(),
            timestamp: Some(crate::record::now_rfc3339()),
            session_id: Some(ctx.route().session_id.clone()),
            agent_id: Some(ctx.agent_id().to_string()),
            parent_uuid: None,
            trace_id: Some(ctx.call.trace_id.clone()),
            call_id: Some(ctx.call.call_id.clone()),
            subagent_id: ctx.route().subagent_id.clone(),
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({
                "status": event.status,
                "streaming": event.streaming,
                "bytes_received": event.bytes_received,
            }),
        };
        let story_cmd = Some(StoryCommand::persist_record(
            StoryScope::from_context(ctx),
            serde_json::to_string(&rec)?,
        ));
        Ok(PreparedCapture {
            ctx: ctx.clone(),
            backfills: vec![],
            story_cmd,
        })
    }
}

impl PreparedCapture {
    pub fn take_story_command(&mut self) -> Option<StoryCommand> {
        self.story_cmd.take()
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
