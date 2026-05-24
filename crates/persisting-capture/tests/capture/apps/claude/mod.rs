//! Claude Code–specific capture regressions (routing, spawn links, markdown filters).
//!
//! See `docs/src/design/llm_capture_proxy.zh.md` §4.1 for the contract these tests guard.

mod dialogue_extract;
mod markdown_filter;
mod routing;
mod spawn_link;
mod support;
