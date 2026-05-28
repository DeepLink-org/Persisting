use crate::session_index::SessionIndexStore;

use super::fixtures::*;
use super::support::*;

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
async fn shutdown_drains_spawned_apply_before_snapshot() {
    use crate::engine::load_story_snapshots;

    let sink = RecordingSink::new();
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(sink.clone(), dir.path(), false).await;
    let ctx = test_context();
    engine.spawn_apply(
        ctx,
        Event::Request(RequestEvent {
            path: "/v1/chat/completions".into(),
            body_bytes: 12,
            user_content: Some("queued hello".into()),
            body_json: None,
            model_rewritten: false,
        }),
    );

    engine.shutdown().await.unwrap();

    let snapshots = load_story_snapshots(dir.path()).unwrap();
    assert_eq!(
        snapshots["sess-1"].turns[0]
            .user
            .as_ref()
            .map(|b| b.text.as_str()),
        Some("queued hello")
    );
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
async fn flush_drains_spawned_apply_without_sleep() {
    let sink = RecordingSink::new();
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(sink.clone(), dir.path(), false).await;
    let ctx = test_context();
    engine.spawn_apply(
        ctx,
        Event::Request(RequestEvent {
            path: "/v1/chat/completions".into(),
            body_bytes: 12,
            user_content: Some("queued ping".into()),
            body_json: None,
            model_rewritten: false,
        }),
    );

    engine.flush().await.unwrap();

    let records = sink.drain();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].payload["user_content"].as_str(),
        Some("queued ping")
    );
}
