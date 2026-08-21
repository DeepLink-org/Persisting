//! Rebuildable canonical-events → Storyline projection lifecycle.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::convert::{event_storyline_key, project_event_records};
use crate::formats::{EventRecord, StorylineDocument};
use crate::store::{
    EventFactSnapshot, ProjectionSourceSnapshot, RawEventDataSource, StorylineLanceStore,
    StorylineProjectionLineage, StorylineProjectionPublicationOutcome,
};

pub const STORYLINE_PROJECTOR_NAME: &str = "canonical-events-to-storyline";
pub const STORYLINE_PROJECTION_COMPLETENESS: &str = "full";
const STORYLINE_PROJECTION_RECIPE: &str =
    "group=storyline_identity;order=manifest_append;projection=events-storyline";

#[cfg(test)]
#[derive(Clone)]
struct BuildPublicationBarrierHook {
    output_uri: String,
    barrier: std::sync::Arc<tokio::sync::Barrier>,
}

#[cfg(test)]
static BUILD_BEFORE_PUBLICATION_BARRIER: std::sync::Mutex<Option<BuildPublicationBarrierHook>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
static PROJECT_SOURCE_READ_FAILURE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[cfg(test)]
async fn wait_before_build_publication(output_uri: &str) {
    let barrier = BUILD_BEFORE_PUBLICATION_BARRIER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .filter(|hook| hook.output_uri == output_uri)
        .map(|hook| hook.barrier.clone());
    if let Some(barrier) = barrier {
        barrier.wait().await;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorylineProjectionBuildReport {
    pub source_uri: String,
    pub output_uri: String,
    pub source_id: String,
    pub generation: String,
    pub fact_version: u64,
    pub fact_rows: u64,
    pub storylines: usize,
    pub steps: usize,
    pub tool_calls: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorylineProjectionBuildOutcome {
    Built(StorylineProjectionBuildReport),
    OutputNotEmpty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorylineProjectionStatus {
    pub output_uri: String,
    pub generation: Option<String>,
    pub lineage: Option<StorylineProjectionLineage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorylineProjectionVerification {
    pub fresh: bool,
    pub reason: String,
    pub source: EventFactSnapshot,
    pub projection: StorylineProjectionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorylineProjectionSyncMode {
    Noop,
    Incremental,
    Rebuild,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorylineProjectionSyncReport {
    pub mode: StorylineProjectionSyncMode,
    pub source_uri: String,
    pub output_uri: String,
    pub generation: String,
    pub previous_fact_version: Option<u64>,
    pub fact_version: u64,
    pub previous_fact_rows: Option<u64>,
    pub fact_rows: u64,
    pub affected_storylines: usize,
    /// Newly appended canonical rows read to discover affected Storylines.
    pub suffix_rows_scanned: u64,
    /// Canonical history rows read for the affected Storylines.
    pub history_rows_scanned: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionRebuildReason {
    MissingLineage,
    IncompatibleLineage,
    NonMonotonicWatermark,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorylineProjectionSyncOutcome {
    Synced(StorylineProjectionSyncReport),
    MissingProjection,
    RequiresRebuild(ProjectionRebuildReason),
}

/// Build a complete, pinned Storyline projection into an empty output store.
pub async fn build_storyline_projection(
    source_uri: impl AsRef<str>,
    output_uri: impl AsRef<str>,
    source_file: impl Into<String>,
) -> Result<StorylineProjectionBuildOutcome> {
    let source = RawEventDataSource::open_uri(source_uri.as_ref())
        .await
        .with_context(|| format!("open canonical event source {}", source_uri.as_ref()))?;
    let output = StorylineLanceStore::open_uri(output_uri.as_ref())
        .await
        .with_context(|| format!("open Storyline projection output {}", output_uri.as_ref()))?;
    // Fast-path known conflicts before reading the full source. Publication
    // still rechecks under the store write guard and object-store CURRENT CAS.
    if output.current_table_paths().await?.is_some() {
        return Ok(StorylineProjectionBuildOutcome::OutputNotEmpty);
    }
    let snapshot = source.fact_snapshot().clone();
    let lineage = canonical_projection_lineage(&snapshot, source_file.into());
    let stories = project_canonical_event_source(&source).await?;
    #[cfg(test)]
    wait_before_build_publication(output.root_uri()).await;
    let report = match output
        .create_projected_storyline_stream(
            stories.into_iter().map(Ok::<_, anyhow::Error>),
            lineage.clone(),
        )
        .await
        .context("publish Storyline projection")?
    {
        StorylineProjectionPublicationOutcome::Published(report) => report,
        StorylineProjectionPublicationOutcome::OutputNotEmpty => {
            return Ok(StorylineProjectionBuildOutcome::OutputNotEmpty);
        }
    };
    Ok(StorylineProjectionBuildOutcome::Built(
        StorylineProjectionBuildReport {
            source_uri: snapshot.source_uri,
            output_uri: output.root_uri().to_string(),
            source_id: lineage.source_id,
            generation: report.generation,
            fact_version: snapshot.fact_version,
            fact_rows: snapshot.fact_rows,
            storylines: report.storylines,
            steps: report.steps,
            tool_calls: report.tool_calls,
        },
    ))
}

pub async fn storyline_projection_status(
    output_uri: impl AsRef<str>,
) -> Result<StorylineProjectionStatus> {
    let output = StorylineLanceStore::open_uri(output_uri.as_ref()).await?;
    let paths = output.current_table_paths().await?;
    Ok(StorylineProjectionStatus {
        output_uri: output.root_uri().to_string(),
        generation: paths.as_ref().map(|paths| paths.generation.clone()),
        lineage: paths.and_then(|paths| paths.projection),
    })
}

/// Replace every projection table from one pinned canonical fact snapshot.
pub async fn rebuild_storyline_projection(
    source_uri: impl AsRef<str>,
    output_uri: impl AsRef<str>,
    source_file: impl Into<String>,
) -> Result<StorylineProjectionSyncReport> {
    let source = RawEventDataSource::open_uri(source_uri.as_ref()).await?;
    let output = StorylineLanceStore::open_uri(output_uri.as_ref()).await?;
    let snapshot = source.fact_snapshot().clone();
    let previous = output
        .current_table_paths()
        .await?
        .and_then(|paths| paths.projection);
    let (previous_fact_version, previous_fact_rows) = lineage_fact_watermark(previous.as_ref());
    let lineage = canonical_projection_lineage(&snapshot, source_file.into());
    let stories = project_canonical_event_source(&source).await?;
    let report = output
        .rebuild_projected_storyline_stream(
            stories.into_iter().map(Ok::<_, anyhow::Error>),
            lineage,
        )
        .await?;
    Ok(StorylineProjectionSyncReport {
        mode: StorylineProjectionSyncMode::Rebuild,
        source_uri: snapshot.source_uri,
        output_uri: output.root_uri().to_string(),
        generation: report.generation,
        previous_fact_version,
        fact_version: snapshot.fact_version,
        previous_fact_rows,
        fact_rows: snapshot.fact_rows,
        affected_storylines: report.storylines,
        suffix_rows_scanned: 0,
        history_rows_scanned: snapshot.fact_rows,
    })
}

/// Advance a complete projection by replacing only Storylines touched by the
/// canonical append suffix. Incompatible or non-monotonic lineage requires an
/// explicit rebuild instead of guessing.
pub async fn sync_storyline_projection(
    source_uri: impl AsRef<str>,
    output_uri: impl AsRef<str>,
) -> Result<StorylineProjectionSyncOutcome> {
    let source = RawEventDataSource::open_uri(source_uri.as_ref()).await?;
    let output = StorylineLanceStore::open_uri(output_uri.as_ref()).await?;
    let snapshot = source.fact_snapshot().clone();
    let Some(paths) = output.current_table_paths().await? else {
        return Ok(StorylineProjectionSyncOutcome::MissingProjection);
    };
    let Some(previous) = paths.projection.as_ref() else {
        return Ok(StorylineProjectionSyncOutcome::RequiresRebuild(
            ProjectionRebuildReason::MissingLineage,
        ));
    };
    if !incremental_lineage_is_compatible(&snapshot, previous) {
        return Ok(StorylineProjectionSyncOutcome::RequiresRebuild(
            ProjectionRebuildReason::IncompatibleLineage,
        ));
    }
    let ProjectionSourceSnapshot::CanonicalEvents {
        fact_version: previous_fact_version,
        fact_rows: previous_fact_rows,
        ..
    } = &previous.source
    else {
        anyhow::bail!(
            "projection source is not canonical events; projection requires a complete rebuild"
        )
    };

    if projection_lineage_is_fresh(&snapshot, previous) {
        return Ok(StorylineProjectionSyncOutcome::Synced(
            StorylineProjectionSyncReport {
                mode: StorylineProjectionSyncMode::Noop,
                source_uri: snapshot.source_uri,
                output_uri: output.root_uri().to_string(),
                generation: paths.generation,
                previous_fact_version: Some(*previous_fact_version),
                fact_version: snapshot.fact_version,
                previous_fact_rows: Some(*previous_fact_rows),
                fact_rows: snapshot.fact_rows,
                affected_storylines: 0,
                suffix_rows_scanned: 0,
                history_rows_scanned: 0,
            },
        ));
    }

    if *previous_fact_rows > snapshot.fact_rows || *previous_fact_version > snapshot.fact_version {
        return Ok(StorylineProjectionSyncOutcome::RequiresRebuild(
            ProjectionRebuildReason::NonMonotonicWatermark,
        ));
    }
    let suffix = source
        .read_records_range_in_append_order(*previous_fact_rows, snapshot.fact_rows)
        .await?;
    let suffix_rows_scanned = u64::try_from(suffix.len())?;
    let affected = suffix
        .iter()
        .map(|record| {
            event_storyline_key(record)
                .context("new canonical event has no Storyline identity")
                .map(str::to_string)
        })
        .collect::<Result<std::collections::BTreeSet<_>>>()?;
    anyhow::ensure!(
        !affected.is_empty(),
        "canonical fact watermark advanced without visible appended events"
    );
    let affected_count = affected.len();
    let selected = source.read_records_for_storylines(&affected).await?;
    let history_rows_scanned = u64::try_from(selected.len())?;
    let stories = project_canonical_event_records(selected)?;
    anyhow::ensure!(
        stories.len() == affected_count,
        "incremental projection did not cover every affected Storyline"
    );
    let lineage = canonical_projection_lineage(&snapshot, previous.source_file.clone());
    output
        .replace_projected_storylines(&stories, lineage)
        .await?;
    let generation = output
        .current_table_paths()
        .await?
        .context("incremental projection committed no CURRENT generation")?
        .generation;
    Ok(StorylineProjectionSyncOutcome::Synced(
        StorylineProjectionSyncReport {
            mode: StorylineProjectionSyncMode::Incremental,
            source_uri: snapshot.source_uri,
            output_uri: output.root_uri().to_string(),
            generation,
            previous_fact_version: Some(*previous_fact_version),
            fact_version: snapshot.fact_version,
            previous_fact_rows: Some(*previous_fact_rows),
            fact_rows: snapshot.fact_rows,
            affected_storylines: affected_count,
            suffix_rows_scanned,
            history_rows_scanned,
        },
    ))
}

pub async fn verify_storyline_projection(
    source_uri: impl AsRef<str>,
    output_uri: impl AsRef<str>,
) -> Result<StorylineProjectionVerification> {
    let source = RawEventDataSource::open_uri(source_uri.as_ref()).await?;
    let snapshot = source.fact_snapshot().clone();
    let projection = storyline_projection_status(output_uri).await?;
    let (fresh, reason) = projection_lineage_freshness(&snapshot, projection.lineage.as_ref());
    Ok(StorylineProjectionVerification {
        fresh,
        reason,
        source: snapshot,
        projection,
    })
}

pub fn canonical_projection_lineage(
    snapshot: &EventFactSnapshot,
    source_file: impl Into<String>,
) -> StorylineProjectionLineage {
    let source_id = format!(
        "events:{}",
        blake3::hash(snapshot.source_uri.as_bytes()).to_hex()
    );
    StorylineProjectionLineage {
        source_id,
        source_file: source_file.into(),
        source: ProjectionSourceSnapshot::CanonicalEvents {
            source_uri: snapshot.source_uri.clone(),
            fact_version: snapshot.fact_version,
            fact_rows: snapshot.fact_rows,
            layout_revision: snapshot.layout_revision,
        },
        projector_name: STORYLINE_PROJECTOR_NAME.into(),
        recipe_hash: blake3::hash(STORYLINE_PROJECTION_RECIPE.as_bytes())
            .to_hex()
            .to_string(),
        completeness: STORYLINE_PROJECTION_COMPLETENESS.into(),
    }
}

pub fn projection_lineage_is_fresh(
    snapshot: &EventFactSnapshot,
    lineage: &StorylineProjectionLineage,
) -> bool {
    projection_lineage_freshness(snapshot, Some(lineage)).0
}

fn incremental_lineage_is_compatible(
    snapshot: &EventFactSnapshot,
    lineage: &StorylineProjectionLineage,
) -> bool {
    let expected = canonical_projection_lineage(snapshot, lineage.source_file.clone());
    let ProjectionSourceSnapshot::CanonicalEvents { source_uri, .. } = &lineage.source else {
        return false;
    };
    source_uri == &snapshot.source_uri
        && lineage.source_id == expected.source_id
        && lineage.projector_name == expected.projector_name
        && lineage.recipe_hash == expected.recipe_hash
        && lineage.completeness == STORYLINE_PROJECTION_COMPLETENESS
}

fn lineage_fact_watermark(
    lineage: Option<&StorylineProjectionLineage>,
) -> (Option<u64>, Option<u64>) {
    match lineage.map(|lineage| &lineage.source) {
        Some(ProjectionSourceSnapshot::CanonicalEvents {
            fact_version,
            fact_rows,
            ..
        }) => (Some(*fact_version), Some(*fact_rows)),
        _ => (None, None),
    }
}

fn projection_lineage_freshness(
    snapshot: &EventFactSnapshot,
    lineage: Option<&StorylineProjectionLineage>,
) -> (bool, String) {
    let Some(lineage) = lineage else {
        return (false, "projection has no canonical source lineage".into());
    };
    let ProjectionSourceSnapshot::CanonicalEvents {
        source_uri,
        fact_version,
        fact_rows,
        ..
    } = &lineage.source
    else {
        return (
            false,
            "projection was not derived from canonical events".into(),
        );
    };
    if source_uri != &snapshot.source_uri
        || fact_version != &snapshot.fact_version
        || fact_rows != &snapshot.fact_rows
    {
        return (
            false,
            "projection fact watermark does not match canonical events".into(),
        );
    }
    let mut expected = canonical_projection_lineage(snapshot, lineage.source_file.clone());
    // Layout-only compaction is not a fact change and must not stale a projection.
    expected.source = lineage.source.clone();
    if lineage == &expected {
        (
            true,
            "projection matches the pinned canonical fact snapshot".into(),
        )
    } else {
        (
            false,
            "projection lineage does not match the pinned canonical fact snapshot".into(),
        )
    }
}

async fn project_canonical_event_source(
    source: &RawEventDataSource,
) -> Result<Vec<StorylineDocument>> {
    #[cfg(test)]
    if PROJECT_SOURCE_READ_FAILURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_deref()
        == Some(source.fact_snapshot().source_uri.as_str())
    {
        anyhow::bail!("injected canonical source read failure");
    }
    let records = source.read_records_in_append_order().await?;
    project_canonical_event_records(records)
}

fn project_canonical_event_records(records: Vec<EventRecord>) -> Result<Vec<StorylineDocument>> {
    let mut groups = BTreeMap::<String, Vec<EventRecord>>::new();
    for record in records {
        let key = event_storyline_key(&record)
            .context("canonical event requires session_id, storyline_id, or run_id")?
            .to_string();
        groups.entry(key).or_default().push(record);
    }
    anyhow::ensure!(!groups.is_empty(), "canonical event source is empty");
    groups
        .into_values()
        .map(|records| project_event_records(&records))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        layout::StoryCoords,
        store::{raw_event_lance_path, RawEventLanceStore},
        EventIdentity,
    };
    use serde_json::json;

    struct BuildPublicationBarrier;

    struct ProjectSourceReadFailure;

    impl ProjectSourceReadFailure {
        fn install(source_uri: impl Into<String>) -> Self {
            *PROJECT_SOURCE_READ_FAILURE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(source_uri.into());
            Self
        }
    }

    impl Drop for ProjectSourceReadFailure {
        fn drop(&mut self) {
            *PROJECT_SOURCE_READ_FAILURE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
    }

    impl BuildPublicationBarrier {
        fn install(output_uri: &str, parties: usize) -> Self {
            *BUILD_BEFORE_PUBLICATION_BARRIER
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(BuildPublicationBarrierHook {
                    output_uri: output_uri.to_string(),
                    barrier: std::sync::Arc::new(tokio::sync::Barrier::new(parties)),
                });
            Self
        }
    }

    impl Drop for BuildPublicationBarrier {
        fn drop(&mut self) {
            *BUILD_BEFORE_PUBLICATION_BARRIER
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
    }

    fn event(session_id: &str, seq: u64) -> EventRecord {
        EventRecord {
            identity: EventIdentity {
                event_id: Some(format!("{session_id}-{seq}")),
                ..Default::default()
            },
            seq,
            source: "test".into(),
            kind: "note".into(),
            timestamp: None,
            session_id: Some(session_id.into()),
            agent_id: Some("agent".into()),
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: json!({"content": format!("{session_id}-{seq}")}),
        }
    }

    async fn canonical_source(temp: &tempfile::TempDir) -> (String, String) {
        let storage = temp.path().join("capture");
        let coords = StoryCoords::new(
            storage.to_string_lossy(),
            "agent",
            "session",
            Some("run".into()),
        );
        RawEventLanceStore
            .append_events(&coords, &[event("session", 1)])
            .await
            .unwrap();
        let source = raw_event_lance_path(&coords)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let output = temp.path().join("storyline").to_string_lossy().into_owned();
        (source, output)
    }

    #[tokio::test]
    async fn building_into_nonempty_output_is_an_explicit_conflict() {
        let temp = tempfile::tempdir().unwrap();
        let (source, output) = canonical_source(&temp).await;
        let StorylineProjectionBuildOutcome::Built(report) =
            build_storyline_projection(&source, &output, "events.lance")
                .await
                .unwrap()
        else {
            panic!("initial build unexpectedly reported nonempty output")
        };
        assert_eq!(
            std::fs::canonicalize(&report.source_uri).unwrap(),
            std::fs::canonicalize(&source).unwrap()
        );
        assert_eq!(
            std::fs::canonicalize(&report.output_uri).unwrap(),
            std::fs::canonicalize(&output).unwrap()
        );
        assert_eq!(report.fact_rows, 1);
        assert_eq!(report.storylines, 1);

        let outcome = build_storyline_projection(&source, &output, "events.lance")
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            StorylineProjectionBuildOutcome::OutputNotEmpty
        ));
    }

    #[tokio::test]
    async fn known_nonempty_output_skips_source_projection() {
        let temp = tempfile::tempdir().unwrap();
        let (source, output) = canonical_source(&temp).await;
        let initial = build_storyline_projection(&source, &output, "events.lance")
            .await
            .unwrap();
        assert!(matches!(initial, StorylineProjectionBuildOutcome::Built(_)));
        let canonical_source = std::fs::canonicalize(&source)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let _failure = ProjectSourceReadFailure::install(canonical_source);

        let outcome = build_storyline_projection(&source, &output, "events.lance")
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            StorylineProjectionBuildOutcome::OutputNotEmpty
        ));
    }

    #[tokio::test]
    async fn concurrent_builds_publish_exactly_one_projection() {
        let temp = tempfile::tempdir().unwrap();
        let (source, output) = canonical_source(&temp).await;
        let _barrier = BuildPublicationBarrier::install(&output, 2);

        let (left, right) = tokio::join!(
            build_storyline_projection(&source, &output, "left-events.lance"),
            build_storyline_projection(&source, &output, "right-events.lance")
        );
        let left = left.unwrap();
        let right = right.unwrap();

        let built = usize::from(matches!(left, StorylineProjectionBuildOutcome::Built(_)))
            + usize::from(matches!(right, StorylineProjectionBuildOutcome::Built(_)));
        let conflicts = usize::from(matches!(
            left,
            StorylineProjectionBuildOutcome::OutputNotEmpty
        )) + usize::from(matches!(
            right,
            StorylineProjectionBuildOutcome::OutputNotEmpty
        ));
        assert_eq!(built, 1);
        assert_eq!(conflicts, 1);

        let (report, source_file) = match (&left, &right) {
            (StorylineProjectionBuildOutcome::Built(report), _) => (report, "left-events.lance"),
            (_, StorylineProjectionBuildOutcome::Built(report)) => (report, "right-events.lance"),
            _ => unreachable!("exactly one build succeeded"),
        };
        let store = StorylineLanceStore::open_uri(&output).await.unwrap();
        let current = store.current_table_paths().await.unwrap().unwrap();
        assert_eq!(current.generation, report.generation);
        assert_eq!(current.projection.unwrap().source_file, source_file);
        let story = store.get_storyline_full("session").await.unwrap().unwrap();
        assert_eq!(story.session_id, "session");
        assert_eq!(story.turns.len(), 1);
    }

    #[tokio::test]
    async fn empty_projection_is_an_explicit_missing_result() {
        let temp = tempfile::tempdir().unwrap();
        let (source, output) = canonical_source(&temp).await;

        let outcome = sync_storyline_projection(&source, &output).await.unwrap();

        assert!(matches!(
            outcome,
            StorylineProjectionSyncOutcome::MissingProjection
        ));
    }

    #[tokio::test]
    async fn projection_without_lineage_requires_rebuild() {
        let temp = tempfile::tempdir().unwrap();
        let (source, output) = canonical_source(&temp).await;
        let output_store = StorylineLanceStore::open_uri(&output).await.unwrap();
        output_store
            .replace_storyline_stream(
                project_canonical_event_records(vec![event("session", 1)])
                    .unwrap()
                    .into_iter()
                    .map(Ok),
            )
            .await
            .unwrap();

        let outcome = sync_storyline_projection(&source, &output).await.unwrap();

        assert!(matches!(
            outcome,
            StorylineProjectionSyncOutcome::RequiresRebuild(
                ProjectionRebuildReason::MissingLineage
            )
        ));
    }

    #[tokio::test]
    async fn incompatible_projection_lineage_requires_rebuild() {
        let temp = tempfile::tempdir().unwrap();
        let (source, output) = canonical_source(&temp).await;
        let source_store = RawEventDataSource::open_uri(&source).await.unwrap();
        let mut lineage =
            canonical_projection_lineage(source_store.fact_snapshot(), "events.lance");
        lineage.recipe_hash = "different-recipe".into();
        let output_store = StorylineLanceStore::open_uri(&output).await.unwrap();
        output_store
            .replace_projected_storyline_stream(
                project_canonical_event_records(vec![event("session", 1)])
                    .unwrap()
                    .into_iter()
                    .map(Ok),
                lineage,
            )
            .await
            .unwrap();

        let outcome = sync_storyline_projection(&source, &output).await.unwrap();

        assert!(matches!(
            outcome,
            StorylineProjectionSyncOutcome::RequiresRebuild(
                ProjectionRebuildReason::IncompatibleLineage
            )
        ));
    }

    #[tokio::test]
    async fn non_monotonic_projection_watermark_requires_rebuild() {
        let temp = tempfile::tempdir().unwrap();
        let (source, output) = canonical_source(&temp).await;
        let source_store = RawEventDataSource::open_uri(&source).await.unwrap();
        let mut lineage =
            canonical_projection_lineage(source_store.fact_snapshot(), "events.lance");
        let ProjectionSourceSnapshot::CanonicalEvents {
            fact_version,
            fact_rows,
            ..
        } = &mut lineage.source
        else {
            unreachable!()
        };
        *fact_version += 1;
        *fact_rows += 1;
        let output_store = StorylineLanceStore::open_uri(&output).await.unwrap();
        output_store
            .replace_projected_storyline_stream(
                project_canonical_event_records(vec![event("session", 1)])
                    .unwrap()
                    .into_iter()
                    .map(Ok),
                lineage,
            )
            .await
            .unwrap();

        let outcome = sync_storyline_projection(&source, &output).await.unwrap();

        assert!(matches!(
            outcome,
            StorylineProjectionSyncOutcome::RequiresRebuild(
                ProjectionRebuildReason::NonMonotonicWatermark
            )
        ));
    }

    #[test]
    fn full_projection_groups_by_identity_without_reordering_each_group() {
        let stories =
            project_canonical_event_records(vec![event("b", 2), event("a", 7), event("b", 1)])
                .unwrap();
        assert_eq!(
            stories
                .iter()
                .map(|story| story.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(stories[1].turns[0].extra.as_ref().unwrap()["seq"], 2);
        assert_eq!(stories[1].turns[1].extra.as_ref().unwrap()["seq"], 1);
    }

    #[test]
    fn verification_uses_fact_identity_not_layout_revision_alone() {
        let snapshot = EventFactSnapshot {
            source_uri: "events.lance".into(),
            fact_version: 4,
            fact_rows: 9,
            layout_revision: 12,
        };
        let lineage = canonical_projection_lineage(&snapshot, "events.lance");
        assert!(projection_lineage_freshness(&snapshot, Some(&lineage)).0);
        let compacted = EventFactSnapshot {
            layout_revision: 13,
            ..snapshot.clone()
        };
        assert!(projection_lineage_freshness(&compacted, Some(&lineage)).0);
        let appended = EventFactSnapshot {
            fact_version: 5,
            fact_rows: 10,
            ..compacted
        };
        assert!(!projection_lineage_freshness(&appended, Some(&lineage)).0);
    }
}
