use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bytes::Bytes;

use crate::config::CaptureLevel;
use crate::engine::{CallContext, CaptureEngine, CompleteEvent, DraftEvent, Event, RequestEvent};
use crate::markdown_trajectory::session_markdown_write_path_for_key;
use crate::protocol::ProtocolKind;
use crate::provider::ProviderKind;
use crate::record::CaptureRecord;
use crate::session_index::SessionIndexStore;
use crate::session_storage::CaptureRoute;
use crate::sink::CaptureSink;
use crate::Call;

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
    ) -> anyhow::Result<()> {
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

fn test_context() -> CallContext {
    CallContext::new(
        CaptureRoute {
            root_session: Some("run-test".into()),
            session_id: "sess-1".into(),
            storage_session_id: "sess-1".into(),
            subagent_id: None,
        },
        "agent-1",
        Call {
            call_id: "call-1".into(),
            trace_id: "trace-1".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        },
        Vec::new(),
        CaptureLevel::Dialogue,
        "deepseek-chat",
        "deepseek-chat",
        ProviderKind::OpenAi,
        ProtocolKind::ChatCompletions,
        false,
    )
}

async fn test_engine(
    sink: Arc<RecordingSink>,
    storage: &std::path::Path,
    stream_markdown: bool,
) -> CaptureEngine {
    let index = SessionIndexStore::open(storage).unwrap().clone_handle();
    CaptureEngine::new(
        sink,
        index,
        Arc::new(storage.to_path_buf()),
        stream_markdown,
    )
    .await
    .unwrap()
}

async fn flush_engine(engine: &CaptureEngine) {
    engine.flush().await.unwrap();
}

#[tokio::test]
async fn story_snapshot_reflects_applied_turns() {
    let sink = RecordingSink::new();
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(sink.clone(), dir.path(), false).await;
    let ctx = test_context();
    engine
        .apply(
            &ctx,
            Event::Request(RequestEvent {
                path: "/v1/chat/completions".into(),
                body_bytes: 12,
                user_content: Some("hello".into()),
                body_json: None,
                model_rewritten: false,
            }),
        )
        .await
        .unwrap();
    flush_engine(&engine).await;

    let snap = engine.story_snapshot(&ctx.story).await.unwrap();
    assert_eq!(snap.story_id.as_str(), ctx.story_id().as_str());
    assert_eq!(snap.turns.len(), 1);
    assert_eq!(snap.turns[0].user.as_ref().unwrap().text, "hello");
}

#[tokio::test]
async fn shutdown_persists_story_snapshots_for_active_stories() {
    use crate::engine::load_story_snapshots;

    let sink = RecordingSink::new();
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(sink.clone(), dir.path(), false).await;
    let ctx = test_context();
    engine
        .apply(
            &ctx,
            Event::Request(RequestEvent {
                path: "/v1/chat/completions".into(),
                body_bytes: 12,
                user_content: Some("hello".into()),
                body_json: None,
                model_rewritten: false,
            }),
        )
        .await
        .unwrap();
    flush_engine(&engine).await;
    engine.shutdown().await.unwrap();

    let snapshots = load_story_snapshots(dir.path()).unwrap();
    assert!(snapshots.contains_key("sess-1"));
    assert_eq!(snapshots["sess-1"].turns.len(), 1);
}

#[tokio::test]
async fn flush_persists_dirty_session_index() {
    let sink = RecordingSink::new();
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(sink.clone(), dir.path(), false).await;
    let ctx = test_context();
    engine
        .apply(
            &ctx,
            Event::Request(RequestEvent {
                path: "/v1/chat/completions".into(),
                body_bytes: 12,
                user_content: Some("ping".into()),
                body_json: None,
                model_rewritten: false,
            }),
        )
        .await
        .unwrap();
    engine.flush().await.unwrap();

    let index = SessionIndexStore::load(dir.path()).unwrap();
    assert_eq!(index.sessions.len(), 1);
    assert_eq!(index.sessions[0].session_id, "sess-1");
    assert!(index.sessions[0].active);
}

#[tokio::test]
async fn request_event_appends_single_llm_request() {
    let sink = RecordingSink::new();
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(sink.clone(), dir.path(), false).await;
    let ctx = test_context();
    engine
        .apply(
            &ctx,
            Event::Request(RequestEvent {
                path: "/v1/chat/completions".into(),
                body_bytes: 12,
                user_content: Some("hi".into()),
                body_json: None,
                model_rewritten: false,
            }),
        )
        .await
        .unwrap();
    flush_engine(&engine).await;
    let records = sink.drain();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].kind, "llm.request");
}

#[tokio::test]
async fn response_event_appends_single_stream_record() {
    let sink = RecordingSink::new();
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(sink.clone(), dir.path(), false).await;
    let ctx = test_context();
    engine
        .apply(
            &ctx,
            Event::ResponseComplete(CompleteEvent {
                status: 200,
                resp_bytes: Bytes::from("data: [DONE]\n\n"),
                streaming: true,
                stream_metrics: None,
                assistant_content: Some("hello".into()),
            }),
        )
        .await
        .unwrap();
    flush_engine(&engine).await;
    let records = sink.drain();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].kind, "llm.response.stream");
    assert_eq!(
        records[0].payload["assistant_content"].as_str(),
        Some("hello")
    );
}

#[tokio::test]
async fn draft_event_does_not_append_to_sink() {
    let sink = RecordingSink::new();
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(sink.clone(), dir.path(), true).await;
    let ctx = test_context();
    engine
        .apply(
            &ctx,
            Event::ResponseDraft(DraftEvent {
                status: 200,
                assistant_content: "partial".into(),
            }),
        )
        .await
        .unwrap();
    flush_engine(&engine).await;
    assert!(sink.drain().is_empty());
}

#[tokio::test]
async fn stream_markdown_keeps_user_block_when_assistant_upserts() {
    use crate::markdown_trajectory::read_blocks_from_file;
    use crate::session_storage::trajectory_run_dir;

    let sink = RecordingSink::new();
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().to_path_buf();
    let engine = test_engine(sink.clone(), &storage, true).await;
    let ctx = test_context();

    engine
        .apply(
            &ctx,
            Event::Request(RequestEvent {
                path: "/v1/chat/completions".into(),
                body_bytes: 12,
                user_content: Some("hello user".into()),
                body_json: None,
                model_rewritten: false,
            }),
        )
        .await
        .unwrap();
    engine
        .apply(
            &ctx,
            Event::ResponseDraft(DraftEvent {
                status: 200,
                assistant_content: "partial assistant".into(),
            }),
        )
        .await
        .unwrap();
    engine
        .apply(
            &ctx,
            Event::ResponseComplete(CompleteEvent {
                status: 200,
                resp_bytes: Bytes::from("data: [DONE]\n\n"),
                streaming: true,
                stream_metrics: None,
                assistant_content: Some("final assistant".into()),
            }),
        )
        .await
        .unwrap();

    flush_engine(&engine).await;
    let run_dir = trajectory_run_dir(storage.as_path(), ctx.agent_id(), ctx.route());
    let md_path = session_markdown_write_path_for_key(&run_dir, &ctx.route().storage_session_id);
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

#[tokio::test]
async fn draft_markdown_uses_peeked_seq_and_matches_final() {
    use crate::markdown_trajectory::read_blocks_from_file;
    use crate::session_storage::trajectory_run_dir;

    let sink = RecordingSink::new();
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().to_path_buf();
    let engine = test_engine(sink.clone(), &storage, true).await;
    let ctx = test_context();

    engine
        .apply(
            &ctx,
            Event::Request(RequestEvent {
                path: "/v1/chat/completions".into(),
                body_bytes: 12,
                user_content: Some("hi".into()),
                body_json: None,
                model_rewritten: false,
            }),
        )
        .await
        .unwrap();
    engine
        .apply(
            &ctx,
            Event::ResponseDraft(DraftEvent {
                status: 200,
                assistant_content: "wip".into(),
            }),
        )
        .await
        .unwrap();

    flush_engine(&engine).await;
    let run_dir = trajectory_run_dir(storage.as_path(), ctx.agent_id(), ctx.route());
    let md_path = session_markdown_write_path_for_key(&run_dir, &ctx.route().storage_session_id);
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
            &ctx,
            Event::ResponseComplete(CompleteEvent {
                status: 200,
                resp_bytes: Bytes::from("data: [DONE]\n\n"),
                streaming: true,
                stream_metrics: None,
                assistant_content: Some("done".into()),
            }),
        )
        .await
        .unwrap();

    flush_engine(&engine).await;
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

#[tokio::test]
async fn overlapping_calls_preserve_later_user_block_in_markdown() {
    use crate::markdown_trajectory::read_blocks_from_file;
    use crate::session_storage::trajectory_run_dir;

    let sink = RecordingSink::new();
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().to_path_buf();
    let engine = test_engine(sink.clone(), &storage, true).await;
    let mut ctx_a = test_context();
    ctx_a.call = Call {
        call_id: "call-a".into(),
        trace_id: "trace-a".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
    };

    engine
        .apply(
            &ctx_a,
            Event::Request(RequestEvent {
                path: "/v1/chat/completions".into(),
                body_bytes: 12,
                user_content: Some("req-a".into()),
                body_json: None,
                model_rewritten: false,
            }),
        )
        .await
        .unwrap();
    engine
        .apply(
            &ctx_a,
            Event::ResponseDraft(DraftEvent {
                status: 200,
                assistant_content: "draft-a".into(),
            }),
        )
        .await
        .unwrap();

    let mut ctx_b = ctx_a.clone();
    ctx_b.call = Call {
        call_id: "call-b".into(),
        trace_id: "trace-b".into(),
        started_at: "2026-01-01T00:00:01Z".into(),
    };
    engine
        .apply(
            &ctx_b,
            Event::Request(RequestEvent {
                path: "/v1/chat/completions".into(),
                body_bytes: 12,
                user_content: Some("req-b".into()),
                body_json: None,
                model_rewritten: false,
            }),
        )
        .await
        .unwrap();

    engine
        .apply(
            &ctx_a,
            Event::ResponseComplete(CompleteEvent {
                status: 200,
                resp_bytes: Bytes::from("data: [DONE]\n\n"),
                streaming: true,
                stream_metrics: None,
                assistant_content: Some("final-a".into()),
            }),
        )
        .await
        .unwrap();

    flush_engine(&engine).await;
    let run_dir = trajectory_run_dir(storage.as_path(), ctx_a.agent_id(), ctx_a.route());
    let md_path = session_markdown_write_path_for_key(&run_dir, &ctx_a.route().storage_session_id);
    let blocks = read_blocks_from_file(&md_path).unwrap();
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[0].value_utf8().unwrap(), "req-a");
    assert_eq!(blocks[1].value_utf8().unwrap(), "final-a");
    assert_eq!(blocks[2].value_utf8().unwrap(), "req-b");
}

fn claude_messages_body(user_lines: &[&str]) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = user_lines
        .iter()
        .map(|line| {
            serde_json::json!({
                "role": "user",
                "content": [{"type": "text", "text": line}]
            })
        })
        .collect();
    serde_json::json!({"messages": messages, "model": "deepseek-v4-pro"})
}

#[tokio::test]
async fn replay_dedup_omits_internal_claude_history_request_from_markdown() {
    use crate::markdown_trajectory::read_blocks_from_file;
    use crate::session_storage::trajectory_run_dir;

    let sink = RecordingSink::new();
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().to_path_buf();
    let engine = test_engine(sink.clone(), &storage, true).await;
    let ctx = test_context();

    async fn user_turn(
        engine: &CaptureEngine,
        ctx: &CallContext,
        call_id: &str,
        user: &str,
        prior_users: &[&str],
    ) {
        let mut lines: Vec<&str> = prior_users.to_vec();
        lines.push(user);
        let body = claude_messages_body(&lines);
        let mut call_ctx = ctx.clone();
        call_ctx.call = Call {
            call_id: call_id.into(),
            trace_id: call_id.into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        engine
            .apply(
                &call_ctx,
                Event::Request(RequestEvent {
                    path: "/v1/messages".into(),
                    body_bytes: 100,
                    user_content: Some(user.into()),
                    body_json: Some(body),
                    model_rewritten: false,
                }),
            )
            .await
            .unwrap();
    }

    async fn assistant_turn(engine: &CaptureEngine, ctx: &CallContext, call_id: &str, text: &str) {
        let mut call_ctx = ctx.clone();
        call_ctx.call = Call {
            call_id: call_id.into(),
            trace_id: call_id.into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        engine
            .apply(
                &call_ctx,
                Event::ResponseComplete(CompleteEvent {
                    status: 200,
                    resp_bytes: Bytes::from("data: [DONE]\n\n"),
                    streaming: true,
                    stream_metrics: None,
                    assistant_content: Some(text.into()),
                }),
            )
            .await
            .unwrap();
    }

    user_turn(&engine, &ctx, "call-1", "hi", &[]).await;
    assistant_turn(&engine, &ctx, "call-1", "Hi! What can I help you with?").await;
    user_turn(&engine, &ctx, "call-2", "hi", &["hi"]).await;
    assistant_turn(&engine, &ctx, "call-2", "Hi! What can I help you with?").await;
    user_turn(&engine, &ctx, "call-3", "hi", &["hi"]).await;
    assistant_turn(
        &engine,
        &ctx,
        "call-3",
        "what's the status of the capture storage feature?",
    )
    .await;
    user_turn(&engine, &ctx, "call-4", "你好", &["hi", "hi"]).await;
    assistant_turn(&engine, &ctx, "call-4", "你好！有什么我可以帮你的吗？").await;

    flush_engine(&engine).await;
    let run_dir = trajectory_run_dir(storage.as_path(), ctx.agent_id(), ctx.route());
    let md_path = session_markdown_write_path_for_key(&run_dir, &ctx.route().storage_session_id);
    let blocks = read_blocks_from_file(&md_path).unwrap();
    let bodies: Vec<_> = blocks
        .iter()
        .map(|b| b.value_utf8().unwrap().to_string())
        .collect();
    assert_eq!(
        bodies,
        vec![
            "hi".to_string(),
            "Hi! What can I help you with?".to_string(),
            "hi".to_string(),
            "Hi! What can I help you with?".to_string(),
            "你好".to_string(),
            "你好！有什么我可以帮你的吗？".to_string(),
        ]
    );
}

mod reliability {
    use super::*;
    use crate::dead_letter::read_dead_letter_entries;
    use crate::engine::Event;

    struct FailingSink;

    impl CaptureSink for FailingSink {
        fn append(
            &self,
            _route: &CaptureRoute,
            _agent_id: &str,
            _record: &mut CaptureRecord,
        ) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("sink unavailable"))
        }
    }

    #[tokio::test]
    async fn session_sink_failure_writes_dead_letter_with_record() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(dir.path().to_path_buf());
        let sink = Arc::new(FailingSink);
        let index = SessionIndexStore::open(dir.path()).unwrap().clone_handle();
        let engine = CaptureEngine::new(sink, index, storage.clone(), false)
            .await
            .unwrap();
        let ctx = test_context();
        let event = Event::Request(RequestEvent {
            path: "/v1/chat/completions".into(),
            body_bytes: 10,
            user_content: Some("hi".into()),
            body_json: None,
            model_rewritten: false,
        });
        assert!(engine.apply(&ctx, event).await.is_err());
        engine.flush().await.unwrap();
        let entries = read_dead_letter_entries(dir.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].error.contains("sink unavailable"));
        assert!(entries[0].prepared_record_json.is_some());
    }
}
