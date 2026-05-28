use axum::http::{HeaderMap, HeaderValue};
use serde_json::json;

use crate::record::CaptureRecord;
use crate::session_storage::CaptureRoute;

use super::*;

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    use axum::http::header::HeaderName;
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.insert(
            HeaderName::from_bytes(k.as_bytes()).unwrap(),
            HeaderValue::try_from(*v).unwrap(),
        );
    }
    h
}

#[test]
fn ignores_cli_agent_id_flag_in_document_text() {
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
fn extracts_agent_id_from_task_notification() {
    let text = "<task-notification>\n<task-id>aeb40a1230926b4a8</task-id>\n</task-notification>";
    assert_eq!(
        extract_subagent_ids_from_text(text),
        vec!["aeb40a1230926b4a8".to_string()]
    );
    let body = json!({
        "messages": [{
            "role": "user",
            "content": [{"type": "text", "text": text}]
        }]
    });
    assert_eq!(
        extract_subagent_ids_from_request(&body),
        vec!["aeb40a1230926b4a8".to_string()]
    );
}

#[test]
fn extracts_agent_id_from_tool_result_line() {
    let text = "Async agent launched.\nagentId: a483eb5f3a45995b0 done\n";
    assert_eq!(
        extract_subagent_ids_from_text(text),
        vec!["a483eb5f3a45995b0".to_string()]
    );
}

#[test]
fn extracts_agent_id_from_tool_result_json() {
    let body = json!({
        "messages": [{
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "toolu_1",
                "content": [{"type":"text","text":"{\"agentId\":\"a5aa661\",\"status\":\"complete\"}"}]
            }]
        }]
    });
    assert_eq!(
        extract_subagent_ids_from_request(&body),
        vec!["a5aa661".to_string()]
    );
}

#[test]
fn extracts_spawn_hints_from_agent_tool_block() {
    let text = r#"Delegated.

```tool:Agent
{
  "subagent_type": "Explore",
  "prompt": "scan src/"
}
```"#;
    let hints = extract_spawn_hints_from_assistant(text);
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].subagent_type, "Explore");
    assert_eq!(hints[0].prompt.as_deref(), Some("scan src/"));
}

#[test]
fn match_spawns_by_doc_target_when_subagents_registered_first() {
    let mut registry = SubagentRegistry::default();
    let run_key = "run-20260524-021032";
    let doc = "docs/src/design/cli_capture_command.zh.md";
    let sub_body = json!({
        "messages": [{
            "role": "user",
            "content": format!("Review the Chinese design document at /Users/reiase/workspace/Persisting/{doc}")
        }]
    });
    let sub_route = CaptureRoute {
        root_session: Some(run_key.into()),
        session_id: "sess".into(),
        storage_session_id: "agent-a583b6c00c3d06436".into(),
        subagent_id: Some("a583b6c00c3d06436".into()),
    };
    registry.register_route(run_key, &sub_route, "call-sub", Some(&sub_body));

    let assistant = format!(
        r#"Launching review.

```tool:Agent
{{
  "description": "Review cli_capture_command design doc",
  "prompt": "Review the Chinese design document at /Users/reiase/workspace/Persisting/{doc}",
  "subagent_type": "general-purpose"
}}
```"#
    );
    let hints = extract_spawn_hints_from_assistant(&assistant);
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].doc_target.as_deref(), Some(doc));

    let links = registry.match_spawns_to_agents(run_key, &hints);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].subagent_id, "a583b6c00c3d06436");
    assert_eq!(links[0].subagent_trajectory, "agent-a583b6c00c3d06436.md");

    let main_route = CaptureRoute {
        root_session: Some(run_key.into()),
        session_id: "sess".into(),
        storage_session_id: run_key.into(),
        subagent_id: None,
    };
    let mut rec = CaptureRecord {
        seq: 3,
        source: "persisting-proxy".into(),
        kind: "llm.response.stream".into(),
        timestamp: None,
        session_id: None,
        agent_id: None,
        parent_uuid: None,
        trace_id: None,
        call_id: Some("call-main".into()),
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: json!({}),
    };
    let outcome = enrich_record(
        &mut rec,
        &main_route,
        &HeaderMap::new(),
        None,
        Some(&assistant),
        &mut registry,
    );
    assert!(outcome.spawn_link_backfills.is_empty());
    assert_eq!(
        rec.payload["spawn_links"][0]["subagent_id"],
        json!("a583b6c00c3d06436")
    );
    assert_eq!(
        rec.payload["refs_subagent_ids"],
        json!(["a583b6c00c3d06436"])
    );
}

#[test]
fn backfill_spawn_link_when_subagent_registers_after_assistant() {
    let mut registry = SubagentRegistry::default();
    let run_key = "run-backfill";
    let doc = "docs/src/design/capture_design.zh.md";
    let assistant = format!(
        r#"Starting.

```tool:Agent
{{
  "description": "Review capture_design",
  "prompt": "Review {doc}",
  "subagent_type": "general-purpose"
}}
```"#
    );
    let main_route = CaptureRoute {
        root_session: Some(run_key.into()),
        session_id: "sess".into(),
        storage_session_id: run_key.into(),
        subagent_id: None,
    };
    let mut main_rec = CaptureRecord {
        seq: 2,
        source: "t".into(),
        kind: "llm.response.stream".into(),
        timestamp: None,
        session_id: None,
        agent_id: None,
        parent_uuid: None,
        trace_id: None,
        call_id: Some("call-main".into()),
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: json!({}),
    };
    enrich_record(
        &mut main_rec,
        &main_route,
        &HeaderMap::new(),
        None,
        Some(&assistant),
        &mut registry,
    );
    assert!(main_rec.payload.get("spawn_links").is_none());

    let sub_body = json!({"messages":[{"role":"user","content": format!("Review {doc}")}]});
    let sub_route = CaptureRoute {
        root_session: Some(run_key.into()),
        session_id: "sess".into(),
        storage_session_id: "agent-deadbeef".into(),
        subagent_id: Some("deadbeef".into()),
    };
    let mut sub_rec = CaptureRecord {
        seq: 0,
        source: "t".into(),
        kind: "llm.request".into(),
        timestamp: None,
        session_id: None,
        agent_id: None,
        parent_uuid: None,
        trace_id: None,
        call_id: Some("call-sub".into()),
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: json!({}),
    };
    let outcome = enrich_record(
        &mut sub_rec,
        &sub_route,
        &HeaderMap::new(),
        Some(&sub_body),
        None,
        &mut registry,
    );
    assert_eq!(outcome.spawn_link_backfills.len(), 1);
    assert_eq!(outcome.spawn_link_backfills[0].parent_call_id, "call-main");
    assert_eq!(
        outcome.spawn_link_backfills[0].links[0].subagent_id,
        "deadbeef"
    );
}

#[test]
fn enrich_links_main_request_to_subagent_trajectory() {
    let mut registry = SubagentRegistry::default();
    let run_key = "run-1";
    let sub_route = CaptureRoute {
        root_session: Some(run_key.into()),
        session_id: "sess".into(),
        storage_session_id: "agent-abc1234".into(),
        subagent_id: Some("abc1234".into()),
    };
    registry.register_route(run_key, &sub_route, "call-sub", None);

    let body = json!({
        "messages": [{
            "role": "user",
            "content": [{
                "type": "tool_result",
                "content": "agentId: abc1234 done"
            }]
        }]
    });
    let main_route = CaptureRoute {
        root_session: Some(run_key.into()),
        session_id: "sess".into(),
        storage_session_id: run_key.into(),
        subagent_id: None,
    };
    let mut rec = CaptureRecord {
        seq: 0,
        source: "t".into(),
        kind: "llm.request".into(),
        timestamp: None,
        session_id: None,
        agent_id: None,
        parent_uuid: None,
        trace_id: None,
        call_id: Some("call-main".into()),
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: json!({}),
    };
    enrich_record(
        &mut rec,
        &main_route,
        &HeaderMap::new(),
        Some(&body),
        None,
        &mut registry,
    );
    assert_eq!(rec.payload["refs_subagent_ids"], json!(["abc1234"]));
    assert_eq!(
        rec.payload["subagent_trajectories"],
        json!(["agent-abc1234.md"])
    );
}

#[test]
fn enrich_subagent_record_has_self_trajectory() {
    let mut registry = SubagentRegistry::default();
    let route = CaptureRoute {
        root_session: Some("run-1".into()),
        session_id: "sess".into(),
        storage_session_id: "agent-xyz".into(),
        subagent_id: Some("xyz".into()),
    };
    let mut rec = CaptureRecord {
        seq: 0,
        source: "t".into(),
        kind: "llm.request".into(),
        timestamp: None,
        session_id: None,
        agent_id: None,
        parent_uuid: None,
        trace_id: None,
        call_id: Some("call-1".into()),
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: json!({}),
    };
    enrich_record(
        &mut rec,
        &route,
        &headers(&[("x-claude-code-parent-agent-id", "main01")]),
        None,
        None,
        &mut registry,
    );
    assert_eq!(rec.subagent_id.as_deref(), Some("xyz"));
    assert_eq!(rec.parent_agent_id.as_deref(), Some("main01"));
    assert_eq!(rec.payload["subagent_trajectory"], json!("agent-xyz.md"));
}
