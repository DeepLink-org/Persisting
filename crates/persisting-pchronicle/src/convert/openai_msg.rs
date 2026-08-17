//! openai_msg ⇄ storyline.

use crate::convert::message_text;
use crate::formats::openai_msg::{OpenaiMsgDocument, OpenaiMsgStep};
use crate::formats::storyline::{StorylineAgent, StorylineDocument, StorylineTurn};
use crate::{DocumentFormat, Error, Result};

pub fn openai_msg_to_storyline(doc: &OpenaiMsgDocument) -> Result<StorylineDocument> {
    let mut turns = Vec::new();
    let mut next_id = 1i64;

    for (record_index, step) in doc.session_steps.iter().enumerate() {
        let messages = Some(
            step.messages_value()
                .map_err(|error| Error::InvalidDocument {
                    format: DocumentFormat::OpenaiMsg,
                    path: None,
                    location: Some(format!("record[{record_index}].messages")),
                    message: error.to_string(),
                })?,
        );
        let response = step
            .response_value()
            .map_err(|error| Error::InvalidDocument {
                format: DocumentFormat::OpenaiMsg,
                path: None,
                location: Some(format!("record[{record_index}].response")),
                message: error.to_string(),
            })?;
        let ts = Some(step.created_at.clone()).filter(|s| !s.is_empty());

        if let Some(user_msg) = last_user_content(messages.as_ref()) {
            turns.push(StorylineTurn {
                id: next_id,
                kind: None,
                timestamp: ts.clone(),
                source: "user".into(),
                message: user_msg,
                reasoning_content: None,
                reasoning_effort: None,
                tool_calls: None,
                observation: None,
                metrics: None,
                model_name: None,
                llm_call_count: None,
                is_copied_context: None,
                latency_ms: None,
                ttft_ms: None,
                extra: Some(serde_json::json!({
                    "openai_msg_step_id": step.id,
                    "run_bucket": step.run_bucket,
                    "call_id": step.call_id,
                })),
            });
            next_id += 1;
        }

        let reply = response
            .as_ref()
            .and_then(|r| r.get("content").cloned())
            .unwrap_or(serde_json::Value::String(String::new()));

        turns.push(StorylineTurn {
            id: next_id,
            kind: None,
            timestamp: ts,
            source: "agent".into(),
            message: reply,
            reasoning_content: None,
            reasoning_effort: None,
            tool_calls: None,
            observation: None,
            metrics: Some(serde_json::json!({
                "step_reward": step.step_reward,
                "reward": step.reward,
                "is_terminal": step.is_terminal,
                "is_trainable": step.is_trainable,
            })),
            model_name: Some(step.llm_model.clone()).filter(|s| !s.is_empty()),
            llm_call_count: Some(1),
            is_copied_context: None,
            latency_ms: None,
            ttft_ms: None,
            extra: Some(serde_json::json!({
                "openai_msg_step_id": step.id,
                "run_bucket": step.run_bucket,
                "call_id": step.call_id,
                "request_messages": messages,
            })),
        });
        next_id += 1;
    }

    Ok(StorylineDocument {
        schema_version: None,
        run_id: Some(doc.run_bucket.clone()).filter(|s| !s.is_empty()),
        attempt_id: None,
        session_id: doc.session_id.clone(),
        agent: StorylineAgent {
            id: doc.agent_id.clone(),
            name: Some(doc.agent_id.clone()),
            version: None,
            model_name: doc
                .session_steps
                .first()
                .map(|s| s.llm_model.clone())
                .filter(|s| !s.is_empty()),
            tool_definitions: None,
            extra: None,
        },
        parent: None,
        child_session_ids: None,
        notes: None,
        final_metrics: None,
        continued_trajectory_ref: None,
        extra: Some(serde_json::json!({
            "source": doc.source,
            "authoritative": doc.authoritative,
        })),
        turns,
    })
}

pub fn storyline_to_openai_msg(story: &StorylineDocument) -> Result<OpenaiMsgDocument> {
    story.validate()?;
    let mut steps = Vec::new();
    let mut i = 0usize;
    while i < story.turns.len() {
        let turn = &story.turns[i];
        let (user_turn, agent_turn) = if turn.source == "user" {
            let agent = story.turns.get(i + 1).filter(|t| t.source == "agent");
            if agent.is_some() {
                i += 2;
            } else {
                i += 1;
            }
            (Some(turn), agent)
        } else {
            i += 1;
            (None, Some(turn))
        };

        let Some(agent) = agent_turn.filter(|t| t.source != "user") else {
            let user = user_turn.unwrap_or(turn);
            steps.push(OpenaiMsgStep {
                id: format!("step-{}", user.id),
                session_id: story.session_id.clone(),
                step_id: user.id,
                job_id: String::new(),
                agent_id: story.agent.id.clone(),
                group_id: String::new(),
                env_name: String::new(),
                llm_model: String::new(),
                step_reward: 0.0,
                reward: 0.0,
                is_terminal: i >= story.turns.len(),
                is_truncated: false,
                is_session_completed: i >= story.turns.len(),
                is_trainable: true,
                created_at: user.timestamp.clone().unwrap_or_default(),
                messages: Some(serde_json::json!([{"role":"user","content": user.message}])),
                response: None,
                messages_json: None,
                response_json: None,
                env_state_json: None,
                extensions_json: None,
                capture_json: None,
                run_bucket: story.run_id.clone().unwrap_or_default(),
                call_id: String::new(),
                source_export_id: None,
            });
            continue;
        };

        let mut messages = Vec::new();
        if let Some(m) = agent
            .extra
            .as_ref()
            .and_then(|e| e.get("request_messages").cloned())
        {
            if let Some(arr) = m.as_array() {
                messages = arr.clone();
            }
        }
        if messages.is_empty() {
            if let Some(u) = user_turn {
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": u.message,
                }));
            }
        }

        let response = Some(if let Some(text) = message_text(&agent.message) {
            serde_json::json!({"role":"assistant","content": text})
        } else {
            serde_json::json!({"role":"assistant","content": agent.message})
        });

        let call_id = agent
            .extra
            .as_ref()
            .and_then(|e| e.get("call_id").and_then(|c| c.as_str()))
            .unwrap_or("")
            .to_string();

        steps.push(OpenaiMsgStep {
            id: format!("step-{}", agent.id),
            session_id: story.session_id.clone(),
            step_id: agent.id,
            job_id: String::new(),
            agent_id: story.agent.id.clone(),
            group_id: String::new(),
            env_name: String::new(),
            llm_model: agent.model_name.clone().unwrap_or_default(),
            step_reward: 0.0,
            reward: 0.0,
            is_terminal: i >= story.turns.len(),
            is_truncated: false,
            is_session_completed: i >= story.turns.len(),
            is_trainable: true,
            created_at: agent.timestamp.clone().unwrap_or_default(),
            messages: Some(serde_json::Value::Array(messages)),
            response,
            messages_json: None,
            response_json: None,
            env_state_json: None,
            extensions_json: None,
            capture_json: None,
            run_bucket: story.run_id.clone().unwrap_or_default(),
            call_id,
            source_export_id: None,
        });
    }

    Ok(OpenaiMsgDocument::new(story.session_id.clone(), steps))
}

fn last_user_content(messages: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    let arr = messages?.as_array()?;
    arr.iter().rev().find_map(|m| {
        if m.get("role").and_then(|r| r.as_str()) == Some("user") {
            m.get("content").cloned()
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::openai_msg_to_storyline;
    use crate::formats::openai_msg::{OpenaiMsgDocument, OpenaiMsgStep};
    use crate::{DocumentFormat, Error};

    fn step() -> OpenaiMsgStep {
        OpenaiMsgStep {
            id: "record-7".into(),
            session_id: "session-1".into(),
            step_id: 7,
            job_id: String::new(),
            agent_id: "agent-1".into(),
            group_id: String::new(),
            env_name: String::new(),
            llm_model: String::new(),
            step_reward: 0.0,
            reward: 0.0,
            is_terminal: false,
            is_truncated: false,
            is_session_completed: false,
            is_trainable: true,
            created_at: String::new(),
            messages: None,
            response: None,
            messages_json: Some("{".into()),
            response_json: None,
            env_state_json: None,
            extensions_json: None,
            capture_json: None,
            run_bucket: String::new(),
            call_id: String::new(),
            source_export_id: None,
        }
    }

    #[test]
    fn malformed_openai_messages_are_not_silently_dropped() {
        let document = OpenaiMsgDocument::new("session-1", vec![step()]);
        let error = openai_msg_to_storyline(&document).unwrap_err();
        assert!(matches!(
            error,
            Error::InvalidDocument {
                format: DocumentFormat::OpenaiMsg,
                location: Some(ref location),
                ..
            } if location == "record[0].messages"
        ));
    }

    #[test]
    fn malformed_openai_response_is_not_silently_dropped() {
        let mut record = step();
        record.messages_json = Some("[]".into());
        record.response_json = Some("{".into());
        let document = OpenaiMsgDocument::new("session-1", vec![record]);
        let error = openai_msg_to_storyline(&document).unwrap_err();
        assert!(matches!(
            error,
            Error::InvalidDocument {
                format: DocumentFormat::OpenaiMsg,
                location: Some(ref location),
                ..
            } if location == "record[0].response"
        ));
    }
}
