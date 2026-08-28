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
        .filter(|(_, part)| part_text(part).is_none())
        .map(|(index, part)| (index, part.clone()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::io::Cursor;

    fn object_strategy() -> impl Strategy<Value = Value> {
        (0u64..100_000).prop_map(|value| serde_json::json!({"value": value}))
    }

    fn text_part_strategy() -> impl Strategy<Value = Value> {
        prop_oneof![
            "[a-z]{0,12}".prop_map(Value::String),
            "[a-z]{0,12}".prop_map(|text| serde_json::json!({"text": text})),
            "[a-z]{0,12}".prop_map(|content| serde_json::json!({"content": content})),
            Just(serde_json::json!({"metadata": true})),
            Just(Value::Null),
            (0u8..4).prop_map(|value| serde_json::json!([value])),
        ]
    }

    proptest! {
        #[test]
        fn jsonl_counts_objects_and_reports_physical_line_numbers(
            objects in proptest::collection::vec(object_strategy(), 0..16),
            blank_lines in proptest::collection::vec(any::<bool>(), 0..16),
        ) {
            let mut input = String::new();
            let mut expected_lines = Vec::new();
            for (index, object) in objects.iter().enumerate() {
                let blanks = blank_lines.get(index).copied().unwrap_or(false);
                if blanks {
                    input.push('\n');
                }
                input.push_str(&object.to_string());
                input.push('\n');
                expected_lines.push(input.lines().count());
            }

            let mut visited = Vec::new();
            let count = for_each_jsonl_object(Cursor::new(input), |line, value| {
                visited.push((line, value));
                Ok(())
            }).unwrap();
            prop_assert_eq!(count, objects.len());
            prop_assert_eq!(visited.len(), objects.len());
            prop_assert_eq!(visited.iter().map(|(line, _)| *line).collect::<Vec<_>>(), expected_lines);
            prop_assert_eq!(visited.into_iter().map(|(_, value)| value).collect::<Vec<_>>(), objects);
        }

        #[test]
        fn jsonl_rejects_non_object_values_with_their_line_number(
            prefix in proptest::collection::vec(object_strategy(), 0..8),
            invalid in prop_oneof![Just("null"), Just("[]"), Just("1"), Just("\"text\"")],
        ) {
            let mut input = prefix.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
            if !input.is_empty() {
                input.push('\n');
            }
            let line = prefix.len() + 1;
            input.push_str(invalid);
            input.push('\n');
            let error = for_each_jsonl_object(Cursor::new(input), |_, _| Ok(())).unwrap_err();
            let expected_location = format!("line {}", line);
            prop_assert!(error.to_string().contains(&expected_location));
            prop_assert!(error.to_string().contains("must be an object"));
        }

        #[test]
        fn parse_json_value_roundtrips_json_and_falls_back_for_raw_text(
            value in object_strategy(),
            raw in "[a-zA-Z0-9 _-]{1,32}",
        ) {
            prop_assert_eq!(parse_json_value(&value.to_string()), value);
            let parsed = parse_json_value(&raw);
            if serde_json::from_str::<Value>(&raw).is_err() {
                prop_assert_eq!(parsed, Value::String(raw));
            }
        }

        #[test]
        fn text_helpers_concatenate_only_text_bearing_parts(
            parts in proptest::collection::vec(text_part_strategy(), 0..24),
        ) {
            let expected = parts.iter().filter_map(part_text)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>().join("");
            let value = Value::Array(parts.clone());
            prop_assert_eq!(join_text_parts(&value), (!expected.is_empty()).then_some(expected));

            let leftovers = leftover_textless_parts(&value);
            let expected_leftovers = parts.iter().enumerate()
                .filter(|(_, part)| part_text(part).is_none())
                .map(|(index, part)| (index, part.clone())).collect::<Vec<_>>();
            prop_assert_eq!(leftovers, expected_leftovers);
        }
    }
}
