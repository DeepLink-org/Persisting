//! Claude Code HTTP routing: main run dir + `{session_id}.md` siblings.

use super::support::{
    body_bytes, claude_run_storage, fixture_body, headers, main_trajectory_dir,
    resolve_claude_route, subagent_markdown_path, CLAUDE_SESSION, PROXY_AGENT, RUN_ROOT,
};
use persisting_capture::session_storage::resolve_capture_route;

#[test]
fn main_agent_pro_turn_writes_header_session_md() {
    let dir = claude_run_storage();
    let body = fixture_body("main_user_pro.json");
    let route = resolve_claude_route(
        dir.path(),
        &[("x-claude-code-session-id", CLAUDE_SESSION)],
        &body,
    );
    assert_eq!(route.root_session.as_deref(), Some(RUN_ROOT));
    assert_eq!(route.storage_session_id, CLAUDE_SESSION);
    assert_eq!(route.session_id, CLAUDE_SESSION);
    assert!(route.subagent_id.is_none());
    assert_eq!(
        main_trajectory_dir(dir.path(), &route),
        dir.path().join(format!("{PROXY_AGENT}/{RUN_ROOT}"))
    );
    assert!(subagent_markdown_path(dir.path(), &route).ends_with(format!("{CLAUDE_SESSION}.md")));
}

#[test]
fn flash_session_probe_without_agent_id_writes_header_session_md() {
    let dir = claude_run_storage();
    let body = fixture_body("flash_session_probe.json");
    let header_session = "884e189a-9433-4996-a809-3a8232a8967d";
    let route = resolve_claude_route(
        dir.path(),
        &[("x-claude-code-session-id", header_session)],
        &body,
    );
    assert_eq!(route.storage_session_id, header_session);
    assert!(route.subagent_id.is_none());
    assert_eq!(route.session_id, header_session);
    assert!(subagent_markdown_path(dir.path(), &route).ends_with(format!("{header_session}.md")));
}

#[test]
fn explore_subagent_with_agent_id_writes_sibling_md() {
    let dir = claude_run_storage();
    let body = fixture_body("subagent_explore_flash.json");
    let agent = "ad20bef53147678d4";
    let route = resolve_claude_route(
        dir.path(),
        &[
            ("x-claude-code-session-id", CLAUDE_SESSION),
            ("x-claude-code-agent-id", agent),
        ],
        &body,
    );
    assert_eq!(route.subagent_id.as_deref(), Some(agent));
    assert_eq!(route.storage_session_id, format!("agent-{agent}"));
    assert_eq!(
        main_trajectory_dir(dir.path(), &route),
        dir.path().join(format!("{PROXY_AGENT}/{RUN_ROOT}"))
    );
    assert_eq!(
        subagent_markdown_path(dir.path(), &route),
        dir.path()
            .join(format!("{PROXY_AGENT}/{RUN_ROOT}/agent-{agent}.md"))
    );
}

#[test]
fn parallel_subagents_get_isolated_md_files() {
    let dir = claude_run_storage();
    let body = fixture_body("subagent_explore_flash.json");
    for agent in [
        "a1111111111111111",
        "b2222222222222222",
        "c3333333333333333",
    ] {
        let route = resolve_claude_route(
            dir.path(),
            &[
                ("x-claude-code-session-id", CLAUDE_SESSION),
                ("x-claude-code-agent-id", agent),
            ],
            &body,
        );
        assert_eq!(route.storage_session_id, format!("agent-{agent}"));
        assert_eq!(
            subagent_markdown_path(dir.path(), &route),
            dir.path()
                .join(format!("{PROXY_AGENT}/{RUN_ROOT}/agent-{agent}.md"))
        );
    }
}

#[test]
fn task_notification_with_agent_id_header_routes_to_main_session_md() {
    let dir = claude_run_storage();
    let body = fixture_body("task_notification.json");
    let agent = "aeb40a1230926b4a8";
    let route = resolve_claude_route(
        dir.path(),
        &[
            ("x-claude-code-session-id", CLAUDE_SESSION),
            ("x-claude-code-agent-id", agent),
        ],
        &body,
    );
    assert_eq!(route.storage_session_id, CLAUDE_SESSION);
    assert!(route.subagent_id.is_none());
    assert!(subagent_markdown_path(dir.path(), &route).ends_with(format!("{CLAUDE_SESSION}.md")));
}

#[test]
fn capture_serve_flat_layout_without_run_root() {
    let body = body_bytes(r#"{"model":"deepseek-v4-pro","messages":[]}"#);
    let route = resolve_capture_route(
        &headers(&[("x-persisting-session-id", "sess-flat")]),
        &body,
        "x-persisting-session-id",
        std::path::Path::new("/tmp/unused"),
    );
    assert!(route.root_session.is_none());
    assert_eq!(route.storage_session_id, "sess-flat");
}
