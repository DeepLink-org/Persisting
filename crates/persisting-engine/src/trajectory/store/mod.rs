//! Unified trajectory storage: Vortex canonical + TLV Markdown materialized view.

mod markdown;
pub(crate) mod rows;
pub(crate) mod vortex;

pub(crate) use vortex::overwrite_session_lines;

use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use persisting_capture::story_coords::{story_vortex_event_path, StoryCoords};
use persisting_proto::TrajectoryStorageFormat;

/// Story coordinates for Vortex/Markdown backends (capture [`StoryCoords`]).
pub type TrajectorySession = StoryCoords;

pub(crate) fn session_vortex_path(session: &StoryCoords) -> Result<PathBuf> {
    story_vortex_event_path(
        &session.storage,
        &session.agent_id,
        &session.session_id,
        session.root_session_id.as_deref(),
    )
}

/// Result of appending RON engine lines to one physical backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrajectoryAppendOutcome {
    /// Input lines accepted for this backend (same as caller batch size).
    pub accepted_lines: usize,
    /// Rows/blocks actually persisted (markdown may be lower when blocks are filtered).
    pub persisted_units: usize,
    pub note: String,
}

/// Replay rows as JSON strings (one per record).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrajectoryReplayOutcome {
    pub records: Vec<String>,
    pub note: String,
}

/// Backend-specific stats (`row_count` = Vortex rows or markdown blocks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrajectoryStatsOutcome {
    pub dataset: String,
    pub row_count: usize,
    pub manifest_version: Option<u64>,
    pub status: String,
    pub note: String,
}

/// Physical trajectory store: Vortex event log or session markdown file(s).
#[async_trait]
pub trait TrajectoryStore: Send + Sync {
    fn format(&self) -> TrajectoryStorageFormat;

    fn display_path(&self, session: &TrajectorySession) -> Result<String>;

    async fn exists(&self, session: &TrajectorySession) -> Result<bool>;

    async fn append(
        &self,
        session: &TrajectorySession,
        lines: &[String],
    ) -> Result<TrajectoryAppendOutcome>;

    async fn replay(
        &self,
        session: &TrajectorySession,
        offset: usize,
        limit: Option<usize>,
    ) -> Result<TrajectoryReplayOutcome>;

    async fn stats(&self, session: &TrajectorySession) -> Result<TrajectoryStatsOutcome>;
}

/// Vortex backend: raw event log at `{run}/events.vortex`.
#[derive(Debug, Clone, Copy, Default)]
pub struct VortexTrajectoryStore;

/// Markdown backend: `0001.md` (TLV blocks) under the session directory.
#[derive(Debug, Clone, Copy, Default)]
pub struct MarkdownTrajectoryStore;

#[async_trait]
impl TrajectoryStore for VortexTrajectoryStore {
    fn format(&self) -> TrajectoryStorageFormat {
        TrajectoryStorageFormat::Vortex
    }

    fn display_path(&self, session: &TrajectorySession) -> Result<String> {
        vortex::display_path(session)
    }

    async fn exists(&self, session: &TrajectorySession) -> Result<bool> {
        vortex::exists(session).await
    }

    async fn append(
        &self,
        session: &TrajectorySession,
        lines: &[String],
    ) -> Result<TrajectoryAppendOutcome> {
        vortex::append(session, lines).await
    }

    async fn replay(
        &self,
        session: &TrajectorySession,
        offset: usize,
        limit: Option<usize>,
    ) -> Result<TrajectoryReplayOutcome> {
        vortex::replay(session, offset, limit).await
    }

    async fn stats(&self, session: &TrajectorySession) -> Result<TrajectoryStatsOutcome> {
        vortex::stats(session).await
    }
}

#[async_trait]
impl TrajectoryStore for MarkdownTrajectoryStore {
    fn format(&self) -> TrajectoryStorageFormat {
        TrajectoryStorageFormat::Markdown
    }

    fn display_path(&self, session: &TrajectorySession) -> Result<String> {
        markdown::display_path(session)
    }

    async fn exists(&self, session: &TrajectorySession) -> Result<bool> {
        markdown::exists(session)
    }

    async fn append(
        &self,
        session: &TrajectorySession,
        lines: &[String],
    ) -> Result<TrajectoryAppendOutcome> {
        markdown::append(session, lines)
    }

    async fn replay(
        &self,
        session: &TrajectorySession,
        offset: usize,
        limit: Option<usize>,
    ) -> Result<TrajectoryReplayOutcome> {
        markdown::replay(session, offset, limit)
    }

    async fn stats(&self, session: &TrajectorySession) -> Result<TrajectoryStatsOutcome> {
        markdown::stats(session)
    }
}

/// Single backend for append after `resolve_for_append`.
pub fn store_for_append(format: TrajectoryStorageFormat) -> Box<dyn TrajectoryStore> {
    match format {
        TrajectoryStorageFormat::Markdown => Box::new(MarkdownTrajectoryStore),
        TrajectoryStorageFormat::Vortex | TrajectoryStorageFormat::Both => {
            Box::new(VortexTrajectoryStore)
        }
        TrajectoryStorageFormat::Auto => {
            panic!("resolve_for_append must resolve Auto before store_for_append")
        }
    }
}

/// Single backend for read/stats after `resolve_for_read_with_root`.
pub fn store_for_read(format: TrajectoryStorageFormat) -> Box<dyn TrajectoryStore> {
    match format {
        TrajectoryStorageFormat::Vortex
        | TrajectoryStorageFormat::Both
        | TrajectoryStorageFormat::Auto => Box::new(VortexTrajectoryStore),
        TrajectoryStorageFormat::Markdown => Box::new(MarkdownTrajectoryStore),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_store_covers_formats() {
        assert!(matches!(
            store_for_append(TrajectoryStorageFormat::Vortex).format(),
            TrajectoryStorageFormat::Vortex
        ));
        assert!(matches!(
            store_for_append(TrajectoryStorageFormat::Markdown).format(),
            TrajectoryStorageFormat::Markdown
        ));
        assert!(matches!(
            store_for_append(TrajectoryStorageFormat::Both).format(),
            TrajectoryStorageFormat::Vortex
        ));
    }

    #[test]
    fn read_store_prefers_vortex_for_both_and_auto() {
        assert!(matches!(
            store_for_read(TrajectoryStorageFormat::Both).format(),
            TrajectoryStorageFormat::Vortex
        ));
        assert!(matches!(
            store_for_read(TrajectoryStorageFormat::Auto).format(),
            TrajectoryStorageFormat::Vortex
        ));
    }
}
