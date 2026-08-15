//! Deterministic local LLM upstream for forwarding and conversion tests.

use std::fmt;
use std::future::Future;

use anyhow::{bail, Context, Result};
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};

pub const ECHO_ENCODING_HEADER: &str = "x-persisting-echo-encoding";
pub const ECHO_MODE_HEADER: &str = "x-persisting-echo-mode";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum EchoMode {
    #[default]
    Text,
    Inspect,
    Tool,
    Reasoning,
    Error,
}

impl EchoMode {
    fn from_headers(headers: &HeaderMap) -> Self {
        match headers
            .get(ECHO_MODE_HEADER)
            .and_then(|value| value.to_str().ok())
        {
            Some("inspect") => Self::Inspect,
            Some("tool") => Self::Tool,
            Some("reasoning") => Self::Reasoning,
            Some("error") => Self::Error,
            _ => Self::Text,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EchoEncoding {
    #[default]
    Plain,
    Base64,
}

impl EchoEncoding {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "plain" | "text" | "direct" => Ok(Self::Plain),
            "base64" => Ok(Self::Base64),
            _ => bail!("unsupported echo encoding '{value}' (expected plain or base64)"),
        }
    }

    fn encode(self, input: &str) -> String {
        match self {
            Self::Plain => input.to_string(),
            Self::Base64 => STANDARD.encode(input.as_bytes()),
        }
    }
}

impl fmt::Display for EchoEncoding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Plain => "plain",
            Self::Base64 => "base64",
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EchoServerConfig {
    pub default_encoding: EchoEncoding,
}

pub fn router(config: EchoServerConfig) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/echo", post(raw_echo))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/messages", post(messages))
        .route("/v1/responses", post(responses))
        .route("/v1beta/models/{model_and_method}", post(gemini_generate))
        .with_state(config)
}

pub async fn serve(listener: tokio::net::TcpListener, config: EchoServerConfig) -> Result<()> {
    axum::serve(listener, router(config))
        .await
        .context("serve Gateway echo upstream")
}

pub async fn serve_with_shutdown(
    listener: tokio::net::TcpListener,
    config: EchoServerConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    axum::serve(listener, router(config))
        .with_graceful_shutdown(shutdown)
        .await
        .context("serve Gateway echo upstream")
}

async fn health() -> Json<Value> {
    Json(json!({"status": "ok", "service": "pchronicle-echo"}))
}

async fn raw_echo(
    State(config): State<EchoServerConfig>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let encoding = match request_encoding(&headers, config) {
        Ok(encoding) => encoding,
        Err(error) => return error_response(&error),
    };
    encoded_response(
        StatusCode::OK,
        "text/plain; charset=utf-8",
        encoding,
        encoding.encode(&String::from_utf8_lossy(&body)),
    )
}

async fn chat_completions(
    State(config): State<EchoServerConfig>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let mode = EchoMode::from_headers(&headers);
    if mode == EchoMode::Error {
        return controlled_error_response();
    }
    let encoding = match request_encoding(&headers, config) {
        Ok(encoding) => encoding,
        Err(error) => return error_response(&error),
    };
    let content = encoding.encode(&match mode {
        EchoMode::Inspect => inspect_chat_input(&request),
        _ => extract_chat_input(&request),
    });
    let model = request_model(&request);
    if mode == EchoMode::Tool {
        if request_streams(&request) {
            return sse_response(encoding, chat_tool_stream(&model));
        }
        return json_response(encoding, chat_tool_body(&model));
    }
    if mode == EchoMode::Reasoning {
        if request_streams(&request) {
            return sse_response(encoding, chat_reasoning_stream(&model, &content));
        }
        return json_response(encoding, chat_reasoning_body(&model, &content));
    }
    if request_streams(&request) {
        return sse_response(encoding, chat_stream(&model, &content));
    }
    json_response(
        encoding,
        json!({
            "id": "chatcmpl-echo",
            "object": "chat.completion",
            "created": 0,
            "model": model,
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": content},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
        }),
    )
}

async fn messages(
    State(config): State<EchoServerConfig>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let encoding = match request_encoding(&headers, config) {
        Ok(encoding) => encoding,
        Err(error) => return error_response(&error),
    };
    let content = encoding.encode(&extract_chat_input(&request));
    let model = request_model(&request);
    if request_streams(&request) {
        return sse_response(encoding, messages_stream(&model, &content));
    }
    json_response(
        encoding,
        json!({
            "id": "msg_echo",
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [{"type": "text", "text": content}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 0, "output_tokens": 0}
        }),
    )
}

async fn responses(
    State(config): State<EchoServerConfig>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let encoding = match request_encoding(&headers, config) {
        Ok(encoding) => encoding,
        Err(error) => return error_response(&error),
    };
    let content = encoding.encode(&extract_responses_input(&request));
    let model = request_model(&request);
    if request_streams(&request) {
        return sse_response(encoding, responses_stream(&model, &content));
    }
    json_response(encoding, responses_body(&model, &content))
}

async fn gemini_generate(
    State(config): State<EchoServerConfig>,
    Path(model_and_method): Path<String>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let encoding = match request_encoding(&headers, config) {
        Ok(encoding) => encoding,
        Err(error) => return error_response(&error),
    };
    let (model, method) = model_and_method
        .split_once(':')
        .unwrap_or((&model_and_method, "generateContent"));
    let content = encoding.encode(&extract_gemini_input(&request));
    let body = gemini_body(model, &content);
    if method == "streamGenerateContent" {
        let event = serde_json::to_string(&body).expect("serialize Gemini echo event");
        return sse_response(encoding, format!("data: {event}\n\n"));
    }
    json_response(encoding, body)
}

fn request_encoding(
    headers: &HeaderMap,
    config: EchoServerConfig,
) -> std::result::Result<EchoEncoding, String> {
    let Some(value) = headers.get(ECHO_ENCODING_HEADER) else {
        return Ok(config.default_encoding);
    };
    let value = match value.to_str() {
        Ok(value) => value,
        Err(_) => return Err("echo encoding header is not valid UTF-8".to_string()),
    };
    EchoEncoding::parse(value).map_err(|error| error.to_string())
}

fn request_model(request: &Value) -> String {
    request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("echo-model")
        .to_string()
}

fn request_streams(request: &Value) -> bool {
    request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn extract_chat_input(request: &Value) -> String {
    request
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages.iter().rev().find_map(|message| {
                (message.get("role").and_then(Value::as_str) == Some("user"))
                    .then(|| text_content(message.get("content")))
            })
        })
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| extract_responses_input(request))
}

fn extract_responses_input(request: &Value) -> String {
    let Some(input) = request.get("input") else {
        return String::new();
    };
    match input {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .rev()
            .find_map(|item| {
                let role_is_user = item.get("role").and_then(Value::as_str) == Some("user");
                role_is_user.then(|| text_content(item.get("content")))
            })
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| text_content(Some(input))),
        _ => text_content(Some(input)),
    }
}

fn extract_gemini_input(request: &Value) -> String {
    request
        .get("contents")
        .and_then(Value::as_array)
        .and_then(|contents| {
            contents.iter().rev().find_map(|content| {
                let role = content.get("role").and_then(Value::as_str);
                matches!(role, Some("user") | None).then(|| text_content(content.get("parts")))
            })
        })
        .unwrap_or_default()
}

fn text_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| match part {
                Value::String(text) => Some(text.as_str()),
                Value::Object(map) => map
                    .get("text")
                    .and_then(Value::as_str)
                    .or_else(|| map.get("input_text").and_then(Value::as_str)),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        Some(Value::Object(map)) => map
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

fn inspect_chat_input(request: &Value) -> String {
    let content = request
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages
                .iter()
                .rev()
                .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        })
        .and_then(|message| message.get("content"));
    let Some(Value::Array(parts)) = content else {
        return "text=0 image=0".to_string();
    };
    let text = parts
        .iter()
        .filter(|part| {
            matches!(
                part.get("type").and_then(Value::as_str),
                Some("text" | "input_text")
            )
        })
        .count();
    let image = parts
        .iter()
        .filter(|part| {
            matches!(
                part.get("type").and_then(Value::as_str),
                Some("image_url" | "input_image" | "image")
            )
        })
        .count();
    format!("text={text} image={image}")
}

fn chat_stream(model: &str, content: &str) -> String {
    let start = json!({
        "id": "chatcmpl-echo",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": model,
        "choices": [{"index": 0, "delta": {"role": "assistant", "content": content}, "finish_reason": null}]
    });
    let stop = json!({
        "id": "chatcmpl-echo",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": model,
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
    });
    format!("data: {start}\n\ndata: {stop}\n\ndata: [DONE]\n\n")
}

fn chat_tool_body(model: &str) -> Value {
    json!({
        "id": "chatcmpl-echo-tool",
        "object": "chat.completion",
        "created": 0,
        "model": model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_echo_weather",
                    "type": "function",
                    "function": {"name": "weather", "arguments": "{\"city\":\"Paris\"}"}
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
    })
}

fn chat_tool_stream(model: &str) -> String {
    let start = json!({
        "id":"chatcmpl-echo-tool","object":"chat.completion.chunk","created":0,"model":model,
        "choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{
            "index":0,"id":"call_echo_weather","type":"function",
            "function":{"name":"weather","arguments":"{\"city\":\"Paris\"}"}
        }]},"finish_reason":null}]
    });
    let stop = json!({
        "id":"chatcmpl-echo-tool","object":"chat.completion.chunk","created":0,"model":model,
        "choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]
    });
    format!("data: {start}\n\ndata: {stop}\n\ndata: [DONE]\n\n")
}

fn chat_reasoning_body(model: &str, content: &str) -> Value {
    json!({
        "id":"chatcmpl-echo-reasoning","object":"chat.completion","created":0,"model":model,
        "choices":[{"index":0,"message":{
            "role":"assistant","reasoning_content":"echo-reasoning","content":content
        },"finish_reason":"stop"}],
        "usage":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}
    })
}

fn chat_reasoning_stream(model: &str, content: &str) -> String {
    let reasoning = json!({
        "id":"chatcmpl-echo-reasoning","object":"chat.completion.chunk","created":0,"model":model,
        "choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"echo-reasoning"},"finish_reason":null}]
    });
    let text = json!({
        "id":"chatcmpl-echo-reasoning","object":"chat.completion.chunk","created":0,"model":model,
        "choices":[{"index":0,"delta":{"content":content},"finish_reason":null}]
    });
    let stop = json!({
        "id":"chatcmpl-echo-reasoning","object":"chat.completion.chunk","created":0,"model":model,
        "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]
    });
    format!("data: {reasoning}\n\ndata: {text}\n\ndata: {stop}\n\ndata: [DONE]\n\n")
}

fn messages_stream(model: &str, content: &str) -> String {
    let events = [
        (
            "message_start",
            json!({"type":"message_start","message":{"id":"msg_echo","type":"message","role":"assistant","model":model,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}}),
        ),
        (
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        ),
        (
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":content}}),
        ),
        (
            "content_block_stop",
            json!({"type":"content_block_stop","index":0}),
        ),
        (
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":0}}),
        ),
        ("message_stop", json!({"type":"message_stop"})),
    ];
    events
        .into_iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect()
}

fn responses_body(model: &str, content: &str) -> Value {
    json!({
        "id": "resp_echo",
        "object": "response",
        "created_at": 0,
        "status": "completed",
        "model": model,
        "output": [{
            "id": "msg_echo",
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{"type": "output_text", "text": content, "annotations": []}]
        }],
        "usage": {"input_tokens": 0, "output_tokens": 0, "total_tokens": 0}
    })
}

fn responses_stream(model: &str, content: &str) -> String {
    let complete = responses_body(model, content);
    let events = [
        json!({"type":"response.created","response":{"id":"resp_echo","object":"response","created_at":0,"status":"in_progress","model":model,"output":[]}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"id":"msg_echo","type":"message","status":"in_progress","role":"assistant","content":[]}}),
        json!({"type":"response.content_part.added","item_id":"msg_echo","output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}),
        json!({"type":"response.output_text.delta","item_id":"msg_echo","output_index":0,"content_index":0,"delta":content,"logprobs":[]}),
        json!({"type":"response.output_text.done","item_id":"msg_echo","output_index":0,"content_index":0,"text":content,"logprobs":[]}),
        json!({"type":"response.completed","response":complete}),
    ];
    events
        .into_iter()
        .enumerate()
        .map(|(sequence_number, mut event)| {
            event["sequence_number"] = json!(sequence_number);
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap_or("message")
            )
        })
        .collect()
}

fn gemini_body(model: &str, content: &str) -> Value {
    json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": content}]},
            "finishReason": "STOP",
            "index": 0
        }],
        "usageMetadata": {"promptTokenCount": 0, "candidatesTokenCount": 0, "totalTokenCount": 0},
        "modelVersion": model
    })
}

fn json_response(encoding: EchoEncoding, body: Value) -> Response {
    encoded_response(
        StatusCode::OK,
        "application/json",
        encoding,
        serde_json::to_string(&body).expect("serialize echo response"),
    )
}

fn sse_response(encoding: EchoEncoding, body: String) -> Response {
    encoded_response(StatusCode::OK, "text/event-stream", encoding, body)
}

fn encoded_response(
    status: StatusCode,
    content_type: &'static str,
    encoding: EchoEncoding,
    body: String,
) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static(content_type),
    );
    response.headers_mut().insert(
        HeaderName::from_static(ECHO_ENCODING_HEADER),
        HeaderValue::from_str(&encoding.to_string()).expect("valid echo encoding header"),
    );
    response
}

fn error_response(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": {"message": message, "type": "echo_request_error"}})),
    )
        .into_response()
}

fn controlled_error_response() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "error": {
                "message": "controlled echo failure",
                "type": "echo_controlled_error",
                "code": "echo_rate_limit"
            }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    async fn spawn_echo() -> (String, tokio::sync::oneshot::Sender<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            serve_with_shutdown(listener, EchoServerConfig::default(), async {
                let _ = stop_rx.await;
            })
            .await
            .unwrap();
        });
        (format!("http://{address}"), stop_tx)
    }

    #[tokio::test]
    async fn raw_echo_supports_plain_and_base64() {
        let (base, stop) = spawn_echo().await;
        let client = reqwest::Client::new();
        let plain = client
            .post(format!("{base}/echo"))
            .body("hello")
            .send()
            .await
            .unwrap();
        assert_eq!(plain.text().await.unwrap(), "hello");

        let encoded = client
            .post(format!("{base}/echo"))
            .header(ECHO_ENCODING_HEADER, "base64")
            .body("hello")
            .send()
            .await
            .unwrap();
        assert_eq!(encoded.text().await.unwrap(), "aGVsbG8=");
        let _ = stop.send(());
    }

    #[tokio::test]
    async fn chat_echo_uses_last_user_message_and_streams() {
        let (base, stop) = spawn_echo().await;
        let client = reqwest::Client::new();
        let response: Value = client
            .post(format!("{base}/v1/chat/completions"))
            .header(ECHO_ENCODING_HEADER, "base64")
            .json(&json!({
                "model": "echo-test",
                "messages": [
                    {"role":"user","content":"first"},
                    {"role":"assistant","content":"ignored"},
                    {"role":"user","content":"最后"}
                ]
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            response["choices"][0]["message"]["content"],
            STANDARD.encode("最后".as_bytes())
        );

        let stream = client
            .post(format!("{base}/v1/chat/completions"))
            .json(&json!({"model":"echo-test","stream":true,"messages":[{"role":"user","content":"chunk"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            stream.headers()[axum::http::header::CONTENT_TYPE],
            "text/event-stream"
        );
        let stream = stream.text().await.unwrap();
        assert!(stream.contains("chunk"));
        assert!(stream.contains("[DONE]"));
        let _ = stop.send(());
    }

    #[tokio::test]
    async fn native_protocol_endpoints_return_their_wire_shapes() {
        let (base, stop) = spawn_echo().await;
        let client = reqwest::Client::new();

        let messages: Value = client
            .post(format!("{base}/v1/messages"))
            .json(&json!({"model":"m","messages":[{"role":"user","content":"message"}]}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(messages["content"][0]["text"], "message");

        let responses: Value = client
            .post(format!("{base}/v1/responses"))
            .json(&json!({"model":"m","input":"response"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(responses["output"][0]["content"][0]["text"], "response");

        let gemini: Value = client
            .post(format!("{base}/v1beta/models/gemini-test:generateContent"))
            .json(&json!({"contents":[{"role":"user","parts":[{"text":"gemini"}]}]}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            gemini["candidates"][0]["content"]["parts"][0]["text"],
            "gemini"
        );
        let _ = stop.send(());
    }

    #[tokio::test]
    async fn pchronicle_echo_exercises_forward_rewrite_bridge_and_capture() {
        let (echo_base, echo_stop) = spawn_echo().await;
        let gateway_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway_address = gateway_listener.local_addr().unwrap();
        let admin_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let admin_address = admin_listener.local_addr().unwrap();
        let config = crate::config::ProxyConfig::from_toml_str(&format!(
            r#"
listen = "{gateway_address}"
admin_listen = "{admin_address}"
agent_id = "echo-test"
capture_level = "dialogue"

[[models]]
name = "echo-upstream"
provider = "openai"
upstream = "{echo_base}/v1"

[[models]]
name = "*"
forward = "echo-upstream"
"#
        ))
        .unwrap();
        let records = Arc::new(Mutex::new(Vec::new()));
        let callback_records = Arc::clone(&records);
        let sink: Arc<dyn crate::sink::CaptureEventSink> = Arc::new(
            crate::sink::CallbackSink::new("echo-test", move |_, _, record| {
                callback_records.lock().unwrap().push(record);
                Ok(())
            }),
        );
        let state = tempfile::tempdir().unwrap();
        let (gateway_stop, gateway_stop_rx) = tokio::sync::oneshot::channel();
        let gateway = tokio::spawn(crate::serve_with_listeners_and_shutdown(
            config,
            state.path().to_path_buf(),
            sink,
            false,
            gateway_listener,
            admin_listener,
            async {
                let _ = gateway_stop_rx.await;
            },
        ));

        let response = reqwest::Client::new()
            .post(format!("http://{gateway_address}/v1/messages"))
            .header(ECHO_ENCODING_HEADER, "base64")
            .json(&json!({
                "model": "client-alias",
                "max_tokens": 32,
                "messages": [{"role":"user","content":"bridged"}]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let response: Value = response.json().await.unwrap();
        assert_eq!(response["type"], "message");
        assert_eq!(response["model"], "client-alias");
        assert_eq!(response["content"][0]["text"], "YnJpZGdlZA==");

        let _ = gateway_stop.send(());
        gateway.await.unwrap().unwrap();
        let _ = echo_stop.send(());

        let records = records.lock().unwrap();
        assert!(records.iter().any(|record| {
            record.kind == "llm.request"
                && record.payload["model"] == "client-alias"
                && record.payload["forward_to"] == "echo-upstream"
        }));
        assert!(records.iter().any(|record| {
            record.kind == "llm.response"
                && record.payload["assistant_content"] == "YnJpZGdlZA=="
                && record.payload["forward_to"] == "echo-upstream"
        }));
    }
}
