//! Resolve Lance vs markdown storage for a session.

use persisting_capture::markdown_trajectory::SESSION_MARKDOWN_FILENAME;
use persisting_proto::TrajectoryStorageFormat;

use super::store::{LanceTrajectoryStore, MarkdownTrajectoryStore, TrajectorySession};
use super::trajectory_dataset_dir;
use super::TrajectoryStore;

async fn resolve_auto(
    session: &TrajectorySession,
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
    let session = TrajectorySession::new(
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
    let session = TrajectorySession::new(
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
                TrajectoryStorageFormat::Both,
                TrajectoryStorageFormat::Both,
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
        TrajectoryStorageFormat::Both => "both",
    }
}

pub fn dataset_display(
    storage: &str,
    agent_id: &str,
    session_id: &str,
    root_session_id: Option<&str>,
    fmt: TrajectoryStorageFormat,
) -> anyhow::Result<String> {
    let session = TrajectorySession::new(
        storage,
        agent_id,
        session_id,
        root_session_id.map(str::to_string),
    );
    let dir = trajectory_dataset_dir(storage, agent_id, session_id, root_session_id)?;
    match fmt {
        TrajectoryStorageFormat::Markdown => {
            Ok(format!("{}/{}", dir.display(), SESSION_MARKDOWN_FILENAME))
        }
        TrajectoryStorageFormat::Both => Ok(format!(
            "{}/[lance + {}]",
            dir.display(),
            SESSION_MARKDOWN_FILENAME
        )),
        _ => session.session_dir().map(|p| p.display().to_string()),
    }
}
