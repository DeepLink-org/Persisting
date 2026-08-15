//! OpenAI Chat Completions ↔ Gemini native `generateContent` wire format.
//!
//! Adapted from agentgateway's `conversion::vertex_gemini` contract. This module intentionally
//! contains no Google SDK or control-plane dependency; Gateway routing, auth, capture and WAL stay
//! outside the protocol adapter.

use std::collections::HashMap;

use anyhow::{Context, Result};
use bytes::Bytes;
use serde_json::{json, Map, Value};

const THOUGHT_SIGNATURE_SEPARATOR: &str = "__thought__";

pub fn completions_request_to_gemini(body: &Bytes, model: &str) -> Result<Bytes> {
    let request: Value = serde_json::from_slice(body).context("parse completions request")?;
    let object = request
        .as_object()
        .context("completions request must be a JSON object")?;
    let messages = object
        .get("messages")
        .and_then(Value::as_array)
        .context("completions request missing messages")?;

    let (system, mut contents) = messages_to_contents(messages)?;
    if contents.is_empty() {
        contents.push(json!({"role":"user", "parts":[{"text":" "}]}));
    }

    let mut output = json!({"contents": contents});
    if !system.is_empty() {
        output["systemInstruction"] = json!({
            "parts": [{"text": system.join("\n")}]
        });
    }
    if let Some(tools) = build_tools(object) {
        output["tools"] = tools;
    }
    if let Some(tool_config) = build_tool_config(object) {
        output["toolConfig"] = tool_config;
    }
    if let Some(generation_config) = build_generation_config(object, model) {
        output["generationConfig"] = generation_config;
    }
    for (source, target) in [
        ("cachedContent", "cachedContent"),
        ("cached_content", "cachedContent"),
        ("safetySettings", "safetySettings"),
        ("safety_settings", "safetySettings"),
        ("labels", "labels"),
    ] {
        if output.get(target).is_none() {
            if let Some(value) = object.get(source).filter(|value| !value.is_null()) {
                output[target] = value.clone();
            }
        }
    }
    // Gemini cached content already owns system instructions and tools.
    if output.get("cachedContent").is_some() {
        output
            .as_object_mut()
            .expect("object")
            .remove("systemInstruction");
        output.as_object_mut().expect("object").remove("tools");
        output.as_object_mut().expect("object").remove("toolConfig");
    }

    Ok(Bytes::from(serde_json::to_vec(&output)?))
}

fn messages_to_contents(messages: &[Value]) -> Result<(Vec<String>, Vec<Value>)> {
    let mut system = Vec::new();
    let mut contents = Vec::new();
    let mut calls: HashMap<String, (String, usize)> = HashMap::new();

    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let content = message.get("content").unwrap_or(&Value::Null);
        match role {
            "system" | "developer" => {
                if let Some(text) = content_text(content).filter(|text| !text.is_empty()) {
                    system.push(text);
                }
            }
            "user" => push_content(&mut contents, "user", user_parts(content)?),
            "assistant" => {
                let mut parts = assistant_text_parts(content);
                if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for (index, call) in tool_calls.iter().enumerate() {
                        if let (Some(id), Some(name)) = (
                            call.get("id").and_then(Value::as_str),
                            call.get("function")
                                .and_then(|function| function.get("name"))
                                .and_then(Value::as_str),
                        ) {
                            calls.insert(
                                split_tool_call_id(id).0.to_string(),
                                (name.to_string(), index),
                            );
                        }
                        parts.push(function_call_part(call));
                    }
                }
                push_content(&mut contents, "model", parts);
            }
            "tool" | "function" => {
                let raw_id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let base_id = split_tool_call_id(raw_id).0;
                let name = calls
                    .get(base_id)
                    .map(|(name, _)| name.as_str())
                    .or_else(|| message.get("name").and_then(Value::as_str))
                    .unwrap_or_default();
                let response = content_text(content)
                    .map(|text| json!({"content": text}))
                    .unwrap_or_else(|| json!({}));
                let mut part = json!({
                    "functionResponse": {
                        "name": name,
                        "response": response
                    }
                });
                if !base_id.is_empty() {
                    // Transient correlation key. Gemini rejects this field, so the ordering pass
                    // below removes it before serialization.
                    part["functionResponse"]["id"] = json!(base_id);
                }
                push_content(&mut contents, "user", vec![part]);
            }
            _ => {}
        }
    }

    // Gemini correlates parallel function responses positionally.
    for content in &mut contents {
        let Some(parts) = content.get_mut("parts").and_then(Value::as_array_mut) else {
            continue;
        };
        if !parts
            .iter()
            .any(|part| part.get("functionResponse").is_some())
        {
            continue;
        }
        parts.sort_by_key(|part| {
            part.get("functionResponse")
                .and_then(|response| response.get("id"))
                .and_then(Value::as_str)
                .and_then(|id| calls.get(id))
                .map(|(_, index)| *index)
                .unwrap_or(usize::MAX)
        });
        for part in parts {
            if let Some(response) = part
                .get_mut("functionResponse")
                .and_then(Value::as_object_mut)
            {
                response.remove("id");
            }
        }
    }
    Ok((system, contents))
}

fn push_content(contents: &mut Vec<Value>, role: &str, mut parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    let function_response = parts
        .iter()
        .any(|part| part.get("functionResponse").is_some());
    let can_merge = contents.last().is_some_and(|last| {
        last.get("role").and_then(Value::as_str) == Some(role)
            && last
                .get("parts")
                .and_then(Value::as_array)
                .is_some_and(|last_parts| {
                    last_parts
                        .iter()
                        .any(|part| part.get("functionResponse").is_some())
                        == function_response
                })
    });
    if can_merge {
        if role == "user"
            && !function_response
            && !contents
                .last()
                .and_then(|last| last.get("parts"))
                .and_then(Value::as_array)
                .is_some_and(|parts| parts.iter().any(|part| part.get("text").is_some()))
            && !parts.iter().any(|part| part.get("text").is_some())
        {
            parts.push(json!({"text":" "}));
        }
        contents
            .last_mut()
            .and_then(|last| last.get_mut("parts"))
            .and_then(Value::as_array_mut)
            .expect("checked above")
            .extend(parts);
    } else {
        if role == "user"
            && !function_response
            && !parts.iter().any(|part| part.get("text").is_some())
        {
            parts.push(json!({"text":" "}));
        }
        contents.push(json!({"role": role, "parts": parts}));
    }
}

fn content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<String>(),
        ),
        _ => None,
    }
}

fn assistant_text_parts(content: &Value) -> Vec<Value> {
    match content {
        Value::String(text) if !text.is_empty() => vec![json!({"text":text})],
        Value::Array(parts) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .map(|text| json!({"text":text}))
            .collect(),
        _ => Vec::new(),
    }
}

fn user_parts(content: &Value) -> Result<Vec<Value>> {
    match content {
        Value::String(text) => Ok(vec![json!({"text":text})]),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part.get("type").and_then(Value::as_str) {
                Some("text") => part
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| Ok(json!({"text":text}))),
                Some("image_url") => Some(image_part(part.get("image_url"))),
                _ => None,
            })
            .collect(),
        _ => Ok(Vec::new()),
    }
}

fn image_part(image_url: Option<&Value>) -> Result<Value> {
    let image_url = image_url.context("image_url content part missing image_url")?;
    let url = image_url
        .get("url")
        .and_then(Value::as_str)
        .context("image_url missing url")?;
    if let Some(raw) = url.strip_prefix("data:") {
        let (meta, data) = raw.split_once(',').context("invalid image data URL")?;
        let (mime, encoding) = meta.split_once(';').context("invalid image data URL")?;
        anyhow::ensure!(
            encoding.eq_ignore_ascii_case("base64"),
            "image data URL must be base64"
        );
        let mime = if mime == "image/jpg" {
            "image/jpeg"
        } else {
            mime
        };
        return Ok(json!({"inlineData":{"mimeType":mime,"data":data}}));
    }
    anyhow::ensure!(
        url.starts_with("gs://"),
        "native Gemini accepts data: or gs:// media URLs"
    );
    let mime = image_url
        .get("format")
        .or_else(|| image_url.get("mime_type"))
        .or_else(|| image_url.get("content_type"))
        .and_then(Value::as_str)
        .and_then(mime_hint)
        .or_else(|| url.rsplit_once('.').and_then(|(_, ext)| mime_hint(ext)))
        .context("gs:// media URL needs a recognized extension or MIME hint")?;
    Ok(json!({"fileData":{"mimeType":mime,"fileUri":url}}))
}

fn mime_hint(value: &str) -> Option<&'static str> {
    Some(match value.to_ascii_lowercase().as_str() {
        "image/png" | "png" => "image/png",
        "image/jpeg" | "image/jpg" | "jpg" | "jpeg" => "image/jpeg",
        "image/webp" | "webp" => "image/webp",
        "image/gif" | "gif" => "image/gif",
        "image/heic" | "heic" => "image/heic",
        "image/heif" | "heif" => "image/heif",
        "application/pdf" | "pdf" => "application/pdf",
        "audio/mpeg" | "mp3" => "audio/mpeg",
        "audio/wav" | "wav" => "audio/wav",
        "video/mp4" | "mp4" => "video/mp4",
        "video/quicktime" | "mov" => "video/quicktime",
        "video/webm" | "webm" => "video/webm",
        "text/plain" | "txt" => "text/plain",
        _ => return None,
    })
}

fn function_call_part(call: &Value) -> Value {
    let function = call.get("function").unwrap_or(&Value::Null);
    let args = function
        .get("arguments")
        .and_then(Value::as_str)
        .and_then(|arguments| serde_json::from_str(arguments).ok())
        .unwrap_or_else(|| json!({}));
    let mut part = json!({
        "functionCall": {
            "name": function.get("name").and_then(Value::as_str).unwrap_or_default(),
            "args": args
        }
    });
    if let Some(signature) = call
        .get("id")
        .and_then(Value::as_str)
        .and_then(|id| split_tool_call_id(id).1)
    {
        part["thoughtSignature"] = json!(signature);
    }
    part
}

fn split_tool_call_id(raw: &str) -> (&str, Option<&str>) {
    match raw.split_once(THOUGHT_SIGNATURE_SEPARATOR) {
        Some((base, signature)) if !signature.is_empty() => (base, Some(signature)),
        _ => (raw, None),
    }
}

fn build_tools(object: &Map<String, Value>) -> Option<Value> {
    let declarations = object
        .get("tools")?
        .as_array()?
        .iter()
        .filter_map(|tool| tool.get("function"))
        .map(|function| {
            let mut declaration = json!({
                "name": function.get("name").and_then(Value::as_str).unwrap_or_default()
            });
            if let Some(description) = function.get("description").filter(|value| !value.is_null())
            {
                declaration["description"] = description.clone();
            }
            if let Some(parameters) = function.get("parameters") {
                declaration["parameters"] = normalize_gemini_schema(parameters);
            }
            declaration
        })
        .collect::<Vec<_>>();
    (!declarations.is_empty()).then(|| json!([{"functionDeclarations":declarations}]))
}

fn build_tool_config(object: &Map<String, Value>) -> Option<Value> {
    let choice = object.get("tool_choice")?;
    let (mode, allowed) = match choice {
        Value::String(value) if value == "none" => ("NONE", None),
        Value::String(value) if value == "required" => ("ANY", None),
        Value::String(_) => ("AUTO", None),
        Value::Object(_) => (
            "ANY",
            choice
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str),
        ),
        _ => return None,
    };
    let mut config = json!({"mode":mode});
    if let Some(name) = allowed {
        config["allowedFunctionNames"] = json!([name]);
    }
    Some(json!({"functionCallingConfig":config}))
}

fn build_generation_config(object: &Map<String, Value>, model: &str) -> Option<Value> {
    let mut config = Map::new();
    for (source, target) in [
        ("temperature", "temperature"),
        ("top_p", "topP"),
        ("top_k", "topK"),
        ("frequency_penalty", "frequencyPenalty"),
        ("presence_penalty", "presencePenalty"),
        ("seed", "seed"),
        ("n", "candidateCount"),
    ] {
        if let Some(value) = object.get(source).filter(|value| !value.is_null()) {
            config.insert(target.to_string(), value.clone());
        }
    }
    if let Some(value) = object
        .get("max_completion_tokens")
        .or_else(|| object.get("max_tokens"))
        .filter(|value| !value.is_null())
    {
        config.insert("maxOutputTokens".into(), value.clone());
    }
    if let Some(stop) = object.get("stop") {
        let sequences = match stop {
            Value::String(text) => json!([text]),
            Value::Array(_) => stop.clone(),
            _ => Value::Null,
        };
        if !sequences.is_null() {
            config.insert("stopSequences".into(), sequences);
        }
    }
    if let Some(response_format) = object.get("response_format") {
        match response_format.get("type").and_then(Value::as_str) {
            Some("json_object") => {
                config.insert("responseMimeType".into(), json!("application/json"));
            }
            Some("json_schema") => {
                config.insert("responseMimeType".into(), json!("application/json"));
                if let Some(schema) = response_format
                    .get("json_schema")
                    .and_then(|schema| schema.get("schema"))
                {
                    config.insert("responseSchema".into(), normalize_gemini_schema(schema));
                }
            }
            _ => {}
        }
    }
    if let Some(thinking) = object
        .get("thinking_config")
        .or_else(|| object.get("thinkingConfig"))
    {
        config.insert("thinkingConfig".into(), thinking.clone());
    } else if let Some(effort) = object.get("reasoning_effort").and_then(Value::as_str) {
        let thinking = if model.contains("gemini-3") {
            let level = match effort {
                "none" => None,
                "minimal" => Some("minimal"),
                "low" => Some("low"),
                "medium" => Some("medium"),
                _ => Some("high"),
            };
            level.map(|level| json!({"thinkingLevel":level,"includeThoughts":true}))
        } else {
            let budget = match effort {
                "none" => None,
                "minimal" | "low" => Some(1024),
                "medium" => Some(2048),
                "high" => Some(4096),
                "xhigh" => Some(8192),
                "max" => Some(16384),
                _ => None,
            };
            budget.map(|budget| json!({"thinkingBudget":budget,"includeThoughts":true}))
        };
        if let Some(thinking) = thinking {
            config.insert("thinkingConfig".into(), thinking);
        }
    }
    (!config.is_empty()).then_some(Value::Object(config))
}

pub(crate) fn normalize_gemini_schema(schema: &Value) -> Value {
    let mut output = schema.clone();
    let definitions = take_definitions(&mut output);
    inline_refs(&mut output, &definitions, &mut Vec::new());
    clean_schema_node(&mut output);
    output
}

fn take_definitions(root: &mut Value) -> Map<String, Value> {
    let mut definitions = Map::new();
    if let Value::Object(object) = root {
        for key in ["$defs", "definitions"] {
            if let Some(Value::Object(values)) = object.remove(key) {
                definitions.extend(values);
            }
        }
    }
    definitions
}

fn for_each_child_schema(object: &mut Map<String, Value>, mut visit: impl FnMut(&mut Value)) {
    if let Some(items) = object.get_mut("items") {
        visit(items);
    }
    if let Some(Value::Object(properties)) = object.get_mut("properties") {
        properties.values_mut().for_each(&mut visit);
    }
    for key in ["anyOf", "allOf"] {
        if let Some(Value::Array(values)) = object.get_mut(key) {
            values.iter_mut().for_each(&mut visit);
        }
    }
}

fn inline_refs(value: &mut Value, definitions: &Map<String, Value>, chain: &mut Vec<String>) {
    let reference_name = value.get("$ref").and_then(Value::as_str).map(|reference| {
        reference
            .rsplit('/')
            .next()
            .unwrap_or(reference)
            .to_string()
    });
    if let Some(name) = reference_name {
        if chain.contains(&name) {
            return;
        }
        if let Some(definition) = definitions.get(&name) {
            let mut resolved = definition.clone();
            if let (Value::Object(resolved), Value::Object(original)) = (&mut resolved, &*value) {
                for (key, child) in original {
                    if key != "$ref" {
                        resolved.insert(key.clone(), child.clone());
                    }
                }
            }
            chain.push(name);
            inline_refs(&mut resolved, definitions, chain);
            chain.pop();
            *value = resolved;
        }
        return;
    }
    match value {
        Value::Object(object) => {
            object.remove("$defs");
            object.remove("definitions");
            for_each_child_schema(object, |child| inline_refs(child, definitions, chain));
        }
        Value::Array(array) => array
            .iter_mut()
            .for_each(|child| inline_refs(child, definitions, chain)),
        _ => {}
    }
}

fn rewrite_schema_structure(object: &mut Map<String, Value>) -> bool {
    let mut changed = false;

    if let Some(Value::Array(members)) = object.remove("allOf") {
        changed = true;
        for member in members {
            let Value::Object(member) = member else {
                continue;
            };
            for (key, value) in member {
                if key == "properties" {
                    if let Value::Object(source) = value {
                        if let Value::Object(target) =
                            object.entry("properties").or_insert_with(|| json!({}))
                        {
                            for (name, schema) in source {
                                target.entry(name).or_insert(schema);
                            }
                        }
                    }
                } else if key == "required" {
                    if let Value::Array(source) = value {
                        if let Value::Array(target) =
                            object.entry("required").or_insert_with(|| json!([]))
                        {
                            for required in source {
                                if !target.contains(&required) {
                                    target.push(required);
                                }
                            }
                        }
                    }
                } else {
                    object.entry(key).or_insert(value);
                }
            }
        }
    }

    if let Some(constant) = object.remove("const") {
        changed = true;
        let schema_type = schema_type_for_constant(&constant);
        object.insert("enum".into(), Value::Array(vec![constant]));
        object.entry("type").or_insert_with(|| json!(schema_type));
    }

    if let Some(Value::Array(types)) = object.get("type").cloned() {
        changed = true;
        let names = types
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>();
        let nullable = names.iter().any(|name| name == "null");
        let non_null = names
            .into_iter()
            .filter(|name| name != "null")
            .collect::<Vec<_>>();
        object.remove("type");
        if nullable {
            object.insert("nullable".into(), Value::Bool(true));
        }
        match non_null.as_slice() {
            [] => {}
            [only] => {
                object.insert("type".into(), json!(only));
            }
            many => {
                object.insert(
                    "anyOf".into(),
                    Value::Array(many.iter().map(|kind| json!({"type":kind})).collect()),
                );
            }
        }
    }

    let collapsible_any_of = object
        .get("anyOf")
        .and_then(Value::as_array)
        .is_some_and(|members| {
            members.len() == 1
                || members
                    .iter()
                    .any(|member| member.get("type").and_then(Value::as_str) == Some("null"))
        });
    if collapsible_any_of {
        if let Some(Value::Array(members)) = object.remove("anyOf") {
            changed = true;
            let nullable = members
                .iter()
                .any(|member| member.get("type").and_then(Value::as_str) == Some("null"));
            let non_null = members
                .into_iter()
                .filter(|member| member.get("type").and_then(Value::as_str) != Some("null"))
                .collect::<Vec<_>>();
            if nullable {
                object.insert("nullable".into(), Value::Bool(true));
            }
            match non_null.len() {
                0 => {}
                1 => {
                    if let Some(Value::Object(member)) = non_null.into_iter().next() {
                        for (key, value) in member {
                            object.entry(key).or_insert(value);
                        }
                    }
                }
                _ => {
                    object.insert("anyOf".into(), Value::Array(non_null));
                }
            }
        }
    }
    changed
}

fn schema_type_for_constant(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "string",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_f64() => "number",
        Value::Number(_) => "integer",
        Value::Array(_) => "array",
        _ => "object",
    }
}

fn clean_schema_node(value: &mut Value) {
    const ALLOWED: &[&str] = &[
        "type",
        "format",
        "description",
        "title",
        "nullable",
        "enum",
        "items",
        "properties",
        "required",
        "anyOf",
        "default",
        "minLength",
        "maxLength",
        "pattern",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "propertyOrdering",
    ];
    let object = match value {
        Value::Object(object) => object,
        Value::Array(values) => {
            values.iter_mut().for_each(clean_schema_node);
            return;
        }
        _ => return,
    };

    let mut guard = 0;
    while rewrite_schema_structure(object) && guard < 64 {
        guard += 1;
    }

    if object.get("type").and_then(Value::as_str) == Some("array") && !object.contains_key("items")
    {
        object.insert("items".into(), json!({"type":"object"}));
    }
    if object.contains_key("enum") {
        match object.get("type").and_then(Value::as_str) {
            Some("string") => {}
            Some(_) => {
                object.remove("enum");
            }
            None => {
                object.insert("type".into(), json!("string"));
            }
        }
    }
    object.remove("additionalProperties");
    if !object.contains_key("type") && !object.contains_key("anyOf") && !object.contains_key("enum")
    {
        object.insert("type".into(), json!("object"));
    }
    if object
        .get("format")
        .and_then(Value::as_str)
        .is_some_and(|format| format != "enum" && format != "date-time")
    {
        object.remove("format");
    }
    for_each_child_schema(object, clean_schema_node);
    object.retain(|key, _| ALLOWED.contains(&key.as_str()));
}

pub fn gemini_response_to_completions(body: &Bytes, fallback_model: &str) -> Result<Bytes> {
    let native: Value = serde_json::from_slice(body).context("parse Gemini response")?;
    Ok(Bytes::from(serde_json::to_vec(
        &gemini_value_to_completions(&native, fallback_model)?,
    )?))
}

pub fn gemini_value_to_completions(native: &Value, fallback_model: &str) -> Result<Value> {
    let model = native
        .get("modelVersion")
        .and_then(Value::as_str)
        .unwrap_or(fallback_model);
    let id = native
        .get("responseId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("gemini-{}", chrono::Utc::now().timestamp_millis()));
    let candidates = native.get("candidates").and_then(Value::as_array);
    let choices = if candidates.is_none_or(Vec::is_empty) {
        let blocked = native
            .get("promptFeedback")
            .and_then(|feedback| feedback.get("blockReason"))
            .is_some();
        vec![json!({
            "message":{"content":"","role":"assistant"},
            "index":0,
            "finish_reason": if blocked {"content_filter"} else {"stop"}
        })]
    } else {
        candidates
            .expect("checked")
            .iter()
            .enumerate()
            .map(|(index, candidate)| build_choice(candidate, index, &id))
            .collect()
    };
    let mut response = json!({
        "model": model,
        "choices": choices,
        "id": id,
        "created": chrono::Utc::now().timestamp(),
        "object": "chat.completion"
    });
    if let Some(usage) = native.get("usageMetadata") {
        response["usage"] = gemini_usage(usage);
    }
    Ok(response)
}

fn build_choice(candidate: &Value, index: usize, response_id: &str) -> Value {
    let decoded = decode_parts(
        candidate
            .get("content")
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array),
    );
    let mut message = json!({"role":"assistant"});
    if !decoded.text.is_empty() || (decoded.tool_calls.is_empty() && decoded.reasoning.is_empty()) {
        message["content"] = json!(decoded.text);
    }
    if !decoded.reasoning.is_empty() {
        message["reasoning_content"] = json!(decoded.reasoning);
    }
    if !decoded.tool_calls.is_empty() {
        message["tool_calls"] = json!(decoded
            .tool_calls
            .iter()
            .enumerate()
            .map(|(tool_index, call)| tool_call_json(call, response_id, tool_index))
            .collect::<Vec<_>>());
    }
    let finish = finish_reason(
        candidate.get("finishReason").and_then(Value::as_str),
        !decoded.tool_calls.is_empty(),
    );
    json!({"message":message,"index":index,"finish_reason":finish})
}

#[derive(Default)]
struct DecodedParts {
    text: String,
    reasoning: String,
    tool_calls: Vec<NativeToolCall>,
}

struct NativeToolCall {
    id: Option<String>,
    name: String,
    args: Value,
    signature: Option<String>,
}

fn decode_parts(parts: Option<&Vec<Value>>) -> DecodedParts {
    let mut decoded = DecodedParts::default();
    for part in parts.into_iter().flatten() {
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            if part
                .get("thought")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                decoded.reasoning.push_str(text);
            } else {
                decoded.text.push_str(text);
            }
        }
        if let Some(call) = part.get("functionCall") {
            decoded.tool_calls.push(NativeToolCall {
                id: call.get("id").and_then(Value::as_str).map(str::to_string),
                name: call
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                args: call.get("args").cloned().unwrap_or_else(|| json!({})),
                signature: part
                    .get("thoughtSignature")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
    }
    decoded
}

fn tool_call_json(call: &NativeToolCall, response_id: &str, index: usize) -> Value {
    let base = call
        .id
        .clone()
        .unwrap_or_else(|| format!("call_{response_id}_{index}"));
    let id = call
        .signature
        .as_deref()
        .map(|signature| format!("{base}{THOUGHT_SIGNATURE_SEPARATOR}{signature}"))
        .unwrap_or(base);
    json!({
        "type":"function",
        "id":id,
        "function":{
            "name":call.name,
            "arguments":serde_json::to_string(&call.args).unwrap_or_else(|_| "{}".into())
        }
    })
}

fn finish_reason(reason: Option<&str>, has_tools: bool) -> &'static str {
    match reason {
        Some("MAX_TOKENS") => "length",
        Some(
            "SAFETY"
            | "RECITATION"
            | "LANGUAGE"
            | "BLOCKLIST"
            | "PROHIBITED_CONTENT"
            | "SPII"
            | "UNEXPECTED_TOOL_CALL"
            | "TOO_MANY_TOOL_CALLS"
            | "IMAGE_SAFETY"
            | "IMAGE_PROHIBITED_CONTENT"
            | "IMAGE_RECITATION",
        ) => "content_filter",
        _ if has_tools => "tool_calls",
        _ => "stop",
    }
}

fn gemini_usage(usage: &Value) -> Value {
    let input = usage
        .get("promptTokenCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage
        .get("candidatesTokenCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut result = json!({
        "prompt_tokens":input,
        "completion_tokens":output,
        "total_tokens":usage.get("totalTokenCount").and_then(Value::as_u64).unwrap_or(input + output)
    });
    if let Some(cached) = usage.get("cachedContentTokenCount").and_then(Value::as_u64) {
        result["prompt_tokens_details"] = json!({"cached_tokens":cached});
    }
    if let Some(reasoning) = usage.get("thoughtsTokenCount").and_then(Value::as_u64) {
        result["completion_tokens_details"] = json!({"reasoning_tokens":reasoning});
    }
    result
}

pub fn translate_gemini_error(body: &Bytes) -> Result<Bytes> {
    let value: Value = serde_json::from_slice(body).context("parse Gemini error")?;
    let root = value
        .as_array()
        .and_then(|array| array.first())
        .unwrap_or(&value);
    let error = root.get("error").unwrap_or(root);
    let status = error.get("status").and_then(Value::as_str);
    let error_type = match status {
        Some("INVALID_ARGUMENT" | "FAILED_PRECONDITION") => "invalid_request_error",
        Some("NOT_FOUND") => "not_found_error",
        Some("PERMISSION_DENIED" | "UNAUTHENTICATED") => "authentication_error",
        Some("RESOURCE_EXHAUSTED") => "rate_limit_error",
        _ => "api_error",
    };
    Ok(Bytes::from(serde_json::to_vec(&json!({
        "error":{
            "type":error_type,
            "message":error.get("message").and_then(Value::as_str).unwrap_or("Gemini request failed"),
            "param":null,
            "code":status
        }
    }))?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_same_name_tool_results_restore_call_order() {
        let body = Bytes::from_static(
            br#"{
              "model":"gemini-2.5-pro",
              "messages":[
                {"role":"user","content":"compare"},
                {"role":"assistant","tool_calls":[
                  {"id":"call_a","type":"function","function":{"name":"lookup","arguments":"{\"id\":\"a\"}"}},
                  {"id":"call_b","type":"function","function":{"name":"lookup","arguments":"{\"id\":\"b\"}"}}
                ]},
                {"role":"tool","tool_call_id":"call_b","content":"result-b"},
                {"role":"tool","tool_call_id":"call_a","content":"result-a"}
              ]
            }"#,
        );
        let translated = completions_request_to_gemini(&body, "gemini-2.5-pro").unwrap();
        let value: Value = serde_json::from_slice(&translated).unwrap();
        let responses = value["contents"][2]["parts"].as_array().unwrap();
        assert_eq!(
            responses[0]["functionResponse"]["response"]["content"],
            "result-a"
        );
        assert_eq!(
            responses[1]["functionResponse"]["response"]["content"],
            "result-b"
        );
        assert!(responses
            .iter()
            .all(|response| response["functionResponse"].get("id").is_none()));
    }

    #[test]
    fn schema_normalization_flattens_and_makes_nullable() {
        let schema = json!({
            "allOf":[
                {"type":"object","properties":{"name":{"type":"string"}},"required":["name"]},
                {"properties":{"age":{"type":["integer","null"]}},"required":["age"]}
            ],
            "additionalProperties":false
        });
        let normalized = normalize_gemini_schema(&schema);
        assert_eq!(normalized["type"], "object");
        assert_eq!(normalized["properties"]["name"]["type"], "string");
        assert_eq!(normalized["properties"]["age"]["type"], "integer");
        assert_eq!(normalized["properties"]["age"]["nullable"], true);
        assert_eq!(normalized["required"], json!(["name", "age"]));
        assert!(normalized.get("allOf").is_none());
        assert!(normalized.get("additionalProperties").is_none());
    }
}
