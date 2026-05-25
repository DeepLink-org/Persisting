//! Session-local markdown I/O (used only inside session actors).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::storage::frontmatter::refresh_document_frontmatter;
use crate::dialogue::try_capture_record_to_block;
use crate::markdown_trajectory::{
    session_markdown_write_path_for_key, upsert_block_by_call_id, BlockHeader,
};
use crate::record::CaptureRecord;
use crate::session_storage::{trajectory_run_dir, CaptureRoute};

use super::super::wire::SessionScope;

/// Markdown path resolution from session route + proxy storage root.
#[derive(Debug, Clone)]
pub struct MarkdownScope {
    pub route: CaptureRoute,
    pub agent_id: String,
    pub storage: PathBuf,
}

impl MarkdownScope {
    pub fn from_parts(scope: &SessionScope, storage: &Path) -> Self {
        Self {
            route: scope.route.clone(),
            agent_id: scope.agent_id.clone(),
            storage: storage.to_path_buf(),
        }
    }

    pub fn path(&self) -> PathBuf {
        let run_dir = trajectory_run_dir(self.storage.as_path(), &self.agent_id, &self.route);
        session_markdown_write_path_for_key(&run_dir, &self.route.storage_session_id)
    }
}

pub fn upsert_markdown(
    scope: &MarkdownScope,
    stream_markdown: bool,
    call_id: &str,
    block: (BlockHeader, Vec<u8>),
) -> Result<()> {
    if !stream_markdown {
        return Ok(());
    }
    let path = scope.path();
    upsert_block_by_call_id(&path, call_id, block)
        .with_context(|| format!("markdown upsert {}", path.display()))?;
    Ok(())
}

pub fn sync_markdown_record(
    scope: &MarkdownScope,
    stream_markdown: bool,
    rec: &CaptureRecord,
    default_call_id: &str,
) -> Result<()> {
    if !stream_markdown {
        return Ok(());
    }
    let Some(block) = try_capture_record_to_block(rec)? else {
        return Ok(());
    };
    let call_id = rec.call_id.as_deref().unwrap_or(default_call_id);
    upsert_markdown(scope, stream_markdown, call_id, block)?;
    if should_refresh_frontmatter(rec) {
        let _ = refresh_document_frontmatter(
            scope.storage.as_path(),
            &scope.agent_id,
            &scope.route,
            &scope.path(),
        )
        .map_err(|e| tracing::debug!("frontmatter refresh: {e:#}"));
    }
    Ok(())
}

fn should_refresh_frontmatter(rec: &CaptureRecord) -> bool {
    matches!(
        rec.kind.as_str(),
        "llm.request" | "llm.response" | "llm.response.stream"
    )
}
