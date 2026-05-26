//! Cross-protocol request body extraction.

use bytes::Bytes;
use serde_json::Value;

use super::messages::{extract_user_from_messages, user_message_has_visible_text};
use super::responses::{count_responses_input_user_turns, extract_user_from_responses_input};

/// Last substantive user turn text (may include formatted `tool_result` blocks).
pub fn extract_user_message_from_request_body(body: &Bytes) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    extract_user_from_messages(&v).or_else(|| extract_user_from_responses_input(&v))
}

/// Count user-visible turns in a request body (Anthropic `messages` or Responses `input`).
pub fn count_visible_user_messages(v: &Value) -> usize {
    if let Some(messages) = v.get("messages").and_then(|m| m.as_array()) {
        return messages
            .iter()
            .filter(|msg| user_message_has_visible_text(msg))
            .count();
    }
    if let Some(input) = v.get("input") {
        return count_responses_input_user_turns(input);
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use serde_json::json;

    #[test]
    fn count_visible_user_messages_increments_with_new_user_turns() {
        let one = json!({"messages":[{"role":"user","content":"hi"}]});
        assert_eq!(count_visible_user_messages(&one), 1);
        let two = json!({"messages":[
            {"role":"user","content":"hi"},
            {"role":"assistant","content":"hello"},
            {"role":"user","content":"hi again"}
        ]});
        assert_eq!(count_visible_user_messages(&two), 2);
        let replay = json!({"messages":[
            {"role":"user","content":"hi"},
            {"role":"assistant","content":"hello"},
            {"role":"user","content":"hi again"}
        ]});
        assert_eq!(count_visible_user_messages(&replay), 2);
    }

    #[test]
    fn anthropic_request_skips_system_reminder() {
        let body = br#"{"messages":[{"role":"user","content":[
            {"type":"text","text":"<system-reminder>\nskills\n</system-reminder>"},
            {"type":"text","text":"hi"}
        ]}]}"#;
        assert_eq!(
            extract_user_message_from_request_body(&Bytes::from_static(body)).as_deref(),
            Some("hi")
        );
    }

    #[test]
    fn unwraps_session_tag() {
        let body = r#"{"messages":[{"role":"user","content":[{"type":"text","text":"<session>\nhi\n</session>"}]}]}"#;
        assert_eq!(
            extract_user_message_from_request_body(&Bytes::copy_from_slice(body.as_bytes()))
                .as_deref(),
            Some("hi")
        );
    }

    #[test]
    fn skips_suggestion_mode_prompt() {
        let body = br#"{"messages":[{"role":"user","content":[{"type":"text","text":"[SUGGESTION MODE: predict next input]\nReply ONLY the suggestion."}]}]}"#;
        assert!(extract_user_message_from_request_body(&Bytes::from_static(body)).is_none());
    }

    #[test]
    fn request_includes_tool_result() {
        let body = br#"{"messages":[{"role":"user","content":[
            {"type":"tool_result","tool_use_id":"toolu_1","content":"file1.rs\nfile2.rs"}
        ]}]}"#;
        let out = extract_user_message_from_request_body(&Bytes::from_static(body)).unwrap();
        assert!(out.contains("```tool_result:toolu_1"));
        assert!(out.contains("file1.rs"));
    }
}
