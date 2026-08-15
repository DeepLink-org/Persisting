//! Rebuildable canonical-events → Storyline projection lifecycle.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::convert::event_storyline_key;
use crate::{
    project_event_records, EventFactSnapshot, EventRecord, ProjectionSourceSnapshot,
    RawEventDataSource, StorylineDocument, StorylineLanceStore, StorylineProjectionLineage,
    EVENTS_TO_STORYLINE_PROJECTOR_VERSION, STORYLINE_SCHEMA_VERSION,
};

pub const STORYLINE_PROJECTOR_NAME: &str = "canonical-events-to-storyline";
pub const STORYLINE_PROJECTION_COMPLETENESS: &str = "full";
const STORYLINE_PROJECTION_RECIPE: &str =
    "group=storyline_identity;order=manifest_append;projection=events-storyline/v1";

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
}

/// Build a complete, pinned Storyline projection into an empty output store.
pub async fn build_storyline_projection(
    source_uri: impl AsRef<str>,
    output_uri: impl AsRef<str>,
    source_file: impl Into<String>,
) -> Result<StorylineProjectionBuildReport> {
    let source = RawEventDataSource::open_uri(source_uri.as_ref())
        .await
        .with_context(|| format!("open canonical event source {}", source_uri.as_ref()))?;
    let output = StorylineLanceStore::open_uri(output_uri.as_ref())
        .await
        .with_context(|| format!("open Storyline projection output {}", output_uri.as_ref()))?;
    anyhow::ensure!(
        output.current_table_paths().await?.is_none(),
        "Storyline projection output is not empty; use `project sync` or `project rebuild`"
    );

    let snapshot = source.fact_snapshot().clone();
    let lineage = canonical_projection_lineage(&snapshot, source_file.into());
    let stories = project_canonical_event_source(&source).await?;
    let report = output
        .replace_projected_storyline_stream(
            stories.into_iter().map(Ok::<_, anyhow::Error>),
            lineage.clone(),
        )
        .await
        .context("publish Storyline projection")?;
    Ok(StorylineProjectionBuildReport {
        source_uri: snapshot.source_uri,
        output_uri: output.root_uri().to_string(),
        source_id: lineage.source_id,
        generation: report.generation,
        fact_version: snapshot.fact_version,
        fact_rows: snapshot.fact_rows,
        storylines: report.storylines,
        steps: report.steps,
        tool_calls: report.tool_calls,
    })
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
    })
}

/// Advance a complete projection by replacing only Storylines touched by the
/// canonical append suffix. Incompatible or non-monotonic lineage requires an
/// explicit rebuild instead of guessing.
pub async fn sync_storyline_projection(
    source_uri: impl AsRef<str>,
    output_uri: impl AsRef<str>,
) -> Result<StorylineProjectionSyncReport> {
    let source = RawEventDataSource::open_uri(source_uri.as_ref()).await?;
    let output = StorylineLanceStore::open_uri(output_uri.as_ref()).await?;
    let snapshot = source.fact_snapshot().clone();
    let paths = output
        .current_table_paths()
        .await?
        .context("Storyline projection is empty; use `project build`")?;
    let previous = paths
        .projection
        .as_ref()
        .context("Storyline store has no canonical lineage; use `project rebuild`")?;
    ensure_incremental_compatibility(&snapshot, previous)?;
    let ProjectionSourceSnapshot::CanonicalEvents {
        fact_version: previous_fact_version,
        fact_rows: previous_fact_rows,
        ..
    } = &previous.source
    else {
        unreachable!()
    };

    if projection_lineage_is_fresh(&snapshot, previous) {
        return Ok(StorylineProjectionSyncReport {
            mode: StorylineProjectionSyncMode::Noop,
            source_uri: snapshot.source_uri,
            output_uri: output.root_uri().to_string(),
            generation: paths.generation,
            previous_fact_version: Some(*previous_fact_version),
            fact_version: snapshot.fact_version,
            previous_fact_rows: Some(*previous_fact_rows),
            fact_rows: snapshot.fact_rows,
            affected_storylines: 0,
        });
    }

    anyhow::ensure!(
        *previous_fact_rows <= snapshot.fact_rows
            && *previous_fact_version <= snapshot.fact_version,
        "canonical fact watermark is non-monotonic; use `project rebuild`"
    );
    let records = source.read_records_in_append_order().await?;
    let previous_rows = usize::try_from(*previous_fact_rows)
        .context("previous fact row watermark does not fit this platform")?;
    anyhow::ensure!(
        records.len() == usize::try_from(snapshot.fact_rows)?,
        "canonical fact row watermark does not match the pinned event scan"
    );
    let affected = records[previous_rows..]
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
    let selected = records
        .into_iter()
        .filter(|record| event_storyline_key(record).is_some_and(|key| affected.contains(key)))
        .collect();
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
    Ok(StorylineProjectionSyncReport {
        mode: StorylineProjectionSyncMode::Incremental,
        source_uri: snapshot.source_uri,
        output_uri: output.root_uri().to_string(),
        generation,
        previous_fact_version: Some(*previous_fact_version),
        fact_version: snapshot.fact_version,
        previous_fact_rows: Some(*previous_fact_rows),
        fact_rows: snapshot.fact_rows,
        affected_storylines: affected_count,
    })
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
        projector_version: EVENTS_TO_STORYLINE_PROJECTOR_VERSION.into(),
        storyline_schema_version: STORYLINE_SCHEMA_VERSION.into(),
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

fn ensure_incremental_compatibility(
    snapshot: &EventFactSnapshot,
    lineage: &StorylineProjectionLineage,
) -> Result<()> {
    let expected = canonical_projection_lineage(snapshot, lineage.source_file.clone());
    let ProjectionSourceSnapshot::CanonicalEvents { source_uri, .. } = &lineage.source else {
        anyhow::bail!("projection source is not canonical events; use `project rebuild`");
    };
    anyhow::ensure!(
        source_uri == &snapshot.source_uri
            && lineage.source_id == expected.source_id
            && lineage.projector_name == expected.projector_name
            && lineage.projector_version == expected.projector_version
            && lineage.storyline_schema_version == expected.storyline_schema_version
            && lineage.recipe_hash == expected.recipe_hash
            && lineage.completeness == STORYLINE_PROJECTION_COMPLETENESS,
        "projection lineage is incompatible with incremental sync; use `project rebuild`"
    );
    Ok(())
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
        .map(|records| project_event_records(&records).map_err(anyhow::Error::from))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EventIdentity;
    use serde_json::json;

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
