use axum::http::HeaderMap;
use persisting_dlcapt::config::{ExportConfig, ModelRoute, ProxyConfig, StorageConfig};
use persisting_dlcapt::dialogue::{InferenceEndpoint, extract_user_text};
use persisting_dlcapt::router::RouteTable;
use persisting_dlcapt::session::{
    RequestContext, RouteSessionMode, SessionSource, extract_session_from_headers, resolve_session,
    resolve_session_id, resolve_session_with_source,
};
use persisting_dlcapt::tlv::{TlvTurnRecord, TlvWriter};
use serde_json::json;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::Mutex;

fn test_config() -> ProxyConfig {
    ProxyConfig {
        listen: "127.0.0.1:19081".to_string(),
        admin_listen: "127.0.0.1:19082".to_string(),
        store_dir: "store".to_string(),
        agent_id: "openclaw".to_string(),
        session_header: "x-persisting-session-id".to_string(),
        session_header_aliases: vec![],
        default_session_id: "default".to_string(),
        preserve_raw: false,
        base_session_path: "/v1/sessions".to_string(),
        storage: StorageConfig::default(),
        export: ExportConfig::default(),
        models: vec![
            ModelRoute {
                name: "kimi-k2.5".to_string(),
                display_name: Some("Kimi K2.5".to_string()),
                provider: "openai".to_string(),
                upstream_base_url: "http://exact-upstream/v1".to_string(),
                api_key: Some("exact-key".to_string()),
            },
            ModelRoute {
                name: "*".to_string(),
                display_name: Some("Fallback".to_string()),
                provider: "openai".to_string(),
                upstream_base_url: "http://fallback-upstream/v1".to_string(),
                api_key: Some("fallback-key".to_string()),
            },
        ],
    }
}

#[test]
fn model_route_should_match_exact_then_wildcard() {
    let config = test_config();
    let routes = RouteTable::from_config(&config);
    let exact = routes
        .resolve_model("kimi-k2.5")
        .expect("should match exact");
    assert_eq!(exact.upstream_base_url, "http://exact-upstream/v1");

    let fallback = routes
        .resolve_model("unknown-model")
        .expect("should match wildcard");
    assert_eq!(fallback.upstream_base_url, "http://fallback-upstream/v1");
}

#[test]
fn session_id_resolution_priority_should_be_header_then_body_then_default() {
    let body = json!({
        "metadata": {
            "session_id": "body-session"
        }
    });

    let from_header = resolve_session_id(Some("header-session"), &body, "default-session");
    assert_eq!(from_header, "header-session");

    let from_body = resolve_session_id(None, &body, "default-session");
    assert_eq!(from_body, "body-session");

    let from_default = resolve_session_id(None, &json!({}), "default-session");
    assert_eq!(from_default, "default-session");
}

#[test]
fn session_header_aliases_should_be_checked_in_order() {
    let mut headers = HeaderMap::new();
    headers.insert("x-openclaw-session-id", "openclaw-session".parse().unwrap());

    let session = extract_session_from_headers(&headers, "x-persisting-session-id", &[])
        .expect("should resolve alias header");
    assert_eq!(session, "openclaw-session");
}

#[test]
fn configured_session_header_aliases_should_be_honored() {
    let mut headers = HeaderMap::new();
    headers.insert("x-custom-session", "custom-session".parse().unwrap());

    let session = extract_session_from_headers(
        &headers,
        "x-persisting-session-id",
        &["x-custom-session".to_string()],
    )
    .expect("should resolve configured alias");
    assert_eq!(session, "custom-session");
}

#[test]
fn resolve_session_with_source_should_mark_default_fallback() {
    let (session_id, source) = resolve_session_with_source(None, &json!({}), "default");
    assert_eq!(session_id, "default");
    assert_eq!(source, SessionSource::Default);
}

#[test]
fn session_scoped_resolver_should_normalize_url_and_ignore_header() {
    let mut headers = HeaderMap::new();
    headers.insert("x-session-id", "bbb".parse().unwrap());
    let body = json!({});
    let ctx = RequestContext {
        mode: RouteSessionMode::SessionScoped,
        path_session_id: Some("aaa"),
        headers: &headers,
        body: &body,
    };

    let resolved = resolve_session(&ctx, &test_config().session_settings()).expect("resolve");
    assert_eq!(resolved.storage_session_id, "aaa");
    assert_eq!(resolved.source, SessionSource::UrlPath);
    assert_eq!(resolved.conflicts.len(), 1);
}

#[test]
fn responses_user_extraction_should_read_string_input() {
    let body = json!({"input": "responses user"});
    assert_eq!(
        extract_user_text(InferenceEndpoint::Responses, &body).as_deref(),
        Some("responses user")
    );
}

#[tokio::test]
async fn tlv_writer_should_append_same_session_into_one_markdown_file() {
    let temp = tempdir().expect("temp dir");
    let writer = TlvWriter::new(
        temp.path().to_path_buf(),
        "openclaw".to_string(),
        "default".to_string(),
        Arc::new(Mutex::new(())),
    );

    let first = TlvTurnRecord {
        session_id: "feishu-main-abc".to_string(),
        agent_id: "openclaw".to_string(),
        model: "kimi-k2.5".to_string(),
        stream: false,
        status_code: 200,
        user_text: Some("hi".to_string()),
        assistant_text: Some("hello".to_string()),
        usage: Some(json!({"prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13})),
        user_seq: 0,
        assistant_seq: 1,
        turn: 1,
        call_id: "call-test-1".to_string(),
        request_path: "/v1/sessions/feishu-main-abc/chat/completions".to_string(),
    };
    let second = TlvTurnRecord {
        session_id: "feishu-main-abc".to_string(),
        agent_id: "openclaw".to_string(),
        model: "kimi-k2.5".to_string(),
        stream: false,
        status_code: 200,
        user_text: Some("again".to_string()),
        assistant_text: Some("sure".to_string()),
        usage: Some(json!({"prompt_tokens": 12, "completion_tokens": 2, "total_tokens": 14})),
        user_seq: 2,
        assistant_seq: 3,
        turn: 2,
        call_id: "call-test-2".to_string(),
        request_path: "/v1/sessions/feishu-main-abc/chat/completions".to_string(),
    };

    let first_path = writer.append_turn(first).await.expect("write first turn");
    let second_path = writer.append_turn(second).await.expect("write second turn");
    assert_eq!(first_path, second_path);
    assert!(first_path.ends_with("feishu-main-abc/trajectory.md"));

    let content = tokio::fs::read_to_string(second_path)
        .await
        .expect("read markdown");
    assert!(content.contains("format: persisting:1.0"));
    assert!(content.contains("session: \"feishu-main-abc\""));
    assert!(content.contains("turns: 2"));
    assert!(content.contains("<!-- persisting:block:user"));
    assert!(content.contains("<!-- persisting:block:assistant"));
    assert!(content.contains("hi"));
    assert!(content.contains("hello"));
    assert!(content.contains("again"));
    assert!(content.contains("sure"));
    assert!(content.contains("\"path\":\"/v1/sessions/feishu-main-abc/chat/completions\""));
    assert!(content.contains("\"session_id\":\"feishu-main-abc\""));

    let storyline =
        persisting_pchronicle::document::decode_agenticmd(&content).unwrap_or_else(|error| {
            panic!("dlcapt output remains valid AgenticMD: {error}\n--- output ---\n{content}")
        });
    assert_eq!(storyline.session_id, "feishu-main-abc");
    assert_eq!(storyline.agent.id, "openclaw");
    assert_eq!(storyline.turns.len(), 4);
}

#[tokio::test]
async fn tlv_writer_should_mix_chat_and_responses_in_one_session_file() {
    let temp = tempdir().expect("temp dir");
    let writer = TlvWriter::new(
        temp.path().to_path_buf(),
        "openclaw".to_string(),
        "default".to_string(),
        Arc::new(Mutex::new(())),
    );

    let chat = TlvTurnRecord {
        session_id: "agent-session-1".to_string(),
        agent_id: "openclaw".to_string(),
        model: "kimi-k2.5".to_string(),
        stream: false,
        status_code: 200,
        user_text: Some("chat user".to_string()),
        assistant_text: Some("chat assistant".to_string()),
        usage: None,
        user_seq: 0,
        assistant_seq: 1,
        turn: 1,
        call_id: "call-chat".to_string(),
        request_path: "/v1/sessions/agent-session-1/chat/completions".to_string(),
    };
    let responses = TlvTurnRecord {
        session_id: "agent-session-1".to_string(),
        agent_id: "openclaw".to_string(),
        model: "kimi-k2.5".to_string(),
        stream: false,
        status_code: 200,
        user_text: Some("responses user".to_string()),
        assistant_text: Some("responses assistant".to_string()),
        usage: None,
        user_seq: 2,
        assistant_seq: 3,
        turn: 2,
        call_id: "call-responses".to_string(),
        request_path: "/v1/sessions/agent-session-1/responses".to_string(),
    };

    let chat_path = writer.append_turn(chat).await.expect("write chat");
    let responses_path = writer
        .append_turn(responses)
        .await
        .expect("write responses");
    assert_eq!(chat_path, responses_path);
    assert!(chat_path.ends_with("agent-session-1/trajectory.md"));

    let content = tokio::fs::read_to_string(responses_path)
        .await
        .expect("read markdown");
    assert!(content.contains("chat user"));
    assert!(content.contains("responses user"));
    assert!(content.contains("\"path\":\"/v1/sessions/agent-session-1/chat/completions\""));
    assert!(content.contains("\"path\":\"/v1/sessions/agent-session-1/responses\""));
}

#[tokio::test]
async fn capture_sink_should_write_session_steps_and_trajectory() {
    use persisting_dlcapt::capture::{
        CaptureEvent, CaptureMeta, CaptureSinkRouter, PostProcessorChain,
    };
    use persisting_dlcapt::config::{ExportConfig, ProxyConfig, StorageConfig};
    use persisting_dlcapt::tlv::TlvWriter;
    use std::collections::BTreeMap;

    let temp = tempdir().expect("temp dir");
    let config = ProxyConfig {
        store_dir: temp.path().to_string_lossy().to_string(),
        agent_id: "openclaw".to_string(),
        default_session_id: "default".to_string(),
        storage: StorageConfig {
            authoritative: "json_file".to_string(),
            also: vec!["md".to_string()],
            ..StorageConfig::default()
        },
        export: ExportConfig::default(),
        ..test_config()
    };
    let write_lock = Arc::new(Mutex::new(()));
    let tlv = TlvWriter::new(
        temp.path().to_path_buf(),
        config.agent_id.clone(),
        config.default_session_id.clone(),
        Arc::clone(&write_lock),
    );
    let sink = CaptureSinkRouter::new(Arc::new(config), tlv, write_lock).expect("router");
    let processors = PostProcessorChain::empty();

    let mut event = CaptureEvent {
        call_id: "call-capture-1".to_string(),
        session_id: "sess-json".to_string(),
        agent_id: "openclaw".to_string(),
        step_id: 1,
        turn: 1,
        endpoint: InferenceEndpoint::ChatCompletions,
        request_path: "/v1/chat/completions".to_string(),
        model: "kimi-k2.5".to_string(),
        request: json!({"messages": [{"role": "user", "content": "ping"}]}),
        request_headers: BTreeMap::new(),
        response_raw: json!({
            "choices": [{"message": {"role": "assistant", "content": "pong"}, "finish_reason": "stop"}]
        }),
        response_text: Some("pong".to_string()),
        stream: false,
        status_code: 200,
        completed_at: chrono::Utc::now(),
        metadata: BTreeMap::new(),
        field_patches: BTreeMap::new(),
        capture_meta: CaptureMeta {
            finish_reason: Some("stop".to_string()),
            usage: Some(json!({"total_tokens": 5})),
            segment_kind: None,
        },
        user_seq: 0,
        assistant_seq: 1,
    };
    processors.apply(&mut event);
    sink.dispatch(event).await.expect("dispatch capture");

    let md_path = temp.path().join("sess-json/trajectory.md");
    let json_path = temp.path().join("sess-json/session_steps.json");
    assert!(md_path.exists(), "trajectory.md missing");
    assert!(json_path.exists(), "session_steps.json missing");

    let md = tokio::fs::read_to_string(md_path).await.expect("read md");
    assert!(md.contains("ping"));
    assert!(md.contains("pong"));

    let json_text = tokio::fs::read_to_string(json_path)
        .await
        .expect("read json");
    let envelope: serde_json::Value = serde_json::from_str(&json_text).expect("parse json");
    assert_eq!(envelope["authoritative"], "json_file");
    assert_eq!(envelope["session_steps"].as_array().unwrap().len(), 1);
    assert_eq!(envelope["session_steps"][0]["id"], "dlcapt:sess-json:1");
}

#[tokio::test]
async fn capture_sink_should_write_lance_when_also_contains_lance() {
    use lance::Dataset;
    use persisting_dlcapt::capture::{
        CaptureEvent, CaptureMeta, CaptureSinkRouter, PostProcessorChain,
    };
    use persisting_dlcapt::config::{ExportConfig, LanceStorageConfig, ProxyConfig, StorageConfig};
    use persisting_dlcapt::tlv::TlvWriter;
    use std::collections::BTreeMap;

    let temp = tempdir().expect("temp dir");
    let lance_dir = temp.path().join("lance-db");
    let config = ProxyConfig {
        store_dir: temp.path().to_string_lossy().to_string(),
        agent_id: "openclaw".to_string(),
        default_session_id: "default".to_string(),
        storage: StorageConfig {
            authoritative: "json_file".to_string(),
            also: vec!["md".to_string(), "lance".to_string()],
            lance: LanceStorageConfig {
                db_uri: lance_dir.to_string_lossy().to_string(),
                table_name: "session_steps".to_string(),
                ..LanceStorageConfig::default()
            },
            ..StorageConfig::default()
        },
        export: ExportConfig::default(),
        ..test_config()
    };
    config.storage.validate().expect("valid storage");

    let write_lock = Arc::new(Mutex::new(()));
    let tlv = TlvWriter::new(
        temp.path().to_path_buf(),
        config.agent_id.clone(),
        config.default_session_id.clone(),
        Arc::clone(&write_lock),
    );
    let sink = CaptureSinkRouter::new(Arc::new(config), tlv, write_lock).expect("router");
    let processors = PostProcessorChain::empty();

    let mut event = CaptureEvent {
        call_id: "call-lance-1".to_string(),
        session_id: "sess-lance".to_string(),
        agent_id: "openclaw".to_string(),
        step_id: 1,
        turn: 1,
        endpoint: InferenceEndpoint::ChatCompletions,
        request_path: "/v1/chat/completions".to_string(),
        model: "kimi-k2.5".to_string(),
        request: json!({"messages": [{"role": "user", "content": "lance ping"}]}),
        request_headers: BTreeMap::new(),
        response_raw: json!({
            "choices": [{"message": {"role": "assistant", "content": "lance pong"}, "finish_reason": "stop"}]
        }),
        response_text: Some("lance pong".to_string()),
        stream: false,
        status_code: 200,
        completed_at: chrono::Utc::now(),
        metadata: BTreeMap::new(),
        field_patches: BTreeMap::new(),
        capture_meta: CaptureMeta {
            finish_reason: Some("stop".to_string()),
            usage: Some(json!({"total_tokens": 5})),
            segment_kind: None,
        },
        user_seq: 0,
        assistant_seq: 1,
    };
    processors.apply(&mut event);
    sink.dispatch(event).await.expect("dispatch capture");

    let json_path = temp.path().join("sess-lance/session_steps.json");
    assert!(json_path.exists(), "session_steps.json missing");

    let dataset_uri = lance_dir.join("session_steps.lance");
    assert!(dataset_uri.exists(), "lance dataset missing");

    let dataset = Dataset::open(dataset_uri.to_string_lossy().as_ref())
        .await
        .expect("open lance dataset");
    let count = dataset.count_rows(None).await.expect("count rows");
    assert_eq!(count, 1);
}

#[test]
fn storage_config_should_reject_json_cache_without_lance_authoritative() {
    use persisting_dlcapt::config::{JsonCacheConfig, StorageConfig};

    let cfg = StorageConfig {
        authoritative: "json_file".to_string(),
        also: vec!["json_cache".to_string()],
        json_cache: JsonCacheConfig { enabled: false },
        ..StorageConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string()
            .contains("storage.json_cache is only valid when storage.authoritative = \"lance\"")
    );
}

#[test]
fn storage_config_should_require_db_uri_when_lance_enabled() {
    use persisting_dlcapt::config::StorageConfig;

    let cfg = StorageConfig {
        authoritative: "json_file".to_string(),
        also: vec!["lance".to_string()],
        ..StorageConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string()
            .contains("storage.lance.db_uri is required when Lance sink is enabled")
    );
}

#[test]
fn storage_config_should_require_s3_region_for_s3_db_uri() {
    use persisting_dlcapt::config::{LanceStorageConfig, StorageConfig};

    let cfg = StorageConfig {
        authoritative: "lance".to_string(),
        lance: LanceStorageConfig {
            db_uri: "s3://my-bucket/capture-prod".to_string(),
            ..LanceStorageConfig::default()
        },
        ..StorageConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string()
            .contains("storage.lance.s3.region is required")
    );
}

#[test]
fn storage_config_should_accept_s3_db_uri_with_region() {
    use persisting_dlcapt::config::{LanceS3Config, LanceStorageConfig, StorageConfig};

    let cfg = StorageConfig {
        authoritative: "lance".to_string(),
        lance: LanceStorageConfig {
            db_uri: "s3://my-bucket/capture-prod".to_string(),
            s3: Some(LanceS3Config {
                region: "cn-north-1".to_string(),
                endpoint: Some("https://minio.local".to_string()),
                allow_http: Some(true),
            }),
            ..LanceStorageConfig::default()
        },
        ..StorageConfig::default()
    };
    cfg.validate().expect("valid s3 config");
}
