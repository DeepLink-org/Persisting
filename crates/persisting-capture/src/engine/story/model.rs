//! Read model: Story → Turn, with call/event detail in each turn.

use serde::{Deserialize, Serialize};

use super::ids::{CallId, RunId, StoryId, TurnId};
use crate::protocol::ProtocolKind;

/// Link from a subagent story back to the spawn point in a parent story.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoryLink {
    pub parent_story_id: StoryId,
    pub spawn_call_id: CallId,
    /// Turn index in the parent story when the subagent was spawned (0-based).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_turn_index: Option<u32>,
    #[serde(default)]
    pub relation: StoryLinkRelation,
}

/// How a child story relates to its parent link point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StoryLinkRelation {
    #[default]
    Spawn,
    MergeBack,
}

/// One agent's full narrative line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Story {
    pub story_id: StoryId,
    pub run_id: Option<RunId>,
    pub agent_id: String,
    pub parent: Option<StoryLink>,
    pub turns: Vec<Turn>,
}

/// One readable user→assistant round within a story.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub turn_id: TurnId,
    pub index: u32,
    pub kind: TurnKind,
    pub user: Option<TextBlock>,
    pub assistant: Option<TextBlock>,
    pub calls: Vec<TurnCall>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnKind {
    /// Visible user → assistant round.
    Dialogue,
    /// Agent-initiated stretch without user text on the opening request (tool loops,
    /// system-only continuations, autonomous work — not limited to tool-call rounds).
    #[serde(alias = "tool_loop")]
    Autonomous,
    /// Reserved for internal / non-dialogue indexing (currently unused).
    Internal,
}

/// Visible text (user or assistant).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextBlock {
    pub text: String,
    pub call_id: Option<CallId>,
}

/// One call's event timeline within a turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnCall {
    pub call_id: CallId,
    pub trace_id: String,
    pub protocol: Option<ProtocolKind>,
    pub model: Option<String>,
    pub events: Vec<CallPhase>,
}

/// Phase of one call within a turn (read model; distinct from [`CaptureRecord::kind`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallPhase {
    Request,
    Draft,
    Complete,
    Cancel,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_phase_serializes_snake_case() {
        let phase = CallPhase::Complete;
        let json = serde_json::to_string(&phase).unwrap();
        assert_eq!(json, "\"complete\"");
        let back: CallPhase = serde_json::from_str(&json).unwrap();
        assert_eq!(back, CallPhase::Complete);
    }

    #[test]
    fn turn_kind_deserializes_legacy_tool_loop_alias() {
        let kind: TurnKind = serde_json::from_str("\"tool_loop\"").unwrap();
        assert_eq!(kind, TurnKind::Autonomous);
    }
}
