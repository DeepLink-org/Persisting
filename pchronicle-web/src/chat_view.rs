//! Frontend-only grouping of Storyline turns into Chats vs Steps.

use crate::model::TurnSummary;

#[derive(Clone, Debug, PartialEq)]
pub enum TraceCard {
    Chat {
        user: Option<TurnSummary>,
        replies: Vec<TurnSummary>,
    },
    System {
        turn: TurnSummary,
    },
}

impl TraceCard {
    pub fn contains_turn(&self, id: i64) -> bool {
        match self {
            Self::Chat { user, replies } => {
                user.as_ref().is_some_and(|turn| turn.id == id)
                    || replies.iter().any(|turn| turn.id == id)
            }
            Self::System { turn } => turn.id == id,
        }
    }
}

pub fn normalize_trace_view(value: &str) -> &'static str {
    if value == "steps" {
        "steps"
    } else {
        "chats"
    }
}

pub fn group_chats(turns: &[TurnSummary]) -> Vec<TraceCard> {
    let mut cards = Vec::new();
    let mut index = 0;
    while index < turns.len() {
        match turns[index].source.as_str() {
            "system" => {
                cards.push(TraceCard::System {
                    turn: turns[index].clone(),
                });
                index += 1;
            }
            "user" => {
                let user = turns[index].clone();
                index += 1;
                let mut replies = Vec::new();
                while index < turns.len() && turns[index].source == "agent" {
                    replies.push(turns[index].clone());
                    index += 1;
                }
                cards.push(TraceCard::Chat {
                    user: Some(user),
                    replies,
                });
            }
            _ => {
                cards.push(TraceCard::Chat {
                    user: None,
                    replies: vec![turns[index].clone()],
                });
                index += 1;
            }
        }
    }
    cards
}

pub fn source_class(source: &str) -> &'static str {
    match source {
        "user" => "user",
        "system" => "system",
        _ => "agent",
    }
}

pub fn turn_matches_query(turn: &TurnSummary, query: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let needle = query.to_ascii_lowercase();
    turn.preview.to_ascii_lowercase().contains(&needle)
        || turn.source.to_ascii_lowercase().contains(&needle)
        || turn.id.to_string() == needle
        || turn
            .tool_names
            .iter()
            .any(|name| name.to_ascii_lowercase().contains(&needle))
        || turn
            .kind
            .as_deref()
            .is_some_and(|kind| kind.to_ascii_lowercase().contains(&needle))
}

pub fn chat_row_visible(entries: &[TurnSummary], source: &str, query: &str) -> bool {
    let source_ok =
        source == "all" || source.is_empty() || entries.iter().any(|turn| turn.source == source);
    let query_ok =
        query.trim().is_empty() || entries.iter().any(|turn| turn_matches_query(turn, query));
    source_ok && query_ok
}

pub fn step_row_visible(turn: &TurnSummary, source: &str, query: &str) -> bool {
    (source == "all" || source.is_empty() || turn.source == source)
        && turn_matches_query(turn, query)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(id: i64, source: &str) -> TurnSummary {
        TurnSummary {
            id,
            source: source.into(),
            kind: None,
            timestamp: None,
            call_id: None,
            preview: format!("{source}-{id}"),
            char_count: 0,
            modalities: Vec::new(),
            model_name: None,
            latency_ms: None,
            ttft_ms: None,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            tool_names: Vec::new(),
            event_seqs: Vec::new(),
            has_error: false,
        }
    }

    fn chat_ids(card: &TraceCard) -> (Option<i64>, Vec<i64>) {
        match card {
            TraceCard::Chat { user, replies } => (
                user.as_ref().map(|turn| turn.id),
                replies.iter().map(|turn| turn.id).collect(),
            ),
            TraceCard::System { turn } => panic!("expected chat, got system {}", turn.id),
        }
    }

    #[test]
    fn unknown_and_legacy_tree_views_become_chats() {
        assert_eq!(normalize_trace_view("chats"), "chats");
        assert_eq!(normalize_trace_view("steps"), "steps");
        assert_eq!(normalize_trace_view("tree"), "chats");
        assert_eq!(normalize_trace_view(""), "chats");
    }

    #[test]
    fn user_opens_a_chat_and_consumes_following_agents() {
        let cards = group_chats(&[
            turn(1, "user"),
            turn(2, "agent"),
            turn(3, "agent"),
            turn(4, "user"),
            turn(5, "agent"),
        ]);
        assert_eq!(cards.len(), 2);
        assert_eq!(chat_ids(&cards[0]), (Some(1), vec![2, 3]));
        assert_eq!(chat_ids(&cards[1]), (Some(4), vec![5]));
        assert!(cards[0].contains_turn(3));
        assert!(!cards[0].contains_turn(4));
    }

    #[test]
    fn leading_agents_and_mid_system_stay_separate() {
        let cards = group_chats(&[
            turn(1, "agent"),
            turn(2, "agent"),
            turn(3, "user"),
            turn(4, "agent"),
            turn(5, "system"),
            turn(6, "agent"),
        ]);
        assert_eq!(cards.len(), 5);
        assert_eq!(chat_ids(&cards[0]), (None, vec![1]));
        assert_eq!(chat_ids(&cards[1]), (None, vec![2]));
        assert_eq!(chat_ids(&cards[2]), (Some(3), vec![4]));
        match &cards[3] {
            TraceCard::System { turn } => assert_eq!(turn.id, 5),
            other => panic!("expected system, got {other:?}"),
        }
        assert_eq!(chat_ids(&cards[4]), (None, vec![6]));
    }

    #[test]
    fn consecutive_users_each_open_a_chat() {
        let cards = group_chats(&[turn(1, "user"), turn(2, "user"), turn(3, "agent")]);
        assert_eq!(cards.len(), 2);
        assert_eq!(chat_ids(&cards[0]), (Some(1), Vec::<i64>::new()));
        assert_eq!(chat_ids(&cards[1]), (Some(2), vec![3]));
    }

    #[test]
    fn source_and_query_filters_keep_the_whole_chat() {
        let user = turn(1, "user");
        let mut agent = turn(2, "agent");
        agent.preview = "look up GOOGL".into();
        let entries = [user, agent];
        assert!(chat_row_visible(&entries, "agent", ""));
        assert!(chat_row_visible(&entries, "user", "googl"));
        assert!(!chat_row_visible(&entries, "system", ""));
        assert!(!chat_row_visible(&entries, "all", "missing"));
        assert!(step_row_visible(&entries[1], "agent", "googl"));
        assert!(!step_row_visible(&entries[0], "agent", ""));
    }
}
