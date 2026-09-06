use dioxus::prelude::*;
use serde_json::Value;

const JSON_VALUE_PREVIEW_LIMIT: usize = 240;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonShape {
    Scalar,
    KvTable,
    RecordTable,
    Tree,
}

pub fn peel_json(value: &Value) -> Value {
    match value {
        Value::String(raw) => match serde_json::from_str::<Value>(raw) {
            Ok(parsed) if parsed.is_object() || parsed.is_array() => parsed,
            _ => value.clone(),
        },
        other => other.clone(),
    }
}

pub fn classify_json(value: &Value) -> JsonShape {
    classify_peeled(&peel_json(value))
}

pub fn is_structured_json(value: &Value) -> bool {
    let peeled = peel_json(value);
    peeled.is_object() || peeled.is_array()
}

fn is_scalar(value: &Value) -> bool {
    matches!(
        value,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    )
}

fn classify_peeled(value: &Value) -> JsonShape {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => JsonShape::Scalar,
        Value::Object(object) => {
            if object.values().all(is_scalar) {
                JsonShape::KvTable
            } else {
                JsonShape::Tree
            }
        }
        Value::Array(items) => {
            if !items.is_empty()
                && items.iter().all(|item| {
                    item.as_object()
                        .is_some_and(|object| object.values().all(is_scalar))
                })
            {
                JsonShape::RecordTable
            } else {
                JsonShape::Tree
            }
        }
    }
}

pub fn record_columns(rows: &[Value]) -> Vec<String> {
    let mut columns = Vec::new();
    for row in rows {
        if let Value::Object(object) = row {
            for key in object.keys() {
                if !columns.contains(key) {
                    columns.push(key.clone());
                }
            }
        }
    }
    columns
}

pub fn json_summary(value: &Value) -> String {
    match peel_json(value) {
        Value::Object(object) => format!("{{{} keys}}", object.len()),
        Value::Array(items) => format!("[{} items]", items.len()),
        Value::String(_) => "string".into(),
        Value::Number(_) => "number".into(),
        Value::Bool(_) => "boolean".into(),
        Value::Null => "null".into(),
    }
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

fn json_literal(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| scalar_text(value))
}

fn json_preview(value: &str) -> String {
    let mut chars = value.chars();
    let preview = chars
        .by_ref()
        .take(JSON_VALUE_PREVIEW_LIMIT)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[component]
pub fn JsonValue(value: Value, #[props(default = false)] default_open: bool) -> Element {
    let peeled = peel_json(&value);
    match classify_json(&value) {
        JsonShape::Scalar => {
            let text = scalar_text(&peeled);
            rsx! { span { class: "pc2-json-scalar", "{text}" } }
        }
        JsonShape::KvTable => rsx! { JsonKvTable { value: peeled, default_open } },
        JsonShape::RecordTable => rsx! { JsonRecordTable { value: peeled, default_open } },
        JsonShape::Tree => rsx! { JsonTree { value: peeled, default_open } },
    }
}

#[component]
fn JsonScalar(value: Value) -> Element {
    let literal = json_literal(&value);
    let kind = json_type(&value);
    if literal.chars().count() > JSON_VALUE_PREVIEW_LIMIT {
        rsx! {
            details { class: "pc2-json-long-value",
                summary { class: "pc2-json-value {kind}", "{json_preview(&literal)}" }
                div { class: "pc2-json-expanded-value {kind}", "{literal}" }
            }
        }
    } else {
        rsx! { span { class: "pc2-json-value {kind}", "{literal}" } }
    }
}

#[component]
fn JsonKvTable(value: Value, default_open: bool) -> Element {
    let map = match value {
        Value::Object(map) => map,
        _ => return rsx! { span { class: "pc2-json-scalar", "—" } },
    };
    rsx! {
        table { class: "pc2-json-table pc2-json-kv",
            thead { tr { th { "key" } th { "value" } } }
            tbody {
                for (key, child) in map {
                    tr { key: "{key}",
                        th { scope: "row", "{key}" }
                        td { JsonValue { value: child, default_open } }
                    }
                }
            }
        }
    }
}

#[component]
fn JsonRecordTable(value: Value, default_open: bool) -> Element {
    let rows = match value {
        Value::Array(rows) => rows,
        _ => return rsx! { span { class: "pc2-json-scalar", "—" } },
    };
    let columns = record_columns(&rows);
    rsx! {
        div { class: "pc2-json-scroll",
            table { class: "pc2-json-table pc2-json-records",
                thead { tr { for column in columns.iter() { th { "{column}" } } } }
                tbody {
                    for (row_index, row) in rows.iter().enumerate() {
                        tr { key: "{row_index}",
                            for column in columns.iter() {
                                td {
                                    JsonValue {
                                        value: match row {
                                            Value::Object(object) => {
                                                object.get(column).cloned().unwrap_or(Value::Null)
                                            }
                                            _ => Value::Null,
                                        },
                                        default_open,
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn JsonTree(value: Value, default_open: bool) -> Element {
    match value {
        Value::Array(items) if items.is_empty() => rsx! {
            details { class: "pc2-json-node", open: default_open,
                summary { span { class: "pc2-json-size", "[0 items]" } }
            }
        },
        Value::Object(map) => rsx! {
            div { class: "pc2-json-tree pc2-json-object",
                for (key, child) in map {
                    JsonTreeNode { key: "{key}", label: key, value: child, default_open }
                }
            }
        },
        Value::Array(items) => rsx! {
            div { class: "pc2-json-tree pc2-json-array",
                for (index, child) in items.into_iter().enumerate() {
                    JsonTreeNode { key: "{index}", label: String::new(), value: child, default_open: false }
                }
            }
        },
        other => {
            let text = scalar_text(&other);
            rsx! { span { class: "pc2-json-scalar", "{text}" } }
        }
    }
}

#[component]
fn JsonTreeNode(label: String, value: Value, default_open: bool) -> Element {
    let peeled = peel_json(&value);
    if is_scalar(&peeled) {
        return rsx! {
            div { class: "pc2-json-leaf",
                if !label.is_empty() { span { class: "pc2-json-key", "{label}" span { class: "pc2-json-punctuation", ":" } } }
                JsonScalar { value: peeled }
            }
        };
    }
    let summary = json_summary(&value);
    rsx! {
        details { class: "pc2-json-node", open: default_open,
            summary {
                if !label.is_empty() { span { class: "pc2-json-key", "{label}" span { class: "pc2-json-punctuation", ":" } } }
                span { class: "pc2-json-size", "{summary}" }
            }
            JsonValue { value, default_open: false }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn peel_promotes_object_and_array_strings_only() {
        assert_eq!(peel_json(&json!({"a": 1})), json!({"a": 1}));
        assert_eq!(peel_json(&json!("{\"a\":1}")), json!({"a": 1}));
        assert_eq!(peel_json(&json!("[1,2]")), json!([1, 2]));
        assert_eq!(peel_json(&json!("not-json")), json!("not-json"));
        assert_eq!(peel_json(&json!("{")), json!("{"));
        assert_eq!(peel_json(&json!("\"hello\"")), json!("\"hello\""));
        assert_eq!(peel_json(&json!(7)), json!(7));
    }

    #[test]
    fn classify_matches_one_level_tables_and_trees() {
        assert_eq!(classify_json(&json!("plain")), JsonShape::Scalar);
        assert_eq!(classify_json(&json!({})), JsonShape::KvTable);
        assert_eq!(
            classify_json(&json!({"b": true, "a": 1})),
            JsonShape::KvTable
        );
        assert_eq!(
            classify_json(&json!([{"b": 2, "a": 1}, {"a": 3, "c": null}])),
            JsonShape::RecordTable
        );
        assert_eq!(
            classify_json(&json!([{"fn": "read", "args": "{\"path\":\"x\"}"}])),
            JsonShape::RecordTable
        );
        assert_eq!(
            classify_json(&json!(
                "{\"fn\":\"read\",\"args\":\"{\\\"path\\\":\\\"x\\\"}\"}"
            )),
            JsonShape::KvTable
        );
        assert_eq!(
            classify_json(&peel_json(&json!("{\"path\":\"x\"}"))),
            JsonShape::KvTable
        );
        assert_eq!(classify_json(&json!({"nested": {"x": 1}})), JsonShape::Tree);
        assert_eq!(classify_json(&json!([1, 2, 3])), JsonShape::Tree);
        assert_eq!(classify_json(&json!([])), JsonShape::Tree);
        assert_eq!(classify_json(&json!([{"a": 1}, "tail"])), JsonShape::Tree);
        assert_eq!(classify_json(&json!([{"a": {"b": 1}}])), JsonShape::Tree);
    }

    #[test]
    fn structured_detection_follows_peel() {
        assert!(!is_structured_json(&json!("hello")));
        assert!(is_structured_json(&json!({"a": 1})));
        assert!(is_structured_json(&json!("{\"a\":1}")));
        assert!(!is_structured_json(&json!("\"hello\"")));
    }

    #[test]
    fn record_columns_keep_first_seen_union() {
        let rows = vec![json!({"b": 2, "a": 1}), json!({"a": 3, "c": null})];
        assert_eq!(record_columns(&rows), vec!["a", "b", "c"]);
    }

    #[test]
    fn json_summary_peels_and_names_types() {
        assert_eq!(json_summary(&json!({"a": 1, "b": 2})), "{2 keys}");
        assert_eq!(json_summary(&json!([1, 2, 3])), "[3 items]");
        assert_eq!(json_summary(&json!([])), "[0 items]");
        assert_eq!(json_summary(&json!("{}")), "{0 keys}");
        assert_eq!(json_summary(&json!("hello")), "string");
        assert_eq!(json_summary(&json!(true)), "boolean");
        assert_eq!(json_summary(&json!(1)), "number");
        assert_eq!(json_summary(&json!(null)), "null");
    }

    #[test]
    fn long_json_values_get_single_line_previews() {
        let value = "x".repeat(JSON_VALUE_PREVIEW_LIMIT + 1);
        let preview = json_preview(&value);
        assert_eq!(preview.chars().count(), JSON_VALUE_PREVIEW_LIMIT + 1);
        assert!(preview.ends_with('…'));
        assert_eq!(json_preview("short"), "short");
    }
}
