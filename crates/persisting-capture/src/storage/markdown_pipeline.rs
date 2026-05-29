//! CaptureRecord → session markdown: eligibility, block build, live upsert.
//!
//! All paths (live `-f md`, materialize, reconcile) go through [`MarkdownPipeline`].
//! Live session actors hold [`LiveMarkdownWriter`] (pipeline + target path + upsert).

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

use super::dialogue::capture_record_to_block;
use super::dialogue_extract::count_visible_user_messages;
use super::markdown::{upsert_block_by_call_id, BlockHeader};
use super::markdown_policy::should_skip_record;
use super::record::CaptureRecord;
use super::session::{trajectory_run_dir, CaptureRoute};
use crate::markdown_trajectory::session_markdown_write_path_for_key;

/// Per-session sequential state: static filters + Claude Code history-replay dedup.
#[derive(Debug, Default)]
pub struct MarkdownPipeline {
    last_user_message_count: usize,
    skipped_call_ids: HashSet<String>,
}

impl MarkdownPipeline {
    pub fn static_skip(rec: &CaptureRecord) -> bool {
        should_skip_record(rec)
    }

    pub fn should_skip(&mut self, rec: &CaptureRecord) -> bool {
        if self.skip_history_replay(rec) {
            return true;
        }
        Self::static_skip(rec)
    }

    pub fn skips_draft(&self, call_id: &str) -> bool {
        self.skipped_call_ids.contains(call_id)
    }

    pub fn try_block(&mut self, rec: &CaptureRecord) -> Result<Option<(BlockHeader, Vec<u8>)>> {
        if self.should_skip(rec) {
            return Ok(None);
        }
        Ok(Some(capture_record_to_block(rec)?))
    }

    pub fn blocks_from_records(records: &[CaptureRecord]) -> Result<Vec<(BlockHeader, Vec<u8>)>> {
        let mut pipeline = Self::default();
        let mut blocks = Vec::new();
        for rec in records {
            if let Some(block) = pipeline.try_block(rec)? {
                blocks.push(block);
            }
        }
        Ok(blocks)
    }

    pub fn call_ids_from_records(records: &[CaptureRecord]) -> BTreeSet<String> {
        let mut pipeline = Self::default();
        let mut ids = BTreeSet::new();
        for rec in records {
            if pipeline.should_skip(rec) {
                continue;
            }
            if let Some(id) = rec.call_id.as_deref().filter(|s| !s.is_empty()) {
                ids.insert(id.to_string());
            }
        }
        ids
    }

    fn skip_history_replay(&mut self, rec: &CaptureRecord) -> bool {
        match rec.kind.as_str() {
            "llm.request" => {
                // Codex / Responses tool rounds resend the full `input` array; visible user
                // turn count stays flat while `user_content` is a new tool_result block.
                if is_tool_result_continuation(rec) {
                    return false;
                }
                let Some(count) = rec
                    .payload
                    .get("user_message_count")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                else {
                    return false;
                };
                if count <= self.last_user_message_count {
                    if let Some(cid) = &rec.call_id {
                        self.skipped_call_ids.insert(cid.clone());
                    }
                    return true;
                }
                self.last_user_message_count = count;
                false
            }
            "llm.response" | "llm.response.stream" => rec
                .call_id
                .as_ref()
                .is_some_and(|cid| self.skipped_call_ids.contains(cid)),
            _ => false,
        }
    }
}

/// Responses / Codex tool-output follow-up (not Claude history replay).
fn is_tool_result_continuation(rec: &CaptureRecord) -> bool {
    rec.visible_user_text()
        .is_some_and(|t| t.contains("```tool_result:"))
}

/// Where live markdown for one session is written.
#[derive(Debug, Clone)]
pub struct MarkdownTarget {
    pub route: CaptureRoute,
    pub agent_id: String,
    pub storage: PathBuf,
}

impl MarkdownTarget {
    pub fn new(route: CaptureRoute, agent_id: impl Into<String>, storage: PathBuf) -> Self {
        Self {
            route,
            agent_id: agent_id.into(),
            storage,
        }
    }

    pub fn path(&self) -> PathBuf {
        let run_dir = trajectory_run_dir(self.storage.as_path(), &self.agent_id, &self.route);
        session_markdown_write_path_for_key(&run_dir, &self.route.storage_session_id)
    }
}

/// Live markdown upsert for one capture session (`-f md`).
#[derive(Debug)]
pub struct LiveMarkdownWriter {
    target: MarkdownTarget,
    enabled: bool,
    pipeline: MarkdownPipeline,
}

impl LiveMarkdownWriter {
    pub fn new(target: MarkdownTarget, enabled: bool) -> Self {
        Self {
            target,
            enabled,
            pipeline: MarkdownPipeline::default(),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.target.path()
    }

    pub fn write_record(&mut self, rec: &CaptureRecord) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let Some(block) = self.pipeline.try_block(rec)? else {
            return Ok(());
        };
        let call_id = rec.call_id.as_deref().unwrap_or("");
        let path = self.target.path();
        upsert_block_by_call_id(&path, call_id, block)
            .with_context(|| format!("markdown upsert {}", path.display()))?;
        Ok(())
    }

    pub fn write_draft(&mut self, call_id: &str, block: (BlockHeader, Vec<u8>)) -> Result<()> {
        if !self.enabled || self.pipeline.skips_draft(call_id) {
            return Ok(());
        }
        let path = self.target.path();
        upsert_block_by_call_id(&path, call_id, block)
            .with_context(|| format!("markdown draft upsert {}", path.display()))?;
        Ok(())
    }
}

/// Stamp request payload fields consumed by [`MarkdownPipeline`].
pub fn stamp_request_payload(payload: &mut Value, body_json: Option<&Value>) {
    if let Some(body) = body_json {
        payload["user_message_count"] = serde_json::json!(count_visible_user_messages(body));
    }
}

/// Alias kept for tests and CLI view helpers.
pub fn skip_markdown_block(rec: &CaptureRecord) -> bool {
    should_skip_record(rec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CaptureLevel;
    use crate::markdown_trajectory::read_blocks_from_file;
    use crate::session_storage::CaptureRoute;
    use crate::sink::{llm_request_summary_record, llm_response_record_with_content};
    use crate::Call;
    use serde_json::json;

    const LEVEL: CaptureLevel = CaptureLevel::Dialogue;

    fn test_call(id: &str) -> Call {
        Call {
            call_id: id.into(),
            trace_id: id.into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn route() -> CaptureRoute {
        CaptureRoute {
            root_session: Some("run-test".into()),
            session_id: "sess-1".into(),
            storage_session_id: "sess-1".into(),
            subagent_id: None,
        }
    }

    fn request(
        id: &str,
        user: &str,
        user_message_count: u64,
        body: Option<Value>,
    ) -> CaptureRecord {
        let mut rec = llm_request_summary_record(
            Some("sess-1".into()),
            Some("agent-1".into()),
            "deepseek-v4-pro",
            "/v1/messages",
            100,
            "messages",
            "anthropic",
            Some(user.into()),
            None,
            &test_call(id),
            LEVEL,
            body.as_ref(),
        );
        rec.payload["user_message_count"] = json!(user_message_count);
        rec
    }

    fn response(id: &str, text: &str) -> CaptureRecord {
        llm_response_record_with_content(
            Some("sess-1".into()),
            Some("agent-1".into()),
            200,
            &json!({"status": 200}),
            true,
            Some(text.into()),
            &test_call(id),
            LEVEL,
        )
    }

    fn messages_body(user_lines: &[&str]) -> Value {
        let messages: Vec<Value> = user_lines
            .iter()
            .map(|line| {
                json!({
                    "role": "user",
                    "content": [{"type": "text", "text": line}]
                })
            })
            .collect();
        json!({"messages": messages, "model": "deepseek-v4-pro"})
    }

    #[test]
    fn stamp_request_payload_sets_user_message_count() {
        let body = messages_body(&["hi", "你好"]);
        let mut payload = json!({});
        stamp_request_payload(&mut payload, Some(&body));
        assert_eq!(payload["user_message_count"], json!(2));
    }

    #[test]
    fn stamp_request_payload_noop_without_body() {
        let mut payload = json!({"model": "m"});
        stamp_request_payload(&mut payload, None);
        assert!(payload.get("user_message_count").is_none());
    }

    #[test]
    fn sink_summary_record_stamps_count_from_body_json() {
        let body = messages_body(&["a", "b", "c"]);
        let rec = llm_request_summary_record(
            Some("s".into()),
            None,
            "m",
            "/v1/messages",
            1,
            "messages",
            "anthropic",
            Some("c".into()),
            None,
            &test_call("c1"),
            LEVEL,
            Some(&body),
        );
        assert_eq!(rec.payload["user_message_count"], json!(3));
    }

    #[test]
    fn static_skip_internal_count_tokens_and_empty_turns() {
        let count_tokens = llm_request_summary_record(
            Some("s".into()),
            None,
            "m",
            "/v1/messages/count_tokens",
            1,
            "count_tokens",
            "anthropic",
            Some("ctx".into()),
            None,
            &test_call("c1"),
            LEVEL,
            None,
        );
        assert!(skip_markdown_block(&count_tokens));

        let empty_user = request("c2", "   ", 1, None);
        assert!(skip_markdown_block(&empty_user));

        let empty_resp = llm_response_record_with_content(
            Some("s".into()),
            None,
            200,
            &json!({"status": 200}),
            true,
            Some("".into()),
            &test_call("c3"),
            LEVEL,
        );
        assert!(skip_markdown_block(&empty_resp));

        let mut partial = response("c4", "wip");
        partial.payload["stream_partial"] = json!(true);
        assert!(skip_markdown_block(&partial));
    }

    #[test]
    fn static_skip_spawn_link_is_visible() {
        let mut rec = response("c1", "linked");
        rec.kind = "llm.spawn_link".into();
        assert!(!skip_markdown_block(&rec));
    }

    #[test]
    fn intentional_duplicate_user_text_with_increasing_count_both_kept() {
        let mut p = MarkdownPipeline::default();
        assert!(p
            .try_block(&request("c1", "hi", 1, None))
            .unwrap()
            .is_some());
        assert!(p.try_block(&response("c1", "Hello")).unwrap().is_some());
        assert!(p
            .try_block(&request("c2", "hi", 2, None))
            .unwrap()
            .is_some());
        assert!(p.try_block(&response("c2", "Hi again")).unwrap().is_some());
    }

    #[test]
    fn skips_history_replay_without_new_user_turn() {
        let mut p = MarkdownPipeline::default();
        assert!(p
            .try_block(&request("c1", "hi", 1, None))
            .unwrap()
            .is_some());
        assert!(p.try_block(&response("c1", "Hello")).unwrap().is_some());

        let replay = request("c3", "hi", 1, None);
        assert!(p.try_block(&replay).unwrap().is_none());
        assert!(p.skips_draft("c3"));
        assert!(p.try_block(&response("c3", "internal")).unwrap().is_none());
    }

    #[test]
    fn codex_tool_result_round_keeps_assistant_when_user_turn_count_flat() {
        let mut tool_req = request("c-tool", "x", 2, None);
        tool_req.payload["user_content"] =
            json!("```tool_result:call_x\nchunk output\n```");

        let mut p = MarkdownPipeline::default();
        assert!(p
            .try_block(&request("c1", "hi", 1, None))
            .unwrap()
            .is_some());
        assert!(p.try_block(&response("c1", "Hello")).unwrap().is_some());
        assert!(p
            .try_block(&request("c2", "review", 2, None))
            .unwrap()
            .is_some());
        assert!(p.try_block(&response("c2", "running tools")).unwrap().is_some());
        assert!(p.try_block(&tool_req).unwrap().is_some());
        assert!(p
            .try_block(&response("c-tool", "Let me dig in."))
            .unwrap()
            .is_some());
        assert!(p
            .try_block(&response("c-final", "Full design review."))
            .unwrap()
            .is_some());
    }

    #[test]
    fn claude_code_three_turn_session_materializes_visible_dialogue_only() {
        let records = vec![
            request("call-1", "hi", 1, None),
            response("call-1", "Hi! What can I help you with?"),
            request("call-2", "hi", 2, None),
            response("call-2", "Hi! What can I help you with?"),
            request("call-3", "hi", 2, None),
            response(
                "call-3",
                "what's the status of the capture storage feature?",
            ),
            request("call-4", "你好", 3, None),
            response("call-4", "你好！有什么我可以帮你的吗？"),
        ];
        let blocks = MarkdownPipeline::blocks_from_records(&records).unwrap();
        let bodies: Vec<_> = blocks
            .iter()
            .map(|(_, b)| std::str::from_utf8(b).unwrap().to_string())
            .collect();
        assert_eq!(
            bodies,
            vec![
                "hi".to_string(),
                "Hi! What can I help you with?".to_string(),
                "hi".to_string(),
                "Hi! What can I help you with?".to_string(),
                "你好".to_string(),
                "你好！有什么我可以帮你的吗？".to_string(),
            ]
        );
    }

    #[test]
    fn call_ids_from_records_matches_blocks_from_records() {
        let records = vec![
            request("call-1", "hi", 1, None),
            response("call-1", "a"),
            request("call-2", "replay", 1, None),
            response("call-2", "orphan"),
            request("call-3", "next", 2, None),
            response("call-3", "b"),
        ];
        let ids = MarkdownPipeline::call_ids_from_records(&records);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("call-1"));
        assert!(!ids.contains("call-2"));
        assert!(ids.contains("call-3"));
    }

    #[test]
    fn skip_main_flash_companion_on_main_session() {
        let rec = llm_request_summary_record(
            Some("sess".into()),
            Some("proxy".into()),
            "deepseek-v4-flash",
            "/v1/messages",
            100,
            "messages",
            "anthropic",
            Some("再次开三个subagent".into()),
            None,
            &test_call("c1"),
            LEVEL,
            None,
        );
        assert!(skip_markdown_block(&rec));
    }

    #[test]
    fn live_writer_disabled_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let target = MarkdownTarget::new(route(), "agent-1", dir.path().to_path_buf());
        let mut writer = LiveMarkdownWriter::new(target, false);
        writer.write_record(&request("c1", "hi", 1, None)).unwrap();
        let path = MarkdownTarget::new(route(), "agent-1", dir.path().to_path_buf()).path();
        assert!(!path.exists());
    }

    #[test]
    fn live_writer_persists_dialogue_and_skips_replay() {
        let dir = tempfile::tempdir().unwrap();
        let target = MarkdownTarget::new(route(), "agent-1", dir.path().to_path_buf());
        let path = target.path();
        let mut writer = LiveMarkdownWriter::new(target, true);

        writer.write_record(&request("c1", "hi", 1, None)).unwrap();
        writer.write_record(&response("c1", "Hello")).unwrap();
        writer.write_record(&request("c2", "hi", 2, None)).unwrap();
        writer.write_record(&response("c2", "Hi again")).unwrap();
        writer.write_record(&request("c3", "hi", 2, None)).unwrap();
        writer
            .write_record(&response(
                "c3",
                "what's the status of the capture storage feature?",
            ))
            .unwrap();
        writer
            .write_record(&request("c4", "你好", 3, None))
            .unwrap();
        writer.write_record(&response("c4", "你好！")).unwrap();

        let blocks = read_blocks_from_file(&path).unwrap();
        assert_eq!(blocks.len(), 6);
        assert_eq!(blocks[0].value_utf8().unwrap(), "hi");
        assert_eq!(blocks[1].value_utf8().unwrap(), "Hello");
        assert_eq!(blocks[2].value_utf8().unwrap(), "hi");
        assert_eq!(blocks[3].value_utf8().unwrap(), "Hi again");
        assert_eq!(blocks[4].value_utf8().unwrap(), "你好");
        assert_eq!(blocks[5].value_utf8().unwrap(), "你好！");
    }

    #[test]
    fn live_writer_skips_draft_for_replayed_call_id() {
        let dir = tempfile::tempdir().unwrap();
        let target = MarkdownTarget::new(route(), "agent-1", dir.path().to_path_buf());
        let path = target.path();
        let mut writer = LiveMarkdownWriter::new(target, true);

        writer.write_record(&request("c1", "hi", 1, None)).unwrap();
        writer.write_record(&request("c2", "hi", 1, None)).unwrap();

        let draft_block = capture_record_to_block(&response("c2", "draft text")).unwrap();
        writer.write_draft("c2", draft_block).unwrap();

        let blocks = read_blocks_from_file(&path).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0]
                .header
                .fields
                .get("call_id")
                .and_then(|v| v.as_str()),
            Some("c1")
        );
    }
}
