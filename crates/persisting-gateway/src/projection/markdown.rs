//! Session AgenticMD writes through the authoritative Storyline model.

use std::path::Path;

use anyhow::{Context, Result};
use persisting_pchronicle::document::upsert_agenticmd_turn;
use persisting_pchronicle::model::{StorylineDocument, StorylineTurn};
use serde_json::{Map, Value};

use crate::session::client::resolve_client_meta_for_run_dir;

/// Insert or replace one Storyline turn in the session's readable AgenticMD view.
pub fn upsert_storyline_turn(
    path: &Path,
    document: &StorylineDocument,
    edit_key: &str,
    turn: &StorylineTurn,
) -> Result<bool> {
    let document = if path.exists() {
        document.clone()
    } else {
        document_with_client_metadata(path, document)?
    };
    upsert_agenticmd_turn(path, &document, turn, edit_key)
        .with_context(|| format!("upsert {}", path.display()))
}

fn document_with_client_metadata(
    path: &Path,
    document: &StorylineDocument,
) -> Result<StorylineDocument> {
    let mut document = document.clone();
    let client = path.parent().and_then(|run_dir| {
        run_dir
            .parent()
            .and_then(|agent_dir| agent_dir.parent())
            .and_then(|storage| resolve_client_meta_for_run_dir(storage, run_dir))
    });
    if let Some(client) = client {
        let extra = document
            .agent
            .extra
            .get_or_insert_with(|| Value::Object(Map::new()));
        let object = extra
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("Storyline agent.extra must be an object"))?;
        object.insert(
            "client".into(),
            serde_json::to_value(client).context("serialize session client metadata")?,
        );
    }
    Ok(document)
}

/// Human-readable duration for session summaries (`42s`, `3m12s`, `1h5m`).
pub(crate) fn format_duration_human(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3600 {
        return format!("{}m{}s", secs / 60, secs % 60);
    }
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    if mins == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h{mins}m")
    }
}
