//! Sidecar Vortex layers under `{run}/layers/` (manifest + joinable files).

mod judge_sidecar;
mod manifest;

pub use judge_sidecar::{
    has_judgment, merge_judge_rows, read_judge_rows, write_judge_rows, JudgeRow,
};
pub const STORY_CALL_ID: &str = "__story__";
pub const MANUAL_RATIONALE_PREFIX: &str = "[manual] ";
pub use manifest::{load_manifest, register_layer, save_manifest};

use std::path::PathBuf;

use anyhow::{Context, Result};
use persisting_capture::story_coords::story_run_dir;

use super::TrajectorySession;

pub fn layers_dir(session: &TrajectorySession) -> Result<PathBuf> {
    Ok(story_run_dir(
        &session.storage,
        &session.agent_id,
        &session.session_id,
        session.root_session_id.as_deref(),
    )?
    .join("layers"))
}

pub fn manifest_path(session: &TrajectorySession) -> Result<PathBuf> {
    Ok(layers_dir(session)?.join("manifest.json"))
}

pub fn sidecar_path(session: &TrajectorySession, layer_file: &str) -> Result<PathBuf> {
    Ok(layers_dir(session)?.join(layer_file))
}

pub async fn ensure_layers_dir(session: &TrajectorySession) -> Result<PathBuf> {
    let dir = layers_dir(session)?;
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("create layers dir {}", dir.display()))?;
    Ok(dir)
}

pub fn layer_field_name(rubric_id: &str) -> String {
    format!("judge_{}", sanitize_layer_token(rubric_id))
}

pub fn layer_file_name(rubric_id: &str) -> String {
    format!("{}.vortex", layer_field_name(rubric_id))
}

fn sanitize_layer_token(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "default".to_string()
    } else {
        out
    }
}

pub fn join_on_session_call_id() -> Vec<String> {
    vec![
        crate::trajectory::TRAJECTORY_SESSION_ID_COL.to_string(),
        crate::trajectory::TRAJECTORY_CALL_ID_COL.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rubric_for_layer_name() {
        assert_eq!(layer_field_name("default"), "judge_default");
        assert_eq!(layer_field_name("task/success"), "judge_task_success");
    }
}
