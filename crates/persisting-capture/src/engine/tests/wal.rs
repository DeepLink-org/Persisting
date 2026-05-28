use super::fixtures::*;
use super::support::*;

#[tokio::test]
async fn wal_replays_unacked_events_on_next_engine() {
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path();

    // Manually append an unacked event to the WAL — this models the case where
    // the original process appended but crashed before the dispatcher applied it.
    {
        let wal = crate::engine::wal::EventWal::open(storage);
        let appended = wal.append_event(
            &test_context(),
            &Event::Request(RequestEvent {
                path: "/v1/chat/completions".into(),
                body_bytes: 5,
                user_content: Some("survives crash".into()),
                body_json: None,
                model_rewritten: false,
            }),
        );
        assert!(appended.is_some(), "wal append should have succeeded");
    }

    // Sanity check: the event is pending replay before any engine boots.
    let pending = crate::engine::wal::replay_pending(storage);
    assert_eq!(pending.len(), 1, "wal should hold one unacked entry");

    // A fresh engine on the same storage replays the unacked event and routes
    // it to its own sink, exactly as the original spawn_apply would have.
    let sink = RecordingSink::new();
    let engine = test_engine(sink.clone(), storage, false).await;
    engine.flush().await.unwrap();

    let records = sink.drain();
    assert!(
        records
            .iter()
            .any(|r| r.payload.get("user_content").and_then(|v| v.as_str())
                == Some("survives crash")),
        "expected wal-replayed request to flow through new engine sink: {records:?}"
    );

    // Clean shutdown should truncate the WAL so a third engine sees no pending events.
    engine.shutdown().await.unwrap();
    let pending_after = crate::engine::wal::replay_pending(storage);
    assert!(
        pending_after.is_empty(),
        "wal not truncated on clean shutdown"
    );
}
