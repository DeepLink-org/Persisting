//! DeepSeek Chat Completions multi-turn tool-call reasoning replay (moon-bridge deepseek_v4 subset).

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};

/// Per-session cache: tool_call_id → reasoning_content to echo on follow-up requests.
#[derive(Debug, Default)]
pub struct ReasoningCache {
    by_tool_call: HashMap<String, String>,
}

impl ReasoningCache {
    pub fn remember(&mut self, tool_call_ids: &[String], reasoning: &str) {
        if reasoning.is_empty() {
            return;
        }
        for id in tool_call_ids {
            if id.is_empty() {
                continue;
            }
            self.by_tool_call.insert(id.clone(), reasoning.to_string());
        }
    }

    pub fn get(&self, tool_call_id: &str) -> Option<&str> {
        self.by_tool_call.get(tool_call_id).map(String::as_str)
    }
}

#[derive(Debug, Default)]
pub struct ReasoningCacheHandle {
    inner: Mutex<ReasoningCache>,
}

impl ReasoningCacheHandle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn remember(&self, tool_call_ids: &[String], reasoning: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.remember(tool_call_ids, reasoning);
        }
    }

    pub fn apply_to_messages(&self, messages: &mut [Value]) {
        let Ok(cache) = self.inner.lock() else {
            return;
        };
        apply_deepseek_message_fixup(messages, &cache);
    }
}

/// Add cached or empty `reasoning_content` on assistant messages with `tool_calls`.
pub fn apply_deepseek_message_fixup(messages: &mut [Value], cache: &ReasoningCache) {
    for msg in messages.iter_mut() {
        let Some(obj) = msg.as_object_mut() else {
            continue;
        };
        if obj.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        let Some(tool_calls) = obj.get("tool_calls").and_then(|t| t.as_array()) else {
            continue;
        };
        if tool_calls.is_empty() {
            continue;
        }
        if obj
            .get("reasoning_content")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
        {
            continue;
        }
        for tc in tool_calls {
            let Some(id) = tc.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            if let Some(reasoning) = cache.get(id) {
                obj.insert("reasoning_content".into(), json!(reasoning));
                break;
            }
        }
        if !obj.contains_key("reasoning_content") {
            obj.insert("reasoning_content".into(), json!(""));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_empty_reasoning_for_tool_call_assistant() {
        let mut cache = ReasoningCache::default();
        let mut msgs = vec![json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "shell", "arguments": "{}"}}]
        })];
        apply_deepseek_message_fixup(&mut msgs, &cache);
        assert_eq!(msgs[0]["reasoning_content"], "");

        cache.remember(&["call_1".into()], "thinking...");
        apply_deepseek_message_fixup(&mut msgs, &cache);
        assert_eq!(msgs[0]["reasoning_content"], "thinking...");
    }
}
