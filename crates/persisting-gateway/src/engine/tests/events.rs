use super::fixtures::*;
use super::support::*;

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
    flush_engine(&engine).await;
    let records = sink.drain();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].kind, "llm.request");
}

#[tokio::test]
async fn request_event_projects_original_body_and_typed_understanding() {
    let sink = RecordingSink::new();
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(sink.clone(), dir.path(), false).await;
    let ctx = test_context();
    let original = serde_json::json!({
        "model": "deepseek-chat",
        "stream": true,
        "max_tokens": 64,
        "messages": [{"role": "user", "content": "keep me"}],
        "tools": [{"type": "function", "function": {"name": "shell"}}]
    });
    engine
        .apply(
            &ctx,
            Event::Request(RequestEvent {
                path: "/v1/chat/completions".into(),
                method: "POST".into(),
                url: Some("//localhost/v1/chat/completions".into()),
                body_bytes: serde_json::to_vec(&original).unwrap().len(),
                user_content: Some("keep me".into()),
                body_json: Some(original.clone()),
                semantic: None,
                model_rewritten: true,
                headers: vec![("content-type".into(), "application/json".into())],
            }),
        )
        .await
        .unwrap();
    flush_engine(&engine).await;

    let records = sink.drain();
    let payload = &records[0].payload;
    assert_eq!(payload["http"]["request_body"], original);
    assert!(payload["llm_request"].get("schema_version").is_none());
    assert_eq!(payload["llm_request"]["input_format"], "chat_completions");
    assert_eq!(payload["llm_request"]["request"]["model"], "deepseek-chat");
    assert_eq!(payload["llm_request"]["request"]["stream"], true);
    assert_eq!(
        payload["llm_request"]["request"]["generation"]["max_output_tokens"],
        64
    );
    assert_eq!(
        payload["llm_request"]["request"]["tools"][0]["name"],
        "shell"
    );
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
                semantic: None,
                headers: vec![],
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
    assert!(records[0].payload["llm_response"]
        .get("schema_version")
        .is_none());
    assert_eq!(
        records[0].payload["llm_response"]["output_format"],
        "chat_completions"
    );
    assert_eq!(
        records[0].payload["llm_response"]["response"]["candidates"][0]["message"]["parts"][0]
            ["text"],
        "hello"
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
