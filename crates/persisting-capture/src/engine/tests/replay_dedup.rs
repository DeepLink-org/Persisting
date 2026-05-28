use super::fixtures::*;
use super::support::*;

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
