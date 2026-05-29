//! Resolve Lance vs markdown storage for a session.

use persisting_capture::markdown_trajectory::session_markdown_filename;
use persisting_proto::TrajectoryStorageFormat;

use super::store::{session_lance_dir, LanceTrajectoryStore, MarkdownTrajectoryStore};
use super::trajectory_dataset_dir;
use super::TrajectoryStore;
use persisting_capture::StoryCoords;

async fn resolve_auto(
    session: &StoryCoords,
    when_empty: TrajectoryStorageFormat,
    when_both: TrajectoryStorageFormat,
) -> anyhow::Result<TrajectoryStorageFormat> {
    let lance = LanceTrajectoryStore;
    let markdown = MarkdownTrajectoryStore;
    let has_lance = lance.exists(session).await?;
    let has_md = markdown.exists(session).await?;
    Ok(match (has_lance, has_md) {
        (true, false) => TrajectoryStorageFormat::Lance,
        (false, true) => TrajectoryStorageFormat::Markdown,
        (true, true) => when_both,
        (false, false) => when_empty,
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
        TrajectoryStorageFormat::Lance
        | TrajectoryStorageFormat::Markdown
        | TrajectoryStorageFormat::Both => Ok(requested),
        TrajectoryStorageFormat::Auto => {
            resolve_auto(
                &session,
                TrajectoryStorageFormat::Markdown,
                TrajectoryStorageFormat::Lance,
            )
            .await
        }
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
        TrajectoryStorageFormat::Lance => Ok(TrajectoryStorageFormat::Lance),
        TrajectoryStorageFormat::Markdown => Ok(TrajectoryStorageFormat::Markdown),
        // Legacy alias: append targets Lance only (same as Lance).
        TrajectoryStorageFormat::Both => Ok(TrajectoryStorageFormat::Lance),
        TrajectoryStorageFormat::Auto => {
            resolve_auto(
                &session,
                TrajectoryStorageFormat::Lance,
                TrajectoryStorageFormat::Lance,
            )
            .await
        }
    }
}

pub fn format_label(fmt: TrajectoryStorageFormat) -> &'static str {
    match fmt {
        TrajectoryStorageFormat::Auto => "auto",
        TrajectoryStorageFormat::Lance => "lance",
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
    let dir = trajectory_dataset_dir(storage, agent_id, session_id, root_session_id)?;
    match fmt {
        TrajectoryStorageFormat::Markdown => {
            let run = super::trajectory_run_dir(storage, agent_id, session_id, root_session_id)?;
            Ok(format!(
                "{}/{}",
                run.display(),
                session_markdown_filename(session_id)
            ))
        }
        TrajectoryStorageFormat::Both => Ok(dir.display().to_string()),
        _ => session_lance_dir(&session).map(|p| p.display().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_proto::TrajectoryStorageFormat;

    #[tokio::test]
    async fn resolve_append_auto_on_empty_session_defaults_lance() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().to_string_lossy().to_string();
        let fmt = resolve_for_append(&storage, "a", "s", None, TrajectoryStorageFormat::Auto)
            .await
            .unwrap();
        assert_eq!(fmt, TrajectoryStorageFormat::Lance);
    }

    #[tokio::test]
    async fn resolve_append_auto_when_only_markdown_exists() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().to_string_lossy().to_string();
        let run = super::super::trajectory_run_dir(&storage, "a", "s", None).unwrap();
        std::fs::create_dir_all(&run).unwrap();
        let md = run.join("s.md");
        std::fs::write(&md, "# seed\n").unwrap();

        let fmt = resolve_for_append(&storage, "a", "s", None, TrajectoryStorageFormat::Auto)
            .await
            .unwrap();
        assert_eq!(fmt, TrajectoryStorageFormat::Markdown);
    }

    #[tokio::test]
    async fn resolve_append_explicit_markdown_and_both() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().to_string_lossy().to_string();

        let md = resolve_for_append(&storage, "a", "s", None, TrajectoryStorageFormat::Markdown)
            .await
            .unwrap();
        assert_eq!(md, TrajectoryStorageFormat::Markdown);

        let both = resolve_for_append(&storage, "a", "s", None, TrajectoryStorageFormat::Both)
            .await
            .unwrap();
        assert_eq!(both, TrajectoryStorageFormat::Lance);
    }

    #[tokio::test]
    async fn resolve_read_auto_when_only_markdown_exists() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().to_string_lossy().to_string();
        let run = super::super::trajectory_run_dir(&storage, "a", "s", None).unwrap();
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(run.join("s.md"), "# md\n").unwrap();

        let fmt =
            resolve_for_read_with_root(&storage, "a", "s", None, TrajectoryStorageFormat::Auto)
                .await
                .unwrap();
        assert_eq!(fmt, TrajectoryStorageFormat::Markdown);
    }
}
