//! Event-driven capture core: all trajectory writes flow through [`CaptureEvent`].

mod types;

pub use types::{
    CaptureEvent, CaptureInvocation, LlmRequestCaptured, LlmResponseCompleted,
    LlmResponseDraftUpdated,
};

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::debug;
use crate::dialogue::{draft_stream_assistant_block, try_capture_record_to_block};
use crate::dialogue_extract::{extract_assistant_text_from_json, extract_assistant_turn_from_sse};
use crate::markdown_trajectory::{
    session_markdown_write_path_for_key, upsert_block_by_call_id, BlockHeader,
};
use crate::session_index::SessionIndexHandle;
use crate::session_storage::trajectory_run_dir;
use crate::sink::{llm_request_summary_record, llm_response_record_with_content, CaptureSink};
use crate::subagent_link::{
    enrich_record, main_route_for_backfill, spawn_link_backfill_record, SpawnLinkBackfill,
    SubagentRegistry,
};
use crate::usage::{
    estimate_cost_usd, extract_usage_from_response, extract_usage_from_sse, StreamMetrics,
    TokenUsage,
};

/// Applies [`CaptureEvent`] to sink, session index, subagent registry, and optional live markdown.
#[derive(Clone)]
pub struct CaptureEngine {
    sink: Arc<dyn CaptureSink>,
    index: SessionIndexHandle,
    subagent_registry: Arc<Mutex<SubagentRegistry>>,
    stream_markdown: bool,
    md_lock: Arc<Mutex<()>>,
}

impl CaptureEngine {
    pub fn new(
        sink: Arc<dyn CaptureSink>,
        index: SessionIndexHandle,
        subagent_registry: Arc<Mutex<SubagentRegistry>>,
        stream_markdown: bool,
    ) -> Self {
        Self {
            sink,
            index,
            subagent_registry,
            stream_markdown,
            md_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Single entry point for all capture-side effects.
    pub fn apply(&self, inv: &CaptureInvocation, event: CaptureEvent) -> Result<()> {
        match event {
            CaptureEvent::LlmRequest(e) => self.on_llm_request(inv, e),
            CaptureEvent::LlmResponseDraftUpdated(e) => self.on_llm_response_draft(inv, e),
            CaptureEvent::LlmResponseCompleted(e) => self.on_llm_response_completed(inv, e),
        }
    }

    fn on_llm_request(&self, inv: &CaptureInvocation, event: LlmRequestCaptured) -> Result<()> {
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

        let backfills = self.enrich_and_append_request(inv, &mut rec, event.body_json.as_ref())?;
        self.sync_markdown_record(inv, &rec)?;
        self.apply_spawn_link_backfills(inv, &backfills);
        Ok(())
    }

    fn on_llm_response_draft(
        &self,
        inv: &CaptureInvocation,
        event: LlmResponseDraftUpdated,
    ) -> Result<()> {
        if !self.stream_markdown || !inv.level.includes_assistant_text() {
            return Ok(());
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
        {
            let mut guard = self
                .subagent_registry
                .lock()
                .map_err(|e| anyhow::anyhow!("subagent registry lock: {e}"))?;
            // Draft is markdown-only: enrich may register spawn hints on `rec`, but
            // `spawn_link` Lance/md backfills run on LlmResponseCompleted only.
            enrich_record(
                &mut rec,
                &inv.route,
                &inv.headers,
                None,
                Some(event.assistant_content.as_str()),
                &mut guard,
            );
        }
        rec.seq = self
            .sink
            .peek_next_seq(&inv.route)
            .context("draft markdown requires sink peek_next_seq")?;
        let Some(block) = draft_stream_assistant_block(&rec, &event.assistant_content)? else {
            return Ok(());
        };
        self.upsert_markdown_block(inv, &inv.call.call_id, block)
    }

    fn on_llm_response_completed(
        &self,
        inv: &CaptureInvocation,
        event: LlmResponseCompleted,
    ) -> Result<()> {
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
                inv.storage.as_path(),
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
            self.enrich_and_append_response(inv, &mut rec, assistant_content.as_deref())?;
        self.sync_markdown_record(inv, &rec)?;
        self.apply_spawn_link_backfills(inv, &backfills);
        Ok(())
    }

    fn markdown_path(&self, inv: &CaptureInvocation) -> PathBuf {
        let run_dir = trajectory_run_dir(inv.storage.as_path(), &inv.agent_id, &inv.route);
        session_markdown_write_path_for_key(&run_dir, &inv.route.storage_session_id)
    }

    fn sync_markdown_record(
        &self,
        inv: &CaptureInvocation,
        rec: &crate::record::CaptureRecord,
    ) -> Result<()> {
        if !self.stream_markdown {
            return Ok(());
        }
        let Some(block) = try_capture_record_to_block(rec)? else {
            return Ok(());
        };
        let call_id = rec.call_id.as_deref().unwrap_or(inv.call.call_id.as_str());
        self.upsert_markdown_block(inv, call_id, block)
    }

    fn upsert_markdown_block(
        &self,
        inv: &CaptureInvocation,
        call_id: &str,
        block: (BlockHeader, Vec<u8>),
    ) -> Result<()> {
        let _guard = self
            .md_lock
            .lock()
            .map_err(|e| anyhow::anyhow!("markdown write lock: {e}"))?;
        let path = self.markdown_path(inv);
        upsert_block_by_call_id(&path, call_id, block)
            .with_context(|| format!("markdown upsert {}", path.display()))?;
        Ok(())
    }

    fn enrich_and_append_request(
        &self,
        inv: &CaptureInvocation,
        rec: &mut crate::record::CaptureRecord,
        body_json: Option<&Value>,
    ) -> Result<Vec<SpawnLinkBackfill>> {
        let mut guard = self
            .subagent_registry
            .lock()
            .map_err(|e| anyhow::anyhow!("subagent registry lock: {e}"))?;
        let outcome = enrich_record(rec, &inv.route, &inv.headers, body_json, None, &mut guard);
        drop(guard);
        self.sink
            .append(&inv.route, &inv.agent_id, rec)
            .context("request capture append")?;
        Ok(outcome.spawn_link_backfills)
    }

    fn enrich_and_append_response(
        &self,
        inv: &CaptureInvocation,
        rec: &mut crate::record::CaptureRecord,
        assistant_text: Option<&str>,
    ) -> Result<Vec<SpawnLinkBackfill>> {
        let mut guard = self
            .subagent_registry
            .lock()
            .map_err(|e| anyhow::anyhow!("subagent registry lock: {e}"))?;
        let outcome = enrich_record(
            rec,
            &inv.route,
            &inv.headers,
            None,
            assistant_text,
            &mut guard,
        );
        drop(guard);
        self.sink
            .append(&inv.route, &inv.agent_id, rec)
            .context("response capture append")?;
        Ok(outcome.spawn_link_backfills)
    }

    fn apply_spawn_link_backfills(&self, inv: &CaptureInvocation, backfills: &[SpawnLinkBackfill]) {
        if backfills.is_empty() {
            return;
        }
        let Ok(guard) = self.subagent_registry.lock() else {
            return;
        };
        let main_route = main_route_for_backfill(&inv.route, &guard);
        drop(guard);
        for bf in backfills {
            let mut rec = spawn_link_backfill_record(&bf.parent_call_id, &bf.links, &inv.call);
            if let Err(e) = self.sink.append(&main_route, &inv.agent_id, &mut rec) {
                tracing::warn!("spawn link backfill append: {e:#}");
                continue;
            }
            if self.stream_markdown {
                let mut inv_main = inv.clone();
                inv_main.route = main_route.clone();
                if let Err(e) = self.sync_markdown_record(&inv_main, &rec) {
                    tracing::warn!("spawn link markdown: {e:#}");
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use axum::http::HeaderMap;
    use bytes::Bytes;

    use crate::capture_call::CaptureCall;
    use crate::config::CaptureLevel;
    use crate::protocol::ProtocolKind;
    use crate::provider::ProviderKind;
    use crate::record::CaptureRecord;
    use crate::session_index::SessionIndexStore;
    use crate::session_storage::CaptureRoute;

    struct RecordingSink {
        records: Mutex<Vec<CaptureRecord>>,
        next_seq: Mutex<HashMap<String, u64>>,
    }

    impl RecordingSink {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                records: Mutex::new(Vec::new()),
                next_seq: Mutex::new(HashMap::new()),
            })
        }

        fn drain(&self) -> Vec<CaptureRecord> {
            self.records.lock().unwrap().drain(..).collect()
        }
    }

    impl CaptureSink for RecordingSink {
        fn append(
            &self,
            route: &CaptureRoute,
            _agent_id: &str,
            record: &mut CaptureRecord,
        ) -> Result<()> {
            let mut guard = self.next_seq.lock().unwrap();
            let seq = guard.entry(route.seq_key()).or_insert(0);
            record.seq = *seq;
            *seq += 1;
            drop(guard);
            self.records.lock().unwrap().push(record.clone());
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

    fn test_invocation() -> CaptureInvocation {
        CaptureInvocation {
            route: CaptureRoute {
                root_session: Some("run-test".into()),
                session_id: "sess-1".into(),
                storage_session_id: "sess-1".into(),
                subagent_id: None,
            },
            agent_id: "agent-1".into(),
            call: CaptureCall {
                call_id: "call-1".into(),
                trace_id: "trace-1".into(),
                started_at: "2026-01-01T00:00:00Z".into(),
            },
            headers: HeaderMap::new(),
            level: CaptureLevel::Dialogue,
            client_model: "deepseek-chat".into(),
            upstream_model: "deepseek-chat".into(),
            provider: ProviderKind::OpenAi,
            protocol: ProtocolKind::ChatCompletions,
            storage: Arc::new(std::path::PathBuf::from("/tmp")),
            debug_on: false,
        }
    }

    #[test]
    fn request_event_appends_single_llm_request() {
        let sink = RecordingSink::new();
        let dir = tempfile::tempdir().unwrap();
        let index = SessionIndexStore::open(dir.path()).unwrap().clone_handle();
        let engine = CaptureEngine::new(
            sink.clone(),
            index,
            Arc::new(Mutex::new(SubagentRegistry::default())),
            false,
        );
        let inv = test_invocation();
        engine
            .apply(
                &inv,
                CaptureEvent::LlmRequest(LlmRequestCaptured {
                    path: "/v1/chat/completions".into(),
                    body_bytes: 12,
                    user_content: Some("hi".into()),
                    body_json: None,
                    model_rewritten: false,
                }),
            )
            .unwrap();
        let records = sink.drain();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, "llm.request");
    }

    #[test]
    fn response_event_appends_single_stream_record() {
        let sink = RecordingSink::new();
        let dir = tempfile::tempdir().unwrap();
        let index = SessionIndexStore::open(dir.path()).unwrap().clone_handle();
        let engine = CaptureEngine::new(
            sink.clone(),
            index,
            Arc::new(Mutex::new(SubagentRegistry::default())),
            false,
        );
        let inv = test_invocation();
        engine
            .apply(
                &inv,
                CaptureEvent::LlmResponseCompleted(LlmResponseCompleted {
                    status: 200,
                    resp_bytes: Bytes::from("data: [DONE]\n\n"),
                    streaming: true,
                    stream_metrics: None,
                    assistant_content: Some("hello".into()),
                }),
            )
            .unwrap();
        let records = sink.drain();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, "llm.response.stream");
        assert_eq!(
            records[0].payload["assistant_content"].as_str(),
            Some("hello")
        );
    }

    #[test]
    fn draft_event_does_not_append_to_sink() {
        let sink = RecordingSink::new();
        let dir = tempfile::tempdir().unwrap();
        let index = SessionIndexStore::open(dir.path()).unwrap().clone_handle();
        let engine = CaptureEngine::new(
            sink.clone(),
            index,
            Arc::new(Mutex::new(SubagentRegistry::default())),
            true,
        );
        let mut inv = test_invocation();
        inv.storage = Arc::new(dir.path().to_path_buf());
        engine
            .apply(
                &inv,
                CaptureEvent::LlmResponseDraftUpdated(LlmResponseDraftUpdated {
                    status: 200,
                    assistant_content: "partial".into(),
                }),
            )
            .unwrap();
        assert!(sink.drain().is_empty());
    }

    #[test]
    fn stream_markdown_keeps_user_block_when_assistant_upserts() {
        use crate::markdown_trajectory::read_blocks_from_file;
        use crate::session_storage::trajectory_run_dir;

        let sink = RecordingSink::new();
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().to_path_buf();
        let index = SessionIndexStore::open(&storage).unwrap().clone_handle();
        let engine = CaptureEngine::new(
            sink.clone(),
            index,
            Arc::new(Mutex::new(SubagentRegistry::default())),
            true,
        );
        let mut inv = test_invocation();
        inv.storage = Arc::new(storage.clone());

        engine
            .apply(
                &inv,
                CaptureEvent::LlmRequest(LlmRequestCaptured {
                    path: "/v1/chat/completions".into(),
                    body_bytes: 12,
                    user_content: Some("hello user".into()),
                    body_json: None,
                    model_rewritten: false,
                }),
            )
            .unwrap();
        engine
            .apply(
                &inv,
                CaptureEvent::LlmResponseDraftUpdated(LlmResponseDraftUpdated {
                    status: 200,
                    assistant_content: "partial assistant".into(),
                }),
            )
            .unwrap();
        engine
            .apply(
                &inv,
                CaptureEvent::LlmResponseCompleted(LlmResponseCompleted {
                    status: 200,
                    resp_bytes: Bytes::from("data: [DONE]\n\n"),
                    streaming: true,
                    stream_metrics: None,
                    assistant_content: Some("final assistant".into()),
                }),
            )
            .unwrap();

        let run_dir = trajectory_run_dir(storage.as_path(), &inv.agent_id, &inv.route);
        let md_path = session_markdown_write_path_for_key(&run_dir, &inv.route.storage_session_id);
        let blocks = read_blocks_from_file(&md_path).unwrap();
        let records = sink.drain();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].seq, 0);
        assert_eq!(records[1].seq, 1);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].role(), Some("user"));
        assert_eq!(blocks[0].value_utf8().unwrap(), "hello user");
        assert_eq!(
            blocks[0].header.fields.get("seq").and_then(|v| v.as_u64()),
            Some(0)
        );
        assert_eq!(blocks[1].role(), Some("assistant"));
        assert_eq!(blocks[1].value_utf8().unwrap(), "final assistant");
        assert_eq!(
            blocks[1].header.fields.get("seq").and_then(|v| v.as_u64()),
            Some(1)
        );
    }

    #[test]
    fn draft_markdown_uses_peeked_seq_and_matches_final() {
        use crate::markdown_trajectory::read_blocks_from_file;
        use crate::session_storage::trajectory_run_dir;

        let sink = RecordingSink::new();
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().to_path_buf();
        let index = SessionIndexStore::open(&storage).unwrap().clone_handle();
        let engine = CaptureEngine::new(
            sink.clone(),
            index,
            Arc::new(Mutex::new(SubagentRegistry::default())),
            true,
        );
        let mut inv = test_invocation();
        inv.storage = Arc::new(storage.clone());

        engine
            .apply(
                &inv,
                CaptureEvent::LlmRequest(LlmRequestCaptured {
                    path: "/v1/chat/completions".into(),
                    body_bytes: 12,
                    user_content: Some("hi".into()),
                    body_json: None,
                    model_rewritten: false,
                }),
            )
            .unwrap();
        engine
            .apply(
                &inv,
                CaptureEvent::LlmResponseDraftUpdated(LlmResponseDraftUpdated {
                    status: 200,
                    assistant_content: "wip".into(),
                }),
            )
            .unwrap();

        let run_dir = trajectory_run_dir(storage.as_path(), &inv.agent_id, &inv.route);
        let md_path = session_markdown_write_path_for_key(&run_dir, &inv.route.storage_session_id);
        let draft_blocks = read_blocks_from_file(&md_path).unwrap();
        assert_eq!(draft_blocks.len(), 2);
        assert_eq!(
            draft_blocks[1]
                .header
                .fields
                .get("seq")
                .and_then(|v| v.as_u64()),
            Some(1)
        );

        engine
            .apply(
                &inv,
                CaptureEvent::LlmResponseCompleted(LlmResponseCompleted {
                    status: 200,
                    resp_bytes: Bytes::from("data: [DONE]\n\n"),
                    streaming: true,
                    stream_metrics: None,
                    assistant_content: Some("done".into()),
                }),
            )
            .unwrap();

        let final_blocks = read_blocks_from_file(&md_path).unwrap();
        assert_eq!(
            final_blocks[1]
                .header
                .fields
                .get("seq")
                .and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(sink.drain()[1].seq, 1);
    }

    #[test]
    fn overlapping_calls_preserve_later_user_block_in_markdown() {
        use crate::markdown_trajectory::read_blocks_from_file;
        use crate::session_storage::trajectory_run_dir;

        let sink = RecordingSink::new();
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().to_path_buf();
        let index = SessionIndexStore::open(&storage).unwrap().clone_handle();
        let engine = CaptureEngine::new(
            sink.clone(),
            index,
            Arc::new(Mutex::new(SubagentRegistry::default())),
            true,
        );
        let mut inv_a = test_invocation();
        inv_a.storage = Arc::new(storage.clone());
        inv_a.call = CaptureCall {
            call_id: "call-a".into(),
            trace_id: "trace-a".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };

        engine
            .apply(
                &inv_a,
                CaptureEvent::LlmRequest(LlmRequestCaptured {
                    path: "/v1/chat/completions".into(),
                    body_bytes: 12,
                    user_content: Some("req-a".into()),
                    body_json: None,
                    model_rewritten: false,
                }),
            )
            .unwrap();
        engine
            .apply(
                &inv_a,
                CaptureEvent::LlmResponseDraftUpdated(LlmResponseDraftUpdated {
                    status: 200,
                    assistant_content: "draft-a".into(),
                }),
            )
            .unwrap();

        let mut inv_b = inv_a.clone();
        inv_b.call = CaptureCall {
            call_id: "call-b".into(),
            trace_id: "trace-b".into(),
            started_at: "2026-01-01T00:00:01Z".into(),
        };
        engine
            .apply(
                &inv_b,
                CaptureEvent::LlmRequest(LlmRequestCaptured {
                    path: "/v1/chat/completions".into(),
                    body_bytes: 12,
                    user_content: Some("req-b".into()),
                    body_json: None,
                    model_rewritten: false,
                }),
            )
            .unwrap();

        engine
            .apply(
                &inv_a,
                CaptureEvent::LlmResponseCompleted(LlmResponseCompleted {
                    status: 200,
                    resp_bytes: Bytes::from("data: [DONE]\n\n"),
                    streaming: true,
                    stream_metrics: None,
                    assistant_content: Some("final-a".into()),
                }),
            )
            .unwrap();

        let run_dir = trajectory_run_dir(storage.as_path(), &inv_a.agent_id, &inv_a.route);
        let md_path =
            session_markdown_write_path_for_key(&run_dir, &inv_a.route.storage_session_id);
        let blocks = read_blocks_from_file(&md_path).unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].value_utf8().unwrap(), "req-a");
        assert_eq!(blocks[1].value_utf8().unwrap(), "final-a");
        assert_eq!(blocks[2].value_utf8().unwrap(), "req-b");
    }
}
