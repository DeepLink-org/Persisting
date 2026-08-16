use std::sync::Arc;

use crate::dead_letter::read_dead_letter_entries;
use crate::engine::{CaptureEngine, Event, RequestEvent};
use crate::record::EventRecord;
use crate::session::index::SessionIndexStore;
use crate::session::storage::CaptureRoute;
use crate::sink::CaptureEventSink;

use super::fixtures::test_context;

struct FailingSink;

impl CaptureEventSink for FailingSink {
    fn append(
        &self,
        _route: &CaptureRoute,
        _agent_id: &str,
        _record: &mut EventRecord,
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
        method: "POST".into(),
        url: None,
        body_bytes: 10,
        user_content: Some("hi".into()),
        body_json: None,
        semantic: None,
        model_rewritten: false,
        headers: vec![],
    });
    assert!(engine.apply(&ctx, event).await.is_err());
    engine.flush().await.unwrap();
    let entries = read_dead_letter_entries(dir.path()).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].error.contains("sink unavailable"));
    assert!(entries[0].prepared_record_json.is_some());
    let story = engine.story_snapshot(&ctx.story).await.unwrap();
    assert!(
        story.turns.is_empty(),
        "failed canonical append must not advance the story read model"
    );
}

#[tokio::test]
async fn failed_durable_sink_keeps_wal_event_pending_after_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(dir.path().to_path_buf());
    let index = SessionIndexStore::open(dir.path()).unwrap().clone_handle();
    let engine = CaptureEngine::new(Arc::new(FailingSink), index, storage.clone(), false)
        .await
        .unwrap();
    engine.spawn_apply(
        test_context(),
        Event::Request(RequestEvent {
            path: "/v1/chat/completions".into(),
            method: "POST".into(),
            url: None,
            body_bytes: 10,
            user_content: Some("retry me".into()),
            body_json: None,
            semantic: None,
            model_rewritten: false,
            headers: vec![],
        }),
    );

    engine.flush().await.unwrap();
    assert_eq!(crate::engine::wal::replay_pending(dir.path()).len(), 1);
    engine.shutdown().await.unwrap();
    assert_eq!(
        crate::engine::wal::replay_pending(dir.path()).len(),
        1,
        "clean shutdown must not erase an event rejected by the durable sink"
    );
}
