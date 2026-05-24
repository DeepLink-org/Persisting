//! Main agent ↔ subagent cross-reference (`spawn_links`, `refs_subagent_ids`).

use axum::http::HeaderMap;
use persisting_capture::dialogue::skip_markdown_block;
use persisting_capture::record::CaptureRecord;
use persisting_capture::session_storage::CaptureRoute;
use persisting_capture::subagent_link::{
    enrich_record, extract_spawn_hints_from_assistant, extract_subagent_ids_from_request,
    extract_subagent_ids_from_text, SubagentRegistry,
};
use serde_json::json;

use super::support::fixture_body;

#[test]
fn parses_agent_tool_blocks_into_spawn_hints() {
    let text = r#"Launching reviewers.

```tool:Agent
{
  "description": "Review capture command design",
  "subagent_type": "Explore",
  "prompt": "Review docs/src/design/cli_capture_command.zh.md"
}
```"#;
    let hints = extract_spawn_hints_from_assistant(text);
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].subagent_type, "Explore");
    assert_eq!(
        hints[0].doc_target.as_deref(),
        Some("docs/src/design/cli_capture_command.zh.md")
    );
}

#[test]
fn matches_spawn_hint_to_registered_subagent_by_doc_target() {
    let mut registry = SubagentRegistry::default();
    let run_key = super::support::RUN_ROOT.to_string();
    let route = CaptureRoute {
        root_session: Some(super::support::RUN_ROOT.into()),
        session_id: super::support::CLAUDE_SESSION.into(),
        storage_session_id: super::support::RUN_ROOT.into(),
        subagent_id: None,
    };
    let sub_route = CaptureRoute {
        root_session: Some(super::support::RUN_ROOT.into()),
        session_id: super::support::CLAUDE_SESSION.into(),
        storage_session_id: "agent-ad20bef53147678d4".into(),
        subagent_id: Some("ad20bef53147678d4".into()),
    };
    let body: serde_json::Value =
        serde_json::from_slice(&fixture_body("subagent_explore_flash.json")).unwrap();
    registry.register_route(&run_key, &sub_route, "call-sub-first", Some(&body));
    let assistant = r#"```tool:Agent
{"description":"Review capture command design","subagent_type":"Explore","prompt":"Review docs/src/design/cli_capture_command.zh.md"}
```"#;
    let mut rec = CaptureRecord {
        seq: 0,
        source: "persisting-proxy".into(),
        kind: "llm.response.stream".into(),
        timestamp: None,
        session_id: Some(super::support::CLAUDE_SESSION.into()),
        agent_id: Some(super::support::PROXY_AGENT.into()),
        parent_uuid: None,
        trace_id: Some("trace-main".into()),
        call_id: Some("call-main".into()),
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: json!({}),
    };
    let outcome = enrich_record(
        &mut rec,
        &route,
        &HeaderMap::new(),
        None,
        Some(assistant),
        &mut registry,
    );
    assert!(outcome.spawn_link_backfills.is_empty());
    let links = rec.payload["spawn_links"].as_array().expect("spawn_links");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["subagent_id"], "ad20bef53147678d4");
    assert_eq!(
        links[0]["subagent_trajectory"],
        "subagents/agent-ad20bef53147678d4"
    );
}

#[test]
fn tool_result_agent_id_line_populates_refs() {
    let body: serde_json::Value =
        serde_json::from_slice(&fixture_body("agent_launch_tool_result.json")).unwrap();
    assert_eq!(
        extract_subagent_ids_from_request(&body),
        vec!["a483eb5f3a45995b0".to_string()]
    );
}

#[test]
fn task_notification_task_id_populates_refs() {
    let text = "<task-notification>\n<task-id>a206e346f287da063</task-id>\n</task-notification>";
    assert_eq!(
        extract_subagent_ids_from_text(text),
        vec!["a206e346f287da063".to_string()]
    );
}

#[test]
fn ignores_documentation_cli_agent_id_flag() {
    let body = json!({
        "messages": [{
            "role": "user",
            "content": [{
                "type": "tool_result",
                "content": "--agent-id my-repo --session-id abc"
            }]
        }]
    });
    assert!(extract_subagent_ids_from_request(&body).is_empty());
}

#[test]
fn spawn_link_records_are_never_skipped_in_markdown() {
    let rec = CaptureRecord {
        seq: 7,
        source: "persisting-proxy".into(),
        kind: "llm.spawn_link".into(),
        timestamp: None,
        session_id: None,
        agent_id: None,
        parent_uuid: None,
        trace_id: None,
        call_id: Some("call-parent".into()),
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: Some("call-parent".into()),
        payload: json!({
            "spawn_links": [{"subagent_id": "abc1234", "subagent_trajectory": "subagents/agent-abc1234"}]
        }),
    };
    assert!(!skip_markdown_block(&rec));
}
