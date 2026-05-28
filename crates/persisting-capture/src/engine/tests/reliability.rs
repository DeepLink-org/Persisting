use std::sync::Arc;

use crate::dead_letter::read_dead_letter_entries;
use crate::engine::{CaptureEngine, Event, RequestEvent};
use crate::record::CaptureRecord;
use crate::session_index::SessionIndexStore;
use crate::session_storage::CaptureRoute;
use crate::sink::CaptureSink;

use super::fixtures::test_context;

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
