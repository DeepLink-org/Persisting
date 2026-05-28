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
