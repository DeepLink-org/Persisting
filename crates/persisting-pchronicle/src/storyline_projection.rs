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

pub async fn verify_storyline_projection(
    source_uri: impl AsRef<str>,
    output_uri: impl AsRef<str>,
) -> Result<StorylineProjectionVerification> {
    let source = RawEventDataSource::open_uri(source_uri.as_ref()).await?;
    let snapshot = source.fact_snapshot().clone();
    let projection = storyline_projection_status(output_uri).await?;
    let (fresh, reason) = verify_lineage(&snapshot, projection.lineage.as_ref());
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

fn verify_lineage(
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
        assert!(verify_lineage(&snapshot, Some(&lineage)).0);
        let compacted = EventFactSnapshot {
            layout_revision: 13,
            ..snapshot.clone()
        };
        assert!(verify_lineage(&compacted, Some(&lineage)).0);
        let appended = EventFactSnapshot {
            fact_version: 5,
            fact_rows: 10,
            ..compacted
        };
        assert!(!verify_lineage(&appended, Some(&lineage)).0);
    }
}
