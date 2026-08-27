use crate::agent::{AssistantThread, ThreadRole, thread_storage_key};
use crate::model::RunSummary;

pub const SESSION_INDEX_KEY: &str = "pchronicle_copilot_index";
pub const SESSION_CAP: usize = 30;
pub const TITLE_LIMIT: usize = 72;

pub fn session_storage_key(id: &str) -> String {
    format!("pchronicle_copilot_session:{id}")
}

pub trait KvStore {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&self, key: &str, value: &str);
    fn remove(&self, key: &str);
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AssistantSessionMeta {
    pub id: String,
    pub run: RunSummary,
    pub title: String,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AssistantSessionIndex {
    #[serde(default)]
    pub sessions: Vec<AssistantSessionMeta>,
    #[serde(default)]
    pub active_id: Option<String>,
}

pub fn empty_thread() -> AssistantThread {
    AssistantThread {
        messages: Vec::new(),
        updated_at: 0,
        truncated: false,
    }
}

pub fn has_user_message(thread: &AssistantThread) -> bool {
    thread
        .messages
        .iter()
        .any(|message| message.role == ThreadRole::User && !message.text.trim().is_empty())
}

pub fn title_from_thread(thread: &AssistantThread) -> String {
    thread
        .messages
        .iter()
        .find(|message| message.role == ThreadRole::User && !message.text.trim().is_empty())
        .map(|message| truncate_title(&message.text))
        .unwrap_or_else(|| "New chat".into())
}

pub fn can_start_new_chat(run_selected: bool, thread: &AssistantThread) -> bool {
    run_selected && has_user_message(thread)
}

pub fn page_after_history_switch(current_page: &str) -> &'static str {
    match current_page {
        "tools" => "tools",
        "detail" => "detail",
        _ => "detail",
    }
}

pub fn upsert_session(
    index: &mut AssistantSessionIndex,
    meta: AssistantSessionMeta,
) -> Vec<String> {
    index.sessions.retain(|item| item.id != meta.id);
    index.active_id = Some(meta.id.clone());
    index.sessions.insert(0, meta);
    let mut evicted = Vec::new();
    while index.sessions.len() > SESSION_CAP {
        if let Some(old) = index.sessions.pop() {
            evicted.push(old.id);
        }
    }
    evicted
}

pub fn delete_session(index: &mut AssistantSessionIndex, id: &str) -> Option<String> {
    let run_query = index
        .sessions
        .iter()
        .find(|item| item.id == id)
        .map(|item| item.run.query());
    index.sessions.retain(|item| item.id != id);
    if index.active_id.as_deref() == Some(id) {
        index.active_id = run_query.and_then(|query| {
            index
                .sessions
                .iter()
                .find(|item| item.run.query() == query)
                .map(|item| item.id.clone())
        });
    }
    index.active_id.clone()
}

pub fn migrate_legacy_thread(
    index: &mut AssistantSessionIndex,
    run: &RunSummary,
    thread: &AssistantThread,
    new_id: String,
    now: i64,
) -> bool {
    if !has_user_message(thread) || latest_session_for_run(index, run).is_some() {
        return false;
    }
    upsert_session(
        index,
        AssistantSessionMeta {
            id: new_id,
            run: run.clone(),
            title: title_from_thread(thread),
            updated_at: now,
        },
    );
    true
}

pub fn latest_session_for_run<'a>(
    index: &'a AssistantSessionIndex,
    run: &RunSummary,
) -> Option<&'a AssistantSessionMeta> {
    index
        .sessions
        .iter()
        .find(|item| item.run.query() == run.query())
}

pub fn relative_time(now: i64, then: i64) -> String {
    let delta = now.saturating_sub(then).max(0);
    if delta < 60_000 {
        "just now".into()
    } else if delta < 3_600_000 {
        format!("{}m ago", delta / 60_000)
    } else if delta < 86_400_000 {
        format!("{}h ago", delta / 3_600_000)
    } else {
        format!("{}d ago", delta / 86_400_000)
    }
}

fn truncate_title(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= TITLE_LIMIT {
        return trimmed.to_string();
    }
    let mut out: String = trimmed
        .chars()
        .take(TITLE_LIMIT.saturating_sub(1))
        .collect();
    out.push('…');
    out
}

pub fn load_index(store: &impl KvStore) -> AssistantSessionIndex {
    store
        .get(SESSION_INDEX_KEY)
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save_index(store: &impl KvStore, index: &AssistantSessionIndex) {
    if let Ok(raw) = serde_json::to_string(index) {
        store.set(SESSION_INDEX_KEY, &raw);
    }
}

pub fn load_session_thread(store: &impl KvStore, id: &str) -> AssistantThread {
    store
        .get(&session_storage_key(id))
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(empty_thread)
}

pub fn save_session_thread(store: &impl KvStore, id: &str, thread: &AssistantThread) {
    if let Ok(raw) = serde_json::to_string(thread) {
        store.set(&session_storage_key(id), &raw);
    }
}

pub fn persist_indexed_thread(
    store: &impl KvStore,
    index: &mut AssistantSessionIndex,
    session_id: &str,
    run: &RunSummary,
    thread: &AssistantThread,
    now: i64,
) {
    save_session_thread(store, session_id, thread);
    if !has_user_message(thread) {
        return;
    }
    let evicted = upsert_session(
        index,
        AssistantSessionMeta {
            id: session_id.to_string(),
            run: run.clone(),
            title: title_from_thread(thread),
            updated_at: now,
        },
    );
    for id in evicted {
        store.remove(&session_storage_key(&id));
    }
    save_index(store, index);
}

pub fn restore_for_run(
    store: &impl KvStore,
    run: &RunSummary,
    new_id: &str,
    now: i64,
) -> (AssistantSessionIndex, Option<String>, AssistantThread) {
    let mut index = load_index(store);
    if let Some(meta) = latest_session_for_run(&index, run).cloned() {
        let thread = load_session_thread(store, &meta.id);
        index.active_id = Some(meta.id.clone());
        save_index(store, &index);
        return (index, Some(meta.id), thread);
    }
    let legacy = store
        .get(&thread_storage_key(run))
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(empty_thread);
    if migrate_legacy_thread(&mut index, run, &legacy, new_id.to_string(), now) {
        save_session_thread(store, new_id, &legacy);
        store.remove(&thread_storage_key(run));
        save_index(store, &index);
        return (index, Some(new_id.to_string()), legacy);
    }
    (index, None, empty_thread())
}

pub fn new_session_id(now: i64) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    format!("c-{now}-{}", SEQ.fetch_add(1, Ordering::Relaxed))
}

pub fn now_millis() -> i64 {
    use web_time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(1)
        .max(1)
}

pub struct BrowserStore;

impl KvStore for BrowserStore {
    fn get(&self, key: &str) -> Option<String> {
        web_sys::window()?
            .local_storage()
            .ok()
            .flatten()?
            .get_item(key)
            .ok()
            .flatten()
    }

    fn set(&self, key: &str, value: &str) {
        let Some(storage) =
            web_sys::window().and_then(|window| window.local_storage().ok().flatten())
        else {
            return;
        };
        let _ = storage.set_item(key, value);
    }

    fn remove(&self, key: &str) {
        let Some(storage) =
            web_sys::window().and_then(|window| window.local_storage().ok().flatten())
        else {
            return;
        };
        let _ = storage.remove_item(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ThreadMessage;

    fn sample_run(session: &str) -> RunSummary {
        RunSummary {
            dataset: "captures".into(),
            file: "events.lance".into(),
            run_id: Some(session.into()),
            agent_id: "agent".into(),
            model_name: None,
            session_id: session.into(),
            root_session_id: None,
            path: String::new(),
            row_count: 1,
            duplicate_event_ids: 0,
            status: "completed".into(),
        }
    }

    fn user(text: &str) -> ThreadMessage {
        ThreadMessage {
            role: ThreadRole::User,
            text: text.into(),
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            sql: None,
            truncated: false,
            reasoning_content: None,
        }
    }

    fn thread(messages: Vec<ThreadMessage>) -> AssistantThread {
        AssistantThread {
            messages,
            updated_at: 1,
            truncated: false,
        }
    }

    fn meta(id: &str, session: &str, title: &str, updated_at: i64) -> AssistantSessionMeta {
        AssistantSessionMeta {
            id: id.into(),
            run: sample_run(session),
            title: title.into(),
            updated_at,
        }
    }

    #[test]
    fn empty_draft_is_not_indexed_and_cannot_start_another_chat() {
        let draft = thread(Vec::new());
        assert!(!has_user_message(&draft));
        assert!(!can_start_new_chat(true, &draft));
        assert!(can_start_new_chat(
            true,
            &thread(vec![user("why did step 4 fail?")])
        ));
        assert!(!can_start_new_chat(false, &thread(vec![user("hello")])));
    }

    #[test]
    fn title_uses_the_first_user_message() {
        let chat = thread(vec![
            user("   Why did the retry loop explode after tool 12?   "),
            ThreadMessage {
                role: ThreadRole::Assistant,
                text: "looking".into(),
                tool_calls: None,
                tool_call_id: None,
                tool_name: None,
                sql: None,
                truncated: false,
                reasoning_content: None,
            },
        ]);
        assert_eq!(
            title_from_thread(&chat),
            "Why did the retry loop explode after tool 12?"
        );
        assert_eq!(title_from_thread(&thread(Vec::new())), "New chat");
    }

    #[test]
    fn history_keeps_analyze_or_detail_and_opens_detail_from_lists() {
        assert_eq!(page_after_history_switch("tools"), "tools");
        assert_eq!(page_after_history_switch("detail"), "detail");
        assert_eq!(page_after_history_switch("catalog"), "detail");
        assert_eq!(page_after_history_switch("runs"), "detail");
    }

    #[test]
    fn upsert_moves_to_front_and_evicts_past_cap() {
        let mut index = AssistantSessionIndex::default();
        let mut evicted = Vec::new();
        for i in 0..(SESSION_CAP + 2) {
            evicted.extend(upsert_session(
                &mut index,
                meta(
                    &format!("s{i}"),
                    &format!("run-{i}"),
                    &format!("chat {i}"),
                    i as i64,
                ),
            ));
        }
        assert_eq!(index.sessions.len(), SESSION_CAP);
        assert_eq!(index.sessions[0].id, format!("s{}", SESSION_CAP + 1));
        assert_eq!(evicted, vec!["s0".to_string(), "s1".to_string()]);
        assert_eq!(index.active_id.as_deref(), Some("s31"));
    }

    #[test]
    fn migrate_wraps_legacy_thread_once_per_run() {
        let mut index = AssistantSessionIndex::default();
        let run_a = sample_run("sess-a");
        let chat = thread(vec![user("explain turn 3")]);
        assert!(migrate_legacy_thread(
            &mut index,
            &run_a,
            &chat,
            "new-1".into(),
            10
        ));
        assert!(!migrate_legacy_thread(
            &mut index,
            &run_a,
            &chat,
            "new-2".into(),
            11
        ));
        assert!(!migrate_legacy_thread(
            &mut index,
            &sample_run("sess-b"),
            &thread(Vec::new()),
            "new-3".into(),
            12
        ));
        assert_eq!(index.sessions.len(), 1);
        assert_eq!(index.sessions[0].id, "new-1");
        assert_eq!(index.sessions[0].title, "explain turn 3");
    }

    #[test]
    fn deleting_active_session_falls_back_to_same_run() {
        let mut index = AssistantSessionIndex {
            sessions: vec![
                meta("keep", "run-a", "older", 1),
                meta("gone", "run-a", "newer", 2),
                meta("other", "run-b", "else", 3),
            ],
            active_id: Some("gone".into()),
        };
        assert_eq!(delete_session(&mut index, "gone").as_deref(), Some("keep"));
        assert_eq!(
            index
                .sessions
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            ["keep", "other"]
        );
        let mut index = AssistantSessionIndex {
            sessions: vec![meta("only", "run-a", "solo", 1)],
            active_id: Some("only".into()),
        };
        assert_eq!(delete_session(&mut index, "only"), None);
        assert!(index.sessions.is_empty());
    }

    #[test]
    fn latest_session_prefers_the_first_matching_run() {
        let index = AssistantSessionIndex {
            sessions: vec![
                meta("b1", "run-b", "b", 3),
                meta("a2", "run-a", "newer a", 2),
                meta("a1", "run-a", "older a", 1),
            ],
            active_id: Some("b1".into()),
        };
        assert_eq!(
            latest_session_for_run(&index, &sample_run("run-a"))
                .unwrap()
                .id,
            "a2"
        );
    }

    #[test]
    fn relative_time_uses_compact_units() {
        assert_eq!(relative_time(10_000, 9_000), "just now");
        assert_eq!(relative_time(120_000, 0), "2m ago");
        assert_eq!(relative_time(3_600_000 * 3, 0), "3h ago");
        assert_eq!(relative_time(86_400_000 * 4, 0), "4d ago");
    }

    #[test]
    fn session_keys_stay_namespaced() {
        assert_eq!(session_storage_key("abc"), "pchronicle_copilot_session:abc");
        assert!(thread_storage_key(&sample_run("s")).starts_with("pchronicle_copilot:"));
    }
}
