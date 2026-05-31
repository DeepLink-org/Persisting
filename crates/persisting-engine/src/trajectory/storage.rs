//! Resolve Vortex vs markdown storage for a session.

use persisting_proto::TrajectoryStorageFormat;

use super::convert::LayerStats;
use super::store::{
    session_vortex_path, MarkdownTrajectoryStore, TrajectoryStore, VortexTrajectoryStore,
};
use persisting_capture::story_coords::StoryCoords;

pub fn detect_story_primary_layer(
    layers: &LayerStats,
    session: &StoryCoords,
) -> TrajectoryStorageFormat {
    let has_vortex = layers.event_rows > 0;
    let has_md = layers.markdown_blocks > 0;
    match (has_vortex, has_md) {
        (false, true) => TrajectoryStorageFormat::Markdown,
        (true, false) => TrajectoryStorageFormat::Vortex,
        (false, false) => TrajectoryStorageFormat::Markdown,
        (true, true) => {
            // Capture run: live TLV markdown is the dialogue story; Vortex is the raw event log.
            if session.root_session_id.is_some() {
                return TrajectoryStorageFormat::Markdown;
            }
            // Flat layout: materialized / lossy markdown has fewer blocks than raw Vortex.
            if layers.markdown_blocks < layers.event_rows {
                return TrajectoryStorageFormat::Markdown;
            }
            TrajectoryStorageFormat::Vortex
        }
    }
}

pub fn story_stats_note(layers: &LayerStats, primary: TrajectoryStorageFormat) -> String {
    let primary_count = match primary {
        TrajectoryStorageFormat::Markdown => layers.markdown_blocks,
        _ => layers.event_rows,
    };
    match (layers.event_rows > 0, layers.markdown_blocks > 0) {
        (true, true) => format!(
            "Story stats via {via} ({primary_count}); Vortex {} raw event(s), Markdown {} dialogue block(s)",
            layers.event_rows, layers.markdown_blocks,
            via = format_label(primary)
        ),
        (true, false) => format!("Story stats via vortex ({primary_count} raw event(s))"),
        (false, true) => format!("Story stats via markdown ({primary_count} dialogue block(s))"),
        (false, false) => "Story stats: no trajectory data".to_string(),
    }
}

async fn resolve_auto_read(session: &StoryCoords) -> anyhow::Result<TrajectoryStorageFormat> {
    resolve_auto_read_with(
        session,
        TrajectoryStorageFormat::Vortex,
        TrajectoryStorageFormat::Vortex,
    )
    .await
}

async fn resolve_auto_read_with(
    session: &StoryCoords,
    when_empty: TrajectoryStorageFormat,
    when_both: TrajectoryStorageFormat,
) -> anyhow::Result<TrajectoryStorageFormat> {
    let vortex = VortexTrajectoryStore;
    let md = MarkdownTrajectoryStore;
    let has_vortex = vortex.exists(session).await?;
    let has_md = md.exists(session).await?;
    Ok(match (has_vortex, has_md) {
        (true, false) => TrajectoryStorageFormat::Vortex,
        (false, true) => TrajectoryStorageFormat::Markdown,
        (false, false) => when_empty,
        (true, true) => when_both,
    })
}

pub async fn resolve_for_read_with_root(
    storage: &str,
    agent_id: &str,
    session_id: &str,
    root_session_id: Option<&str>,
    requested: TrajectoryStorageFormat,
) -> anyhow::Result<TrajectoryStorageFormat> {
    let session = StoryCoords::new(
        storage,
        agent_id,
        session_id,
        root_session_id.map(str::to_string),
    );
    match requested {
        TrajectoryStorageFormat::Vortex
        | TrajectoryStorageFormat::Markdown
        | TrajectoryStorageFormat::Both => Ok(requested),
        TrajectoryStorageFormat::Auto => resolve_auto_read(&session).await,
    }
}

pub async fn resolve_for_append(
    storage: &str,
    agent_id: &str,
    session_id: &str,
    root_session_id: Option<&str>,
    requested: TrajectoryStorageFormat,
) -> anyhow::Result<TrajectoryStorageFormat> {
    let session = StoryCoords::new(
        storage,
        agent_id,
        session_id,
        root_session_id.map(str::to_string),
    );
    match requested {
        TrajectoryStorageFormat::Vortex => Ok(TrajectoryStorageFormat::Vortex),
        TrajectoryStorageFormat::Markdown => Ok(TrajectoryStorageFormat::Markdown),
        // Legacy alias: append targets Vortex only (same as Vortex).
        TrajectoryStorageFormat::Both => Ok(TrajectoryStorageFormat::Vortex),
        TrajectoryStorageFormat::Auto => {
            resolve_auto_read_with(
                &session,
                TrajectoryStorageFormat::Vortex,
                TrajectoryStorageFormat::Vortex,
            )
            .await
        }
    }
}

pub fn format_label(fmt: TrajectoryStorageFormat) -> &'static str {
    match fmt {
        TrajectoryStorageFormat::Auto => "auto",
        TrajectoryStorageFormat::Vortex => "vortex",
        TrajectoryStorageFormat::Markdown => "markdown",
        TrajectoryStorageFormat::Both => "both (legacy)",
    }
}

pub fn dataset_display(
    storage: &str,
    agent_id: &str,
    session_id: &str,
    root_session_id: Option<&str>,
    fmt: TrajectoryStorageFormat,
) -> anyhow::Result<String> {
    let session = StoryCoords::new(
        storage,
        agent_id,
        session_id,
        root_session_id.map(str::to_string),
    );
    let dir = session.run_dir()?;
    match fmt {
        TrajectoryStorageFormat::Markdown => {
            let path = persisting_capture::markdown_trajectory::locate_session_markdown_for_key(
                &dir, session_id,
            );
            Ok(path
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| dir.display().to_string()))
        }
        TrajectoryStorageFormat::Both => Ok(dir.display().to_string()),
        _ => session_vortex_path(&session).map(|p| p.display().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_proto::TrajectoryStorageFormat;

    #[test]
    fn detect_primary_capture_run_prefers_markdown_when_both() {
        let layers = LayerStats {
            event_rows: 30,
            markdown_blocks: 30,
            event_log_path: "store/a/run/events.vortex".into(),
            markdown_path: Some("store/a/run/0001.md".into()),
            note: String::new(),
        };
        let session = StoryCoords::new("store", "a", "run-1", Some("run-1".into()));
        assert_eq!(
            detect_story_primary_layer(&layers, &session),
            TrajectoryStorageFormat::Markdown
        );
    }

    #[test]
    fn detect_primary_flat_vortex_canonical_when_counts_match() {
        let layers = LayerStats {
            event_rows: 2,
            markdown_blocks: 2,
            event_log_path: "store/a/s/events.vortex".into(),
            markdown_path: Some("store/a/s/s.md".into()),
            note: String::new(),
        };
        let session = StoryCoords::new("store", "a", "s", None);
        assert_eq!(
            detect_story_primary_layer(&layers, &session),
            TrajectoryStorageFormat::Vortex
        );
    }

    #[test]
    fn detect_primary_flat_markdown_when_fewer_blocks() {
        let layers = LayerStats {
            event_rows: 10,
            markdown_blocks: 3,
            event_log_path: "store/a/s/events.vortex".into(),
            markdown_path: Some("store/a/s/s.md".into()),
            note: String::new(),
        };
        let session = StoryCoords::new("store", "a", "s", None);
        assert_eq!(
            detect_story_primary_layer(&layers, &session),
            TrajectoryStorageFormat::Markdown
        );
    }

    #[tokio::test]
    async fn resolve_append_auto_on_empty_session_defaults_vortex() {
        let storage = tempfile::tempdir().unwrap();
        let storage = storage.path().to_string_lossy().into_owned();
        let fmt = resolve_for_append(&storage, "a", "s", None, TrajectoryStorageFormat::Auto)
            .await
            .unwrap();
        assert_eq!(fmt, TrajectoryStorageFormat::Vortex);
    }

    #[tokio::test]
    async fn resolve_append_auto_on_markdown_only_session() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("a").join("s");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(run.join("0001.md"), "# x\n").unwrap();
        let storage = dir.path().to_string_lossy().into_owned();
        let fmt = resolve_for_append(&storage, "a", "s", None, TrajectoryStorageFormat::Auto)
            .await
            .unwrap();
        assert_eq!(fmt, TrajectoryStorageFormat::Markdown);
    }

    #[tokio::test]
    async fn resolve_append_explicit_formats() {
        let storage = tempfile::tempdir().unwrap();
        let storage = storage.path().to_string_lossy().into_owned();
        let md = resolve_for_append(&storage, "a", "s", None, TrajectoryStorageFormat::Markdown)
            .await
            .unwrap();
        assert_eq!(md, TrajectoryStorageFormat::Markdown);

        let both = resolve_for_append(&storage, "a", "s", None, TrajectoryStorageFormat::Both)
            .await
            .unwrap();
        assert_eq!(both, TrajectoryStorageFormat::Vortex);
    }

    #[tokio::test]
    async fn resolve_read_auto_on_markdown_only() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("a").join("s");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(run.join("0001.md"), "# x\n").unwrap();
        let storage = dir.path().to_string_lossy().into_owned();
        let fmt =
            resolve_for_read_with_root(&storage, "a", "s", None, TrajectoryStorageFormat::Auto)
                .await
                .unwrap();
        assert_eq!(fmt, TrajectoryStorageFormat::Markdown);
    }
}
