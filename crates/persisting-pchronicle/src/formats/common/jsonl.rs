use std::io::BufRead;

use serde_json::Value;

use crate::{InputIssue, InputResult};

pub(crate) fn for_each_jsonl_object<R, F>(mut reader: R, mut visit: F) -> InputResult<usize>
where
    R: BufRead,
    F: FnMut(usize, Value) -> InputResult<()>,
{
    let mut buf = String::new();
    let mut line_number = 0usize;
    let mut objects = 0usize;
    loop {
        buf.clear();
        let n = reader.read_line(&mut buf).map_err(|error| {
            InputIssue::invalid(error.to_string()).at(format!("line {}", line_number + 1))
        })?;
        if n == 0 {
            break;
        }
        line_number += 1;
        let trimmed = buf.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(trimmed).map_err(|error| {
            InputIssue::invalid(error.to_string()).at(format!("line {line_number}"))
        })?;
        if !value.is_object() {
            return Err(InputIssue::invalid("JSONL line must be an object")
                .at(format!("line {line_number}")));
        }
        visit(line_number, value)?;
        objects += 1;
    }
    Ok(objects)
}

pub(crate) fn filename_stem(relative_path: &str) -> String {
    std::path::Path::new(relative_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(relative_path)
        .to_string()
}

pub(crate) fn join_text_parts(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Array(parts) => {
            let texts = parts
                .iter()
                .filter_map(part_text)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>();
            (!texts.is_empty()).then_some(texts.join(""))
        }
        Value::Object(object) => object
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
            .or_else(|| object.get("content").and_then(join_text_parts)),
        _ => None,
    }
}

pub(crate) fn leftover_textless_parts(value: &Value) -> Vec<(usize, Value)> {
    let Value::Array(parts) = value else {
        return Vec::new();
    };
    parts
        .iter()
        .enumerate()
        .filter_map(|(index, part)| part_text(part).is_none().then(|| (index, part.clone())))
        .collect()
}

fn part_text(part: &Value) -> Option<String> {
    match part {
        Value::String(text) => Some(text.clone()),
        Value::Object(object) => object
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                object
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }),
        _ => None,
    }
}

pub(crate) fn parse_json_value(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}
