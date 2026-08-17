use super::fixtures::*;

#[tokio::test]
async fn stream_markdown_keeps_user_block_when_assistant_upserts() {
    use super::support::*;
    use crate::session::storage::trajectory_run_dir;
    use persisting_pchronicle::document::parse_agenticmd;

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
                method: "POST".into(),
                url: None,
                body_bytes: 12,
                user_content: Some("hello user".into()),
                body_json: None,
                semantic: None,
                model_rewritten: false,
                headers: vec![],
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
                semantic: None,
                headers: vec![],
            }),
        )
        .await
        .unwrap();

    flush_engine(&engine).await;
    let run_dir = trajectory_run_dir(storage.as_path(), ctx.agent_id(), ctx.route());
    let md_path = session_markdown_write_path_for_key(&run_dir, &ctx.route().storage_session_id);
    let story = parse_agenticmd(&std::fs::read_to_string(&md_path).unwrap()).unwrap();
    let records = sink.drain();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].seq, 0);
    assert_eq!(records[1].seq, 1);
    assert_eq!(story.turns.len(), 2);
    assert_eq!(story.turns[0].source, "user");
    assert_eq!(story.turns[0].message, serde_json::json!("hello user"));
    assert_eq!(story.turns[0].id, 0);
    assert_eq!(story.turns[1].source, "agent");
    assert_eq!(story.turns[1].message, serde_json::json!("final assistant"));
    assert_eq!(story.turns[1].id, 1);
}

#[tokio::test]
async fn draft_markdown_uses_peeked_seq_and_matches_final() {
    use super::support::*;
    use crate::session::storage::trajectory_run_dir;
    use persisting_pchronicle::document::parse_agenticmd;

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
                method: "POST".into(),
                url: None,
                body_bytes: 12,
                user_content: Some("hi".into()),
                body_json: None,
                semantic: None,
                model_rewritten: false,
                headers: vec![],
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
    let draft_story = parse_agenticmd(&std::fs::read_to_string(&md_path).unwrap()).unwrap();
    assert_eq!(draft_story.turns.len(), 2);
    assert_eq!(draft_story.turns[1].id, 1);
    assert_eq!(draft_story.turns[1].message, serde_json::json!("wip"));

    engine
        .apply(
            &ctx,
            Event::ResponseComplete(CompleteEvent {
                status: 200,
                resp_bytes: Bytes::from("data: [DONE]\n\n"),
                streaming: true,
                stream_metrics: None,
                assistant_content: Some("done".into()),
                semantic: None,
                headers: vec![],
            }),
        )
        .await
        .unwrap();

    flush_engine(&engine).await;
    let final_story = parse_agenticmd(&std::fs::read_to_string(&md_path).unwrap()).unwrap();
    assert_eq!(final_story.turns[1].id, 1);
    assert_eq!(final_story.turns[1].message, serde_json::json!("done"));
    assert_eq!(sink.drain()[1].seq, 1);
}

#[tokio::test]
async fn overlapping_calls_preserve_later_user_block_in_markdown() {
    use super::support::*;
    use crate::session::storage::trajectory_run_dir;
    use persisting_pchronicle::document::parse_agenticmd;

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
                method: "POST".into(),
                url: None,
                body_bytes: 12,
                user_content: Some("req-a".into()),
                body_json: None,
                semantic: None,
                model_rewritten: false,
                headers: vec![],
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
                method: "POST".into(),
                url: None,
                body_bytes: 12,
                user_content: Some("req-b".into()),
                body_json: None,
                semantic: None,
                model_rewritten: false,
                headers: vec![],
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
                semantic: None,
                headers: vec![],
            }),
        )
        .await
        .unwrap();

    flush_engine(&engine).await;
    let run_dir = trajectory_run_dir(storage.as_path(), ctx_a.agent_id(), ctx_a.route());
    let md_path = session_markdown_write_path_for_key(&run_dir, &ctx_a.route().storage_session_id);
    let story = parse_agenticmd(&std::fs::read_to_string(&md_path).unwrap()).unwrap();
    assert_eq!(story.turns.len(), 3);
    assert_eq!(story.turns[0].message, serde_json::json!("req-a"));
    assert_eq!(story.turns[1].message, serde_json::json!("final-a"));
    assert_eq!(story.turns[2].message, serde_json::json!("req-b"));
}
