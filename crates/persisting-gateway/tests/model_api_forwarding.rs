//! End-to-end request and response forwarding checks for supported model APIs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::post;
use persisting_gateway::config::ProxyConfig;
use persisting_gateway::serve_with_listeners_and_shutdown;
use persisting_gateway::sink::SeqOnlySink;
use serde_json::{Value, json};
use tokio::sync::oneshot;

// The request/response shapes and tool-call fields follow recordings from the
// supplied Ornith-1.5-35B-A3B service; the test itself replays them locally.
const MODEL: &str = "Ornith-1.5-35B-A3B";
const UPSTREAM_KEY: &str = "upstream-test-key";
const CLIENT_TRACE: &str = "client-trace-42";
const MODEL_REQUEST_ID: &str = "model-request-42";

#[derive(Clone, Copy, Debug)]
enum Api {
    ChatCompletions,
    Responses,
    Messages,
}

impl Api {
    fn name(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat_completions",
            Self::Responses => "responses",
            Self::Messages => "messages",
        }
    }

    fn path(self) -> &'static str {
        match self {
            Self::ChatCompletions => "/v1/chat/completions",
            Self::Responses => "/v1/responses",
            Self::Messages => "/v1/messages",
        }
    }

    fn request(self, stream: bool, tool_call: bool) -> Value {
        let mut request = match self {
            Self::ChatCompletions => json!({
                "model": MODEL,
                "stream": stream,
                "temperature": 0.25,
                "messages": [{"role": "user", "content": "chat request"}]
            }),
            Self::Responses => json!({
                "model": MODEL,
                "stream": stream,
                "instructions": "Answer briefly.",
                "input": "responses request"
            }),
            Self::Messages => json!({
                "model": MODEL,
                "stream": stream,
                "max_tokens": 64,
                "system": "Answer briefly.",
                "messages": [{"role": "user", "content": "messages request"}]
            }),
        };
        if tool_call {
            request["tools"] = match self {
                Self::ChatCompletions => json!([{
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "description": "Get the weather.",
                        "parameters": {
                            "type": "object",
                            "properties": {"city": {"type": "string"}},
                            "required": ["city"]
                        }
                    }
                }]),
                Self::Responses => json!([{
                    "type": "function",
                    "name": "get_weather",
                    "description": "Get the weather.",
                    "parameters": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}},
                        "required": ["city"]
                    }
                }]),
                Self::Messages => json!([{
                    "name": "get_weather",
                    "description": "Get the weather.",
                    "input_schema": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}},
                        "required": ["city"]
                    }
                }]),
            };
            request["tool_choice"] = match self {
                Self::Messages => json!({"type": "tool", "name": "get_weather"}),
                Self::ChatCompletions | Self::Responses => json!("required"),
            };
        }
        request
    }

    fn non_streaming_response(self) -> Value {
        match self {
            Self::ChatCompletions => json!({
                "id": "chatcmpl-mock",
                "object": "chat.completion",
                "created": 0,
                "model": MODEL,
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "chat response"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 2, "completion_tokens": 2, "total_tokens": 4}
            }),
            Self::Responses => json!({
                "id": "resp-mock",
                "object": "response",
                "created_at": 0,
                "status": "completed",
                "model": MODEL,
                "output": [{
                    "id": "msg-mock",
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "responses response",
                        "annotations": []
                    }]
                }],
                "usage": {"input_tokens": 2, "output_tokens": 2, "total_tokens": 4}
            }),
            Self::Messages => json!({
                "id": "msg-mock",
                "type": "message",
                "role": "assistant",
                "model": MODEL,
                "content": [{"type": "text", "text": "messages response"}],
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": {"input_tokens": 2, "output_tokens": 2}
            }),
        }
    }

    fn tool_non_streaming_response(self) -> Value {
        match self {
            Self::ChatCompletions => json!({
                "id": "chatcmpl-tool-mock",
                "object": "chat.completion",
                "created": 0,
                "model": MODEL,
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call-weather-42",
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": "{\"city\":\"Paris\"}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
            }),
            Self::Responses => json!({
                "id": "resp-tool-mock",
                "object": "response",
                "created_at": 0,
                "status": "completed",
                "model": MODEL,
                "output": [{
                    "id": "fc-item-42",
                    "type": "function_call",
                    "status": "completed",
                    "call_id": "call-weather-42",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Paris\"}"
                }],
                "usage": {"input_tokens": 3, "output_tokens": 4, "total_tokens": 7}
            }),
            Self::Messages => json!({
                "id": "msg-tool-mock",
                "type": "message",
                "role": "assistant",
                "model": MODEL,
                "content": [{
                    "type": "tool_use",
                    "id": "call-weather-42",
                    "name": "get_weather",
                    "input": {"city": "Paris"}
                }],
                "stop_reason": "tool_use",
                "stop_sequence": null,
                "usage": {"input_tokens": 3, "output_tokens": 4}
            }),
        }
    }

    fn streaming_response(self) -> String {
        match self {
            Self::ChatCompletions => concat!(
                "data: {\"id\":\"chatcmpl-mock\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"Ornith-1.5-35B-A3B\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"chat stream\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"chatcmpl-mock\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"Ornith-1.5-35B-A3B\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            )
            .to_string(),
            Self::Responses => concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"sequence_number\":0,\"response\":{\"id\":\"resp-mock\",\"object\":\"response\",\"created_at\":0,\"status\":\"in_progress\",\"model\":\"Ornith-1.5-35B-A3B\",\"output\":[]}}\n\n",
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"sequence_number\":1,\"item_id\":\"msg-mock\",\"output_index\":0,\"content_index\":0,\"delta\":\"responses stream\",\"logprobs\":[]}\n\n",
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"sequence_number\":2,\"response\":{\"id\":\"resp-mock\",\"object\":\"response\",\"created_at\":0,\"status\":\"completed\",\"model\":\"Ornith-1.5-35B-A3B\",\"output\":[{\"id\":\"msg-mock\",\"type\":\"message\",\"status\":\"completed\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"responses stream\",\"annotations\":[]}]}]}}\n\n"
            )
            .to_string(),
            Self::Messages => concat!(
                "event: message_start\n",
                "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-mock\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"Ornith-1.5-35B-A3B\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":2,\"output_tokens\":0}}}\n\n",
                "event: content_block_start\n",
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"messages stream\"}}\n\n",
                "event: content_block_stop\n",
                "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "event: message_delta\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":2}}\n\n",
                "event: message_stop\n",
                "data: {\"type\":\"message_stop\"}\n\n"
            )
            .to_string(),
        }
    }

    fn tool_streaming_response(self) -> String {
        match self {
            Self::ChatCompletions => [
                sse_data(json!({
                    "id": "chatcmpl-tool-mock", "object": "chat.completion.chunk", "created": 0, "model": MODEL,
                    "choices": [{"index": 0, "delta": {"role": "assistant", "tool_calls": [{
                        "index": 0, "id": "call-weather-42", "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"city\":"}
                    }]}, "finish_reason": null}]
                })),
                sse_data(json!({
                    "id": "chatcmpl-tool-mock", "object": "chat.completion.chunk", "created": 0, "model": MODEL,
                    "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0,
                        "function": {"arguments": "\"Paris\"}"}
                    }]}, "finish_reason": null}]
                })),
                sse_data(json!({
                    "id": "chatcmpl-tool-mock", "object": "chat.completion.chunk", "created": 0, "model": MODEL,
                    "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
                })),
                "data: [DONE]\n\n".to_string(),
            ]
            .concat(),
            Self::Responses => [
                sse_event("response.created", json!({
                    "type": "response.created", "sequence_number": 0,
                    "response": {"id": "resp-tool-mock", "object": "response", "created_at": 0,
                        "status": "in_progress", "model": MODEL, "output": []}
                })),
                sse_event("response.output_item.added", json!({
                    "type": "response.output_item.added", "sequence_number": 1, "output_index": 0,
                    "item": {"id": "fc-item-42", "type": "function_call", "status": "in_progress",
                        "call_id": "call-weather-42", "name": "get_weather", "arguments": ""}
                })),
                sse_event("response.function_call_arguments.delta", json!({
                    "type": "response.function_call_arguments.delta", "sequence_number": 2,
                    "item_id": "fc-item-42", "output_index": 0, "delta": "{\"city\":"
                })),
                sse_event("response.function_call_arguments.delta", json!({
                    "type": "response.function_call_arguments.delta", "sequence_number": 3,
                    "item_id": "fc-item-42", "output_index": 0, "delta": "\"Paris\"}"
                })),
                sse_event("response.function_call_arguments.done", json!({
                    "type": "response.function_call_arguments.done", "sequence_number": 4,
                    "item_id": "fc-item-42", "output_index": 0, "name": "get_weather",
                    "arguments": "{\"city\":\"Paris\"}"
                })),
                sse_event("response.output_item.done", json!({
                    "type": "response.output_item.done", "sequence_number": 5, "output_index": 0,
                    "item": {"id": "fc-item-42", "type": "function_call", "status": "completed",
                        "call_id": "call-weather-42", "name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}
                })),
                sse_event("response.completed", json!({
                    "type": "response.completed", "sequence_number": 6,
                    "response": {"id": "resp-tool-mock", "object": "response", "created_at": 0,
                        "status": "completed", "model": MODEL, "output": [{"id": "fc-item-42",
                            "type": "function_call", "status": "completed", "call_id": "call-weather-42",
                            "name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}]}
                })),
            ]
            .concat(),
            Self::Messages => [
                sse_event("message_start", json!({
                    "type": "message_start", "message": {"id": "msg-tool-mock", "type": "message",
                        "role": "assistant", "model": MODEL, "content": [], "stop_reason": null,
                        "stop_sequence": null, "usage": {"input_tokens": 3, "output_tokens": 0}}
                })),
                sse_event("content_block_start", json!({
                    "type": "content_block_start", "index": 0,
                    "content_block": {"type": "tool_use", "id": "call-weather-42", "name": "get_weather", "input": {}}
                })),
                sse_event("content_block_delta", json!({
                    "type": "content_block_delta", "index": 0,
                    "delta": {"type": "input_json_delta", "partial_json": "{\"city\":"}
                })),
                sse_event("content_block_delta", json!({
                    "type": "content_block_delta", "index": 0,
                    "delta": {"type": "input_json_delta", "partial_json": "\"Paris\"}"}
                })),
                sse_event("content_block_stop", json!({"type": "content_block_stop", "index": 0})),
                sse_event("message_delta", json!({
                    "type": "message_delta", "delta": {"stop_reason": "tool_use", "stop_sequence": null},
                    "usage": {"output_tokens": 4}
                })),
                sse_event("message_stop", json!({"type": "message_stop"})),
            ]
            .concat(),
        }
    }
}

fn sse_data(value: Value) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(&value).expect("serialize SSE data")
    )
}

fn sse_event(event: &str, value: Value) -> String {
    format!("event: {event}\n{}", sse_data(value))
}

#[derive(Clone)]
struct MockState {
    response_content_type: &'static str,
    response_body: String,
    captured: Arc<Mutex<Option<CapturedRequest>>>,
}

#[derive(Clone, Debug)]
struct CapturedRequest {
    method: String,
    path_and_query: String,
    headers: HashMap<String, String>,
    body: Value,
}

async fn mock_model(State(state): State<MockState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, 1024 * 1024)
        .await
        .expect("read mock model request body");
    let headers = parts
        .headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect();
    let captured = CapturedRequest {
        method: parts.method.to_string(),
        path_and_query: parts
            .uri
            .path_and_query()
            .map(|value| value.as_str())
            .unwrap_or(parts.uri.path())
            .to_string(),
        headers,
        body: serde_json::from_slice(&body).expect("mock model request is JSON"),
    };
    *state.captured.lock().expect("lock captured request") = Some(captured);

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", state.response_content_type)
        .header("x-model-request-id", MODEL_REQUEST_ID)
        .body(Body::from(state.response_body))
        .expect("build mock model response")
}

async fn spawn_mock_model(
    response_content_type: &'static str,
    response_body: String,
) -> (
    String,
    Arc<Mutex<Option<CapturedRequest>>>,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock model service");
    let address = listener.local_addr().expect("mock model address");
    let captured = Arc::new(Mutex::new(None));
    let app = Router::new()
        .fallback(post(mock_model))
        .with_state(MockState {
            response_content_type,
            response_body,
            captured: Arc::clone(&captured),
        });
    let (stop_tx, stop_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await
            .expect("serve mock model service");
    });
    (format!("http://{address}"), captured, stop_tx, task)
}

async fn spawn_gateway(
    api: Api,
    mock_base: &str,
) -> (
    String,
    tempfile::TempDir,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let gateway_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind Gateway");
    let gateway_address = gateway_listener.local_addr().expect("Gateway address");
    let admin_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind Gateway admin API");
    let admin_address = admin_listener.local_addr().expect("Gateway admin address");

    let (provider, upstream, upstream_anthropic) = match api {
        Api::ChatCompletions => ("openai", format!("{mock_base}/v1"), None),
        Api::Responses => {
            // Native Responses routing recognizes official OpenAI endpoints. A URL fragment
            // supplies that marker for this loopback mock and, by HTTP definition, is never
            // included in the request sent to the model service.
            ("openai", format!("{mock_base}/v1#api.openai.com"), None)
        }
        Api::Messages => (
            "anthropic",
            format!("{mock_base}/v1"),
            Some(format!("{mock_base}/v1")),
        ),
    };
    let upstream_anthropic = upstream_anthropic
        .map(|value| format!("upstream_anthropic = {value:?}\n"))
        .unwrap_or_default();
    let config = ProxyConfig::from_toml_str(&format!(
        r#"
listen = "{gateway_address}"
admin_listen = "{admin_address}"
agent_id = "model-api-forwarding-test"
capture_level = "summary"

[[models]]
name = "*"
provider = "{provider}"
upstream = {upstream:?}
{upstream_anthropic}api_key = "{UPSTREAM_KEY}"
"#
    ))
    .expect("Gateway config");
    let storage = tempfile::tempdir().expect("Gateway state directory");
    let storage_path = storage.path().to_path_buf();
    let sink: Arc<dyn persisting_gateway::sink::CaptureEventSink> = Arc::new(SeqOnlySink::new());
    let (stop_tx, stop_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        serve_with_listeners_and_shutdown(
            config,
            storage_path,
            sink,
            false,
            gateway_listener,
            admin_listener,
            async {
                let _ = stop_rx.await;
            },
        )
        .await
    });
    tokio::task::yield_now().await;

    (format!("http://{gateway_address}"), storage, stop_tx, task)
}

async fn assert_round_trip(api: Api, stream: bool, tool_call: bool) {
    let expected_request = api.request(stream, tool_call);
    let (response_content_type, expected_response) = if stream {
        (
            "text/event-stream",
            if tool_call {
                api.tool_streaming_response()
            } else {
                api.streaming_response()
            },
        )
    } else {
        (
            "application/json",
            serde_json::to_string(&if tool_call {
                api.tool_non_streaming_response()
            } else {
                api.non_streaming_response()
            })
            .expect("serialize response"),
        )
    };
    let (mock_base, captured, mock_stop, mock_task) =
        spawn_mock_model(response_content_type, expected_response.clone()).await;
    let (gateway_base, _storage, gateway_stop, gateway_task) = spawn_gateway(api, &mock_base).await;

    let mut request = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("HTTP client")
        .post(format!("{gateway_base}{}", api.path()))
        .header("content-type", "application/json")
        .header("x-client-trace", CLIENT_TRACE)
        .body(serde_json::to_vec(&expected_request).expect("serialize request"));
    request = match api {
        Api::Messages => request
            .header("x-api-key", "client-key-must-not-reach-upstream")
            .header("anthropic-version", "2023-06-01"),
        Api::ChatCompletions | Api::Responses => {
            request.header("authorization", "Bearer client-key-must-not-reach-upstream")
        }
    };
    let response = request.send().await.expect("request Gateway");
    let status = response.status();
    let headers = response.headers().clone();
    let actual_response = response.text().await.expect("read Gateway response");

    let captured = captured
        .lock()
        .expect("lock captured request")
        .clone()
        .unwrap_or_else(|| panic!("{} mock received no request", api.name()));

    let _ = gateway_stop.send(());
    gateway_task
        .await
        .expect("join Gateway task")
        .expect("Gateway shutdown");
    let _ = mock_stop.send(());
    mock_task.await.expect("join mock model task");

    assert_eq!(status, StatusCode::OK, "{} status", api.name());
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some(response_content_type),
        "{} response content type",
        api.name()
    );
    assert_eq!(
        headers
            .get("x-model-request-id")
            .and_then(|value| value.to_str().ok()),
        Some(MODEL_REQUEST_ID),
        "{} response header",
        api.name()
    );
    assert_eq!(
        actual_response,
        expected_response,
        "{} response body",
        api.name()
    );

    assert_eq!(captured.method, "POST", "{} method", api.name());
    assert_eq!(
        captured.path_and_query,
        api.path(),
        "{} upstream path",
        api.name()
    );
    assert_eq!(
        captured.body,
        expected_request,
        "{} upstream JSON body",
        api.name()
    );
    if tool_call {
        assert_eq!(
            captured.body["tools"][0]["type"],
            if matches!(api, Api::Messages) {
                Value::Null
            } else {
                Value::String("function".into())
            },
            "{} tool type",
            api.name()
        );
        assert_eq!(
            captured.body["tools"][0]["name"],
            if matches!(api, Api::ChatCompletions) {
                Value::Null
            } else {
                Value::String("get_weather".into())
            },
            "{} tool name",
            api.name()
        );
        let function_name = captured.body["tools"][0]
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str);
        assert_eq!(
            function_name,
            matches!(api, Api::ChatCompletions).then_some("get_weather"),
            "{} function tool definition",
            api.name()
        );
    }
    assert_eq!(
        captured.headers.get("x-client-trace").map(String::as_str),
        Some(CLIENT_TRACE),
        "{} forwarded client header",
        api.name()
    );
    match api {
        Api::Messages => {
            assert_eq!(
                captured.headers.get("x-api-key").map(String::as_str),
                Some(UPSTREAM_KEY)
            );
            assert_eq!(
                captured
                    .headers
                    .get("anthropic-version")
                    .map(String::as_str),
                Some("2023-06-01")
            );
            assert!(!captured.headers.contains_key("authorization"));
        }
        Api::ChatCompletions | Api::Responses => {
            assert_eq!(
                captured.headers.get("authorization").map(String::as_str),
                Some("Bearer upstream-test-key")
            );
            assert!(!captured.headers.contains_key("x-api-key"));
        }
    }
}

#[tokio::test]
async fn chat_completions_non_streaming_round_trip() {
    assert_round_trip(Api::ChatCompletions, false, false).await;
}

#[tokio::test]
async fn chat_completions_sse_round_trip() {
    assert_round_trip(Api::ChatCompletions, true, false).await;
}

#[tokio::test]
async fn responses_non_streaming_round_trip() {
    assert_round_trip(Api::Responses, false, false).await;
}

#[tokio::test]
async fn responses_sse_round_trip() {
    assert_round_trip(Api::Responses, true, false).await;
}

#[tokio::test]
async fn messages_non_streaming_round_trip() {
    assert_round_trip(Api::Messages, false, false).await;
}

#[tokio::test]
async fn messages_sse_round_trip() {
    assert_round_trip(Api::Messages, true, false).await;
}

#[tokio::test]
async fn chat_completions_tool_call_non_streaming_round_trip() {
    assert_round_trip(Api::ChatCompletions, false, true).await;
}

#[tokio::test]
async fn chat_completions_tool_call_sse_round_trip() {
    assert_round_trip(Api::ChatCompletions, true, true).await;
}

#[tokio::test]
async fn responses_tool_call_non_streaming_round_trip() {
    assert_round_trip(Api::Responses, false, true).await;
}

#[tokio::test]
async fn responses_tool_call_sse_round_trip() {
    assert_round_trip(Api::Responses, true, true).await;
}

#[tokio::test]
async fn messages_tool_call_non_streaming_round_trip() {
    assert_round_trip(Api::Messages, false, true).await;
}

#[tokio::test]
async fn messages_tool_call_sse_round_trip() {
    assert_round_trip(Api::Messages, true, true).await;
}
