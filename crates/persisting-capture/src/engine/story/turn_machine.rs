//! In-memory turn index updated as capture records flow through a story actor.

use serde_json::json;

use super::ids::{CallId, StoryId, TurnId};
use super::model::{CallPhase, Story, TextBlock, Turn, TurnCall, TurnKind};
use crate::record::CaptureRecord;

/// Outcome of observing one persisted capture record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnObserveOutcome {
    pub turn_id: Option<TurnId>,
    pub turn_index: Option<u32>,
}

/// Tracks turns for one story (single-writer, inside [`super::super::actors::StoryActor`]).
#[derive(Debug)]
pub struct TurnMachine {
    story_id: StoryId,
    agent_id: String,
    run_id: Option<super::ids::RunId>,
    turns: Vec<Turn>,
    active_turn: Option<TurnId>,
    next_index: u32,
}

impl TurnMachine {
    pub fn new(story_id: StoryId) -> Self {
        Self {
            story_id,
            agent_id: String::new(),
            run_id: None,
            turns: Vec::new(),
            active_turn: None,
            next_index: 0,
        }
    }

    pub fn with_context(
        story_id: StoryId,
        agent_id: impl Into<String>,
        run_id: Option<super::ids::RunId>,
    ) -> Self {
        Self {
            story_id,
            agent_id: agent_id.into(),
            run_id,
            turns: Vec::new(),
            active_turn: None,
            next_index: 0,
        }
    }

    pub fn set_story_meta(&mut self, agent_id: &str, run_id: Option<super::ids::RunId>) {
        if self.agent_id.is_empty() {
            self.agent_id = agent_id.to_string();
        }
        if self.run_id.is_none() {
            self.run_id = run_id;
        }
    }

    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }

    /// Build a read-model snapshot of the in-memory story state.
    pub fn snapshot(&self) -> Story {
        Story {
            story_id: self.story_id.clone(),
            run_id: self.run_id.clone(),
            agent_id: self.agent_id.clone(),
            parent: None,
            turns: self.turns.clone(),
        }
    }

    /// Replay persisted records into a fresh turn machine (offline egress).
    pub fn replay_records(
        story_id: StoryId,
        agent_id: impl Into<String>,
        run_id: Option<super::ids::RunId>,
        records: &[CaptureRecord],
    ) -> Self {
        let mut tm = Self::with_context(story_id, agent_id, run_id);
        for rec in records {
            let mut r = rec.clone();
            tm.observe_record(&mut r);
        }
        tm
    }

    /// Stamp `turn_id` / `turn_index` on the record and update in-memory story state.
    pub fn observe_record(&mut self, rec: &mut CaptureRecord) -> TurnObserveOutcome {
        let outcome = self.apply_record(rec);
        if let Some(ref turn_id) = outcome.turn_id {
            rec.payload["turn_id"] = json!(turn_id.as_str());
        }
        if let Some(idx) = outcome.turn_index {
            rec.payload["turn_index"] = json!(idx);
        }
        outcome
    }

    fn apply_record(&mut self, rec: &CaptureRecord) -> TurnObserveOutcome {
        let call_id = rec
            .call_id
            .as_deref()
            .map(CallId::new)
            .unwrap_or_else(|| CallId::new("unknown"));

        match rec.kind.as_str() {
            "llm.request" => self.on_request(&call_id, rec),
            "llm.response" | "llm.response.stream" => self.on_response(&call_id, rec),
            "llm.call.cancelled" => self.on_cancel(&call_id),
            _ => TurnObserveOutcome {
                turn_id: self.active_turn.clone(),
                turn_index: self.active_turn.as_ref().and_then(|t| self.turn_index(t)),
            },
        }
    }

    fn on_request(&mut self, call_id: &CallId, rec: &CaptureRecord) -> TurnObserveOutcome {
        if rec.is_internal_llm_request() {
            if self.active_turn.is_some() {
                self.ensure_call(call_id, CallPhase::Request);
            }
            return TurnObserveOutcome {
                turn_id: self.active_turn.clone(),
                turn_index: self.active_turn.as_ref().and_then(|t| self.turn_index(t)),
            };
        }

        if let Some(text) = rec.visible_user_text() {
            let turn_id = TurnId::new(&self.story_id, self.next_index);
            self.next_index += 1;
            let turn = Turn {
                turn_id: turn_id.clone(),
                index: self.next_index - 1,
                kind: TurnKind::Dialogue,
                user: Some(TextBlock {
                    text,
                    call_id: Some(call_id.clone()),
                }),
                assistant: None,
                calls: vec![self.call_snapshot(call_id)],
            };
            self.turns.push(turn);
            self.active_turn = Some(turn_id.clone());
            return TurnObserveOutcome {
                turn_id: Some(turn_id),
                turn_index: Some(self.next_index - 1),
            };
        }

        if self.active_turn.is_some() {
            self.ensure_call(call_id, CallPhase::Request);
            return TurnObserveOutcome {
                turn_id: self.active_turn.clone(),
                turn_index: self.active_turn.as_ref().and_then(|t| self.turn_index(t)),
            };
        }

        let turn_id = TurnId::new(&self.story_id, self.next_index);
        self.next_index += 1;
        let turn = Turn {
            turn_id: turn_id.clone(),
            index: self.next_index - 1,
            kind: TurnKind::Autonomous,
            user: None,
            assistant: None,
            calls: vec![self.call_snapshot(call_id)],
        };
        self.turns.push(turn);
        self.active_turn = Some(turn_id.clone());
        TurnObserveOutcome {
            turn_id: Some(turn_id),
            turn_index: Some(self.next_index - 1),
        }
    }

    fn on_response(&mut self, call_id: &CallId, rec: &CaptureRecord) -> TurnObserveOutcome {
        self.ensure_call(call_id, CallPhase::Complete);
        if let Some(text) = rec.visible_assistant_text() {
            if let Some(turn) = self.turns.last_mut() {
                turn.assistant = Some(TextBlock {
                    text,
                    call_id: Some(call_id.clone()),
                });
            }
        }
        TurnObserveOutcome {
            turn_id: self.active_turn.clone(),
            turn_index: self.active_turn.as_ref().and_then(|t| self.turn_index(t)),
        }
    }

    fn on_cancel(&mut self, call_id: &CallId) -> TurnObserveOutcome {
        self.ensure_call(call_id, CallPhase::Cancel);
        TurnObserveOutcome {
            turn_id: self.active_turn.clone(),
            turn_index: self.active_turn.as_ref().and_then(|t| self.turn_index(t)),
        }
    }

    fn ensure_call(&mut self, call_id: &CallId, kind: CallPhase) {
        if let Some(turn) = self.turns.last_mut() {
            if let Some(entry) = turn.calls.iter_mut().find(|c| c.call_id == *call_id) {
                if !entry.events.contains(&kind) {
                    entry.events.push(kind);
                }
                return;
            }
            turn.calls.push(TurnCall {
                call_id: call_id.clone(),
                trace_id: String::new(),
                protocol: None,
                model: None,
                events: vec![kind],
            });
        }
    }

    fn call_snapshot(&self, call_id: &CallId) -> TurnCall {
        TurnCall {
            call_id: call_id.clone(),
            trace_id: String::new(),
            protocol: None,
            model: None,
            events: vec![CallPhase::Request],
        }
    }

    fn turn_index(&self, turn_id: &TurnId) -> Option<u32> {
        self.turns
            .iter()
            .find(|t| t.turn_id == *turn_id)
            .map(|t| t.index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::model::CallPhase;
    use crate::config::CaptureLevel;
    use crate::sink::{llm_request_summary_record, llm_response_record_with_content};

    use crate::Call;

    fn sample_call(id: &str) -> Call {
        Call {
            call_id: id.into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn opens_turn_on_visible_user_request() {
        let story_id = StoryId::new("run|main");
        let mut tm = TurnMachine::new(story_id);
        let call = sample_call("c1");
        let mut rec = llm_request_summary_record(
            Some("s".into()),
            Some("a".into()),
            "m",
            "/v1/chat/completions",
            10,
            "chat_completions",
            "openai",
            Some("hello".into()),
            None,
            &call,
            CaptureLevel::Dialogue,
            None,
        );
        let out = tm.observe_record(&mut rec);
        assert!(out.turn_id.is_some());
        assert_eq!(tm.turns().len(), 1);
        assert_eq!(tm.turns()[0].kind, TurnKind::Dialogue);
        assert_eq!(tm.turns()[0].user.as_ref().unwrap().text, "hello");
        assert_eq!(
            rec.payload.get("turn_id").and_then(|v| v.as_str()),
            out.turn_id.as_ref().map(|t| t.as_str())
        );
    }

    #[test]
    fn attaches_assistant_to_active_turn() {
        let story_id = StoryId::new("run|main");
        let mut tm = TurnMachine::new(story_id);
        let call = sample_call("c1");
        let mut req = llm_request_summary_record(
            Some("s".into()),
            Some("a".into()),
            "m",
            "/v1/chat/completions",
            10,
            "chat_completions",
            "openai",
            Some("hi".into()),
            None,
            &call,
            CaptureLevel::Dialogue,
            None,
        );
        tm.observe_record(&mut req);

        let mut resp = llm_response_record_with_content(
            Some("s".into()),
            Some("a".into()),
            200,
            &json!({}),
            false,
            Some("ok".into()),
            &call,
            CaptureLevel::Dialogue,
        );
        tm.observe_record(&mut resp);
        assert_eq!(tm.turns()[0].assistant.as_ref().unwrap().text, "ok");
    }

    #[test]
    fn internal_request_does_not_overwrite_dialogue_kind() {
        let story_id = StoryId::new("run|main");
        let mut tm = TurnMachine::new(story_id);
        let user_call = sample_call("c-user");
        let mut user_req = llm_request_summary_record(
            Some("s".into()),
            Some("a".into()),
            "m",
            "/v1/chat/completions",
            10,
            "chat_completions",
            "openai",
            Some("hello".into()),
            None,
            &user_call,
            CaptureLevel::Dialogue,
            None,
        );
        tm.observe_record(&mut user_req);
        assert_eq!(tm.turns()[0].kind, TurnKind::Dialogue);

        let internal_call = sample_call("c-internal");
        let mut internal_req = llm_request_summary_record(
            Some("s".into()),
            Some("a".into()),
            "m",
            "/v1/messages/count_tokens",
            10,
            "count_tokens",
            "openai",
            None,
            None,
            &internal_call,
            CaptureLevel::Dialogue,
            None,
        );
        tm.observe_record(&mut internal_req);

        assert_eq!(tm.turns().len(), 1);
        assert_eq!(tm.turns()[0].kind, TurnKind::Dialogue);
        assert_eq!(tm.turns()[0].calls.len(), 2);
    }

    #[test]
    fn snapshot_reflects_turns() {
        let story_id = StoryId::new("run|main");
        let mut tm = TurnMachine::with_context(story_id.clone(), "agent-1", None);
        let call = sample_call("c1");
        let mut rec = llm_request_summary_record(
            Some("s".into()),
            Some("a".into()),
            "m",
            "/v1/chat/completions",
            10,
            "chat_completions",
            "openai",
            Some("hi".into()),
            None,
            &call,
            CaptureLevel::Dialogue,
            None,
        );
        tm.observe_record(&mut rec);
        let snap = tm.snapshot();
        assert_eq!(snap.story_id, story_id);
        assert_eq!(snap.agent_id, "agent-1");
        assert_eq!(snap.turns.len(), 1);
    }

    #[test]
    fn replay_records_matches_incremental_observe() {
        let story_id = StoryId::new("run|main");
        let mut live = TurnMachine::with_context(story_id.clone(), "agent-1", None);
        let mut records = Vec::new();

        for (id, text) in [("c1", "one"), ("c2", "two")] {
            let call = sample_call(id);
            let mut req = llm_request_summary_record(
                Some("s".into()),
                Some("a".into()),
                "m",
                "/v1/chat/completions",
                10,
                "chat_completions",
                "openai",
                Some(text.into()),
                None,
                &call,
                CaptureLevel::Dialogue,
                None,
            );
            live.observe_record(&mut req);
            records.push(req);

            let mut resp = llm_response_record_with_content(
                Some("s".into()),
                Some("a".into()),
                200,
                &json!({}),
                false,
                Some(format!("ok-{text}")),
                &call,
                CaptureLevel::Dialogue,
            );
            live.observe_record(&mut resp);
            records.push(resp);
        }

        let replayed =
            TurnMachine::replay_records(story_id.clone(), "agent-1", None, &records).snapshot();
        assert_eq!(replayed, live.snapshot());
    }

    #[test]
    fn cancel_records_call_phase() {
        let story_id = StoryId::new("run|main");
        let mut tm = TurnMachine::new(story_id);
        let call = sample_call("c1");
        let mut req = llm_request_summary_record(
            Some("s".into()),
            Some("a".into()),
            "m",
            "/v1/chat/completions",
            10,
            "chat_completions",
            "openai",
            Some("hi".into()),
            None,
            &call,
            CaptureLevel::Dialogue,
            None,
        );
        tm.observe_record(&mut req);

        let mut cancel = llm_response_record_with_content(
            Some("s".into()),
            Some("a".into()),
            499,
            &json!({ "cancelled": true }),
            true,
            None,
            &call,
            CaptureLevel::Dialogue,
        );
        cancel.kind = "llm.call.cancelled".into();
        tm.observe_record(&mut cancel);

        let phases = &tm.turns()[0].calls[0].events;
        assert!(phases.contains(&CallPhase::Request));
        assert!(phases.contains(&CallPhase::Cancel));
    }
}
