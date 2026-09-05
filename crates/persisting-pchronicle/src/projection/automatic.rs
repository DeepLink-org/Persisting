//! Automatic Storyline projections paired with canonical event stores.

use std::path::Path;

use anyhow::{Context, Result};

use super::storyline::{
    StorylineProjectionBuildOutcome, StorylineProjectionSyncMode, StorylineProjectionSyncOutcome,
    build_storyline_projection, canonical_projection_lineage, projection_lineage_is_fresh,
    rebuild_storyline_projection, storyline_projection_status, sync_storyline_projection,
};
use crate::store::{
    CatalogSourceStatus, DatasetCatalogSnapshot, EventFactSnapshot, ProjectionSourceSnapshot,
    RawEventDataSource, StorylineLanceStore,
};

const CANONICAL_EVENT_STORE_LEAF: &str = "events.lance";
const AUTOMATIC_STORYLINE_PROJECTION_LEAF: &str = "storyline";

pub(crate) fn automatic_storyline_projection_uri(source_uri: &str) -> Result<String> {
    if source_uri.contains("://") {
        let source_uri = source_uri.trim_end_matches('/');
        let (parent, leaf) = source_uri
            .rsplit_once('/')
            .with_context(|| format!("canonical event URI has no parent: {source_uri}"))?;
        anyhow::ensure!(
            leaf == CANONICAL_EVENT_STORE_LEAF,
            "canonical event URI must end with /{CANONICAL_EVENT_STORE_LEAF}: {source_uri}"
        );
        return Ok(format!("{parent}/{AUTOMATIC_STORYLINE_PROJECTION_LEAF}"));
    }

    let source = Path::new(source_uri);
    anyhow::ensure!(
        source.file_name().and_then(|name| name.to_str()) == Some(CANONICAL_EVENT_STORE_LEAF),
        "canonical event path must end with {CANONICAL_EVENT_STORE_LEAF}: {source_uri}"
    );
    let parent = source
        .parent()
        .with_context(|| format!("canonical event path has no parent: {source_uri}"))?;
    Ok(parent
        .join(AUTOMATIC_STORYLINE_PROJECTION_LEAF)
        .to_string_lossy()
        .into_owned())
}

pub async fn probe_canonical_event_store(
    uri: impl AsRef<str>,
) -> Result<Option<EventFactSnapshot>> {
    RawEventDataSource::probe_uri(uri).await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomaticProjectionTarget {
    pub dataset: String,
    pub source_path: String,
    pub source_uri: String,
    pub projection_path: String,
    pub projection_uri: String,
    pub source_snapshot: EventFactSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomaticProjectionInventoryError {
    pub dataset: String,
    pub source_path: String,
    pub projection_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomaticProjectionInventory {
    pub snapshot_id: String,
    pub targets: Vec<AutomaticProjectionTarget>,
    pub errors: Vec<AutomaticProjectionInventoryError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomaticProjectionState {
    Fresh,
    Stale,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomaticProjectionInspection {
    pub state: AutomaticProjectionState,
    pub generation: Option<String>,
    pub fact_version: u64,
    pub fact_rows: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomaticProjectionMaintenanceMode {
    Unchanged,
    Built,
    Incremental,
    Rebuilt,
    ConcurrentWinner,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomaticProjectionMaintenanceReport {
    pub mode: AutomaticProjectionMaintenanceMode,
    pub generation: String,
    pub fact_version: u64,
    pub fact_rows: u64,
    pub trajectories: Option<usize>,
}

impl AutomaticProjectionMaintenanceReport {
    pub fn published(&self) -> bool {
        matches!(
            self.mode,
            AutomaticProjectionMaintenanceMode::Built
                | AutomaticProjectionMaintenanceMode::Incremental
                | AutomaticProjectionMaintenanceMode::Rebuilt
        )
    }
}

pub fn automatic_projection_inventory(
    snapshot: &DatasetCatalogSnapshot,
) -> Result<AutomaticProjectionInventory> {
    let mut targets = snapshot
        .canonical_event_sources()
        .into_iter()
        .map(|source| {
            let source_path = display_source_path(&source.source_path);
            Ok(AutomaticProjectionTarget {
                dataset: source.dataset,
                projection_path: display_projection_path(&source_path)?,
                projection_uri: automatic_storyline_projection_uri(&source.source_uri)?,
                source_path,
                source_uri: source.source_uri,
                source_snapshot: source.snapshot,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    targets.sort_by(|left, right| {
        (&left.dataset, &left.source_path).cmp(&(&right.dataset, &right.source_path))
    });

    let mut errors = snapshot
        .datasets()
        .iter()
        .flat_map(|dataset| {
            dataset
                .sources
                .iter()
                .filter(|source| {
                    source.status == CatalogSourceStatus::Error
                        && source.format.as_deref()
                            == Some(crate::DocumentFormat::CanonicalEvent.as_str())
                })
                .map(|source| {
                    let source_path = display_source_path(&source.file);
                    display_projection_path(&source_path).map(|projection_path| {
                        AutomaticProjectionInventoryError {
                            dataset: dataset.mount.name.clone(),
                            source_path,
                            projection_path,
                        }
                    })
                })
        })
        .collect::<Result<Vec<_>>>()?;
    errors.sort_by(|left, right| {
        (&left.dataset, &left.source_path).cmp(&(&right.dataset, &right.source_path))
    });

    Ok(AutomaticProjectionInventory {
        snapshot_id: snapshot.snapshot_id().to_string(),
        targets,
        errors,
    })
}

pub async fn inspect_automatic_storyline_projection(
    target: &AutomaticProjectionTarget,
) -> Result<AutomaticProjectionInspection> {
    let status = storyline_projection_status(&target.projection_uri).await?;
    let Some(generation) = status.generation else {
        return Ok(inspection(target, AutomaticProjectionState::Missing, None));
    };
    let lineage = status
        .lineage
        .as_ref()
        .context("automatic Storyline destination has no canonical lineage")?;
    let ProjectionSourceSnapshot::CanonicalEvents { source_uri, .. } = &lineage.source else {
        anyhow::bail!("automatic Storyline destination was not derived from canonical events");
    };
    anyhow::ensure!(
        source_uri == &target.source_snapshot.source_uri,
        "automatic Storyline destination does not have matching canonical source ownership"
    );
    let state = if projection_lineage_is_fresh(&target.source_snapshot, lineage) {
        AutomaticProjectionState::Fresh
    } else {
        AutomaticProjectionState::Stale
    };
    Ok(inspection(target, state, Some(generation)))
}

pub async fn maintain_automatic_storyline_projection(
    target: &AutomaticProjectionTarget,
) -> Result<AutomaticProjectionMaintenanceReport> {
    let inspection = inspect_automatic_storyline_projection(target).await?;
    match inspection.state {
        AutomaticProjectionState::Missing => build_or_accept_concurrent_winner(target).await,
        AutomaticProjectionState::Fresh => {
            report_from_inspection(AutomaticProjectionMaintenanceMode::Unchanged, inspection)
        }
        AutomaticProjectionState::Stale => {
            let outcome =
                match sync_storyline_projection(&target.source_uri, &target.projection_uri).await {
                    Ok(outcome) => outcome,
                    Err(error) => return accept_fresh_concurrent_winner(target, error).await,
                };
            match outcome {
                StorylineProjectionSyncOutcome::Synced(report) => {
                    let mode = match report.mode {
                        StorylineProjectionSyncMode::Noop => {
                            AutomaticProjectionMaintenanceMode::ConcurrentWinner
                        }
                        StorylineProjectionSyncMode::Incremental => {
                            AutomaticProjectionMaintenanceMode::Incremental
                        }
                        StorylineProjectionSyncMode::Rebuild => {
                            AutomaticProjectionMaintenanceMode::Rebuilt
                        }
                    };
                    Ok(AutomaticProjectionMaintenanceReport {
                        mode,
                        generation: report.generation,
                        fact_version: report.fact_version,
                        fact_rows: report.fact_rows,
                        trajectories: (mode
                            != AutomaticProjectionMaintenanceMode::ConcurrentWinner)
                            .then_some(report.affected_storylines),
                    })
                }
                StorylineProjectionSyncOutcome::MissingProjection => {
                    build_or_accept_concurrent_winner(target).await
                }
                StorylineProjectionSyncOutcome::RequiresRebuild(_) => {
                    ensure_current_lineage_owns_target(target).await?;
                    match rebuild_storyline_projection(
                        &target.source_uri,
                        &target.projection_uri,
                        &target.source_path,
                    )
                    .await
                    {
                        Ok(report) => Ok(AutomaticProjectionMaintenanceReport {
                            mode: AutomaticProjectionMaintenanceMode::Rebuilt,
                            generation: report.generation,
                            fact_version: report.fact_version,
                            fact_rows: report.fact_rows,
                            trajectories: Some(report.affected_storylines),
                        }),
                        Err(error) => accept_fresh_concurrent_winner(target, error).await,
                    }
                }
            }
        }
    }
}

pub async fn storyline_projection_destination_exists(uri: impl AsRef<str>) -> Result<bool> {
    StorylineLanceStore::destination_exists(uri).await
}

fn inspection(
    target: &AutomaticProjectionTarget,
    state: AutomaticProjectionState,
    generation: Option<String>,
) -> AutomaticProjectionInspection {
    AutomaticProjectionInspection {
        state,
        generation,
        fact_version: target.source_snapshot.fact_version,
        fact_rows: target.source_snapshot.fact_rows,
    }
}

async fn build_or_accept_concurrent_winner(
    target: &AutomaticProjectionTarget,
) -> Result<AutomaticProjectionMaintenanceReport> {
    match build_storyline_projection(
        &target.source_uri,
        &target.projection_uri,
        &target.source_path,
    )
    .await?
    {
        StorylineProjectionBuildOutcome::Built(report) => {
            Ok(AutomaticProjectionMaintenanceReport {
                mode: AutomaticProjectionMaintenanceMode::Built,
                generation: report.generation,
                fact_version: report.fact_version,
                fact_rows: report.fact_rows,
                trajectories: Some(report.storylines),
            })
        }
        StorylineProjectionBuildOutcome::OutputNotEmpty => {
            let inspection = inspect_automatic_storyline_projection(target).await?;
            anyhow::ensure!(
                inspection.state == AutomaticProjectionState::Fresh,
                "automatic Storyline destination became nonempty without a fresh matching projection"
            );
            report_from_inspection(
                AutomaticProjectionMaintenanceMode::ConcurrentWinner,
                inspection,
            )
        }
    }
}

async fn ensure_current_lineage_owns_target(target: &AutomaticProjectionTarget) -> Result<()> {
    let status = storyline_projection_status(&target.projection_uri).await?;
    let lineage = status
        .lineage
        .as_ref()
        .context("automatic Storyline destination has no canonical lineage")?;
    let ProjectionSourceSnapshot::CanonicalEvents { source_uri, .. } = &lineage.source else {
        anyhow::bail!("automatic Storyline destination was not derived from canonical events");
    };
    let expected = canonical_projection_lineage(&target.source_snapshot, &target.source_path);
    anyhow::ensure!(
        source_uri == &target.source_snapshot.source_uri && lineage.source_id == expected.source_id,
        "automatic Storyline destination does not have matching canonical source ownership"
    );
    Ok(())
}

async fn accept_fresh_concurrent_winner(
    target: &AutomaticProjectionTarget,
    original: anyhow::Error,
) -> Result<AutomaticProjectionMaintenanceReport> {
    match inspect_automatic_storyline_projection(target).await {
        Ok(inspection) if inspection.state == AutomaticProjectionState::Fresh => {
            report_from_inspection(
                AutomaticProjectionMaintenanceMode::ConcurrentWinner,
                inspection,
            )
        }
        _ => Err(original),
    }
}

fn report_from_inspection(
    mode: AutomaticProjectionMaintenanceMode,
    inspection: AutomaticProjectionInspection,
) -> Result<AutomaticProjectionMaintenanceReport> {
    let generation = inspection
        .generation
        .context("fresh automatic Storyline projection has no generation")?;
    Ok(AutomaticProjectionMaintenanceReport {
        mode,
        generation,
        fact_version: inspection.fact_version,
        fact_rows: inspection.fact_rows,
        trajectories: None,
    })
}

fn display_source_path(source_path: &str) -> String {
    if source_path == "." {
        CANONICAL_EVENT_STORE_LEAF.into()
    } else {
        source_path.into()
    }
}

fn display_projection_path(source_path: &str) -> Result<String> {
    let parent = source_path
        .strip_suffix(CANONICAL_EVENT_STORE_LEAF)
        .with_context(|| {
            format!("canonical source path must end with {CANONICAL_EVENT_STORE_LEAF}")
        })?
        .trim_end_matches('/');
    Ok(if parent.is_empty() {
        AUTOMATIC_STORYLINE_PROJECTION_LEAF.into()
    } else {
        format!("{parent}/{AUTOMATIC_STORYLINE_PROJECTION_LEAF}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::StoryCoords;
    use crate::projection::{StorylineProjectionBuildOutcome, build_storyline_projection};
    use crate::store::opendal_store::Store as OpendalStore;
    use crate::store::{
        CatalogSnapshotOptions, DatasetCatalogSnapshot, DatasetMount, RawEventLanceStore,
        raw_event_lance_path,
    };
    use crate::{EventIdentity, EventRecord};

    fn note(session_id: &str, seq: u64) -> EventRecord {
        EventRecord {
            identity: EventIdentity::default(),
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
            payload: serde_json::json!({"content": session_id}),
        }
    }

    async fn append_note(storage: &Path, run_id: &str) -> Result<String> {
        append_note_at(storage, run_id, 0).await
    }

    async fn append_note_at(storage: &Path, run_id: &str, seq: u64) -> Result<String> {
        let coords = StoryCoords::new(
            storage.to_string_lossy(),
            "agent",
            run_id,
            Some(run_id.into()),
        );
        RawEventLanceStore
            .append_events(&coords, &[note(run_id, seq)])
            .await?;
        Ok(raw_event_lance_path(&coords)?
            .to_string_lossy()
            .into_owned())
    }

    async fn target_for(source_uri: &str, projection: &Path) -> Result<AutomaticProjectionTarget> {
        let source_snapshot = probe_canonical_event_store(source_uri)
            .await?
            .expect("test source is canonical");
        Ok(AutomaticProjectionTarget {
            dataset: "dataset".into(),
            source_path: "events.lance".into(),
            source_uri: source_snapshot.source_uri.clone(),
            projection_path: "storyline".into(),
            projection_uri: projection.to_string_lossy().into_owned(),
            source_snapshot,
        })
    }

    #[test]
    fn automatic_projection_uri_requires_the_exact_canonical_leaf() {
        assert_eq!(
            automatic_storyline_projection_uri("/datasets/run/events.lance").unwrap(),
            "/datasets/run/storyline"
        );
        assert_eq!(
            automatic_storyline_projection_uri("s3://bucket/runs/one/events.lance").unwrap(),
            "s3://bucket/runs/one/storyline"
        );
        assert_eq!(
            automatic_storyline_projection_uri("memory://events.lance").unwrap(),
            "memory://storyline"
        );

        for invalid in [
            "/datasets/run/not-events.lance",
            "/datasets/run/events.lance/segment",
            "s3://bucket/runs/events.lance-copy",
        ] {
            assert!(
                automatic_storyline_projection_uri(invalid).is_err(),
                "{invalid}"
            );
        }
    }

    #[cfg(feature = "proptest")]
    mod proptests {
        use proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn local_projection_uris_replace_only_the_canonical_leaf(
                parent in proptest::string::string_regex("[A-Za-z0-9_-]{1,24}").unwrap(),
                nested in proptest::string::string_regex("[A-Za-z0-9_-]{1,24}").unwrap(),
            ) {
                let source = format!("/datasets/{parent}/{nested}/events.lance");
                let projection = automatic_storyline_projection_uri(&source).unwrap();
                prop_assert_eq!(projection, format!("/datasets/{parent}/{nested}/storyline"));
            }

            #[test]
            fn remote_projection_uris_preserve_scheme_and_bucket(
                bucket in proptest::string::string_regex("[a-z0-9-]{1,16}").unwrap(),
                prefix in proptest::string::string_regex("[a-z0-9/_-]{1,32}").unwrap(),
            ) {
                let source = format!("s3://{bucket}/{prefix}/events.lance");
                let projection = automatic_storyline_projection_uri(&source).unwrap();
                prop_assert_eq!(projection, format!("s3://{bucket}/{prefix}/storyline"));
            }
        }
    }

    #[tokio::test]
    async fn inventory_is_sorted_and_uses_pinned_event_snapshots() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let storage = temp.path().join("capture");
        let source_b = append_note(&storage, "b").await?;
        let source_a = append_note(&storage, "a").await?;
        let projection_a = storage.join("agent/a/storyline");
        assert!(matches!(
            build_storyline_projection(&source_a, projection_a.to_string_lossy(), "a/events.lance")
                .await?,
            StorylineProjectionBuildOutcome::Built(_)
        ));

        let mount_root = storage.join("agent");
        let snapshot = DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(mount_root.to_string_lossy())?],
            Some("dataset".into()),
            CatalogSnapshotOptions::default(),
        )
        .await?;
        let inventory = automatic_projection_inventory(&snapshot)?;

        assert_eq!(inventory.snapshot_id, snapshot.snapshot_id());
        assert!(inventory.errors.is_empty());
        assert_eq!(
            inventory
                .targets
                .iter()
                .map(|target| target.source_path.as_str())
                .collect::<Vec<_>>(),
            ["a/events.lance", "b/events.lance"]
        );
        assert_eq!(inventory.targets[0].projection_path, "a/storyline");
        assert_eq!(inventory.targets[1].projection_path, "b/storyline");
        assert_eq!(
            inventory.targets[1].source_uri,
            std::fs::canonicalize(source_b)?.to_string_lossy()
        );
        assert_eq!(inventory.targets[1].source_snapshot.fact_rows, 1);
        let fresh = inspect_automatic_storyline_projection(&inventory.targets[0]).await?;
        assert_eq!(fresh.state, AutomaticProjectionState::Fresh);
        assert!(fresh.generation.is_some());

        let direct = DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(&source_a)?],
            Some("dataset".into()),
            CatalogSnapshotOptions::default(),
        )
        .await?;
        let direct = automatic_projection_inventory(&direct)?;
        assert_eq!(direct.targets[0].source_path, "events.lance");
        assert_eq!(direct.targets[0].projection_path, "storyline");
        Ok(())
    }

    #[tokio::test]
    async fn missing_inspection_is_observational() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let storage = temp.path().join("capture");
        append_note(&storage, "missing").await?;
        let mount_root = storage.join("agent");
        let snapshot = DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(mount_root.to_string_lossy())?],
            None,
            CatalogSnapshotOptions::default(),
        )
        .await?;
        let target = automatic_projection_inventory(&snapshot)?.targets.remove(0);

        let inspection = inspect_automatic_storyline_projection(&target).await?;
        assert_eq!(inspection.state, AutomaticProjectionState::Missing);
        assert_eq!(inspection.generation, None);
        assert_eq!(inspection.fact_rows, 1);
        assert!(!Path::new(&target.projection_uri).exists());
        Ok(())
    }

    #[tokio::test]
    async fn destination_existence_covers_local_and_object_stores() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let missing = temp.path().join("missing");
        assert!(!storyline_projection_destination_exists(missing.to_string_lossy()).await?);
        assert!(!missing.exists());

        let empty = temp.path().join("empty");
        std::fs::create_dir(&empty)?;
        assert!(storyline_projection_destination_exists(empty.to_string_lossy()).await?);

        let remote = format!(
            "shared-memory://pchronicle-automatic-exists-{}/storyline",
            uuid::Uuid::new_v4().simple()
        );
        assert!(!storyline_projection_destination_exists(&remote).await?);
        let store = OpendalStore::from_uri(&remote).await?;
        store
            .write_overwrite("sentinel", b"present".to_vec())
            .await?;
        assert!(storyline_projection_destination_exists(&remote).await?);
        Ok(())
    }

    #[tokio::test]
    async fn inspection_refuses_foreign_lineage_free_and_malformed_destinations() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let storage = temp.path().join("capture");
        let source_a = append_note(&storage, "a").await?;
        let source_b = append_note(&storage, "b").await?;
        let snapshot_b = probe_canonical_event_store(&source_b)
            .await?
            .expect("source b is canonical");
        let projection_b = storage.join("agent/b/storyline");
        assert!(matches!(
            build_storyline_projection(&source_a, projection_b.to_string_lossy(), "a/events.lance")
                .await?,
            StorylineProjectionBuildOutcome::Built(_)
        ));
        let target_b = AutomaticProjectionTarget {
            dataset: "dataset".into(),
            source_path: "b/events.lance".into(),
            source_uri: snapshot_b.source_uri.clone(),
            projection_path: "b/storyline".into(),
            projection_uri: projection_b.to_string_lossy().into_owned(),
            source_snapshot: snapshot_b,
        };
        let before = std::fs::read(projection_b.join("CURRENT"))?;
        let error = inspect_automatic_storyline_projection(&target_b)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("matching canonical source ownership")
        );
        assert_eq!(std::fs::read(projection_b.join("CURRENT"))?, before);

        let projection_a = storage.join("agent/a/storyline");
        assert!(matches!(
            build_storyline_projection(&source_a, projection_a.to_string_lossy(), "a/events.lance")
                .await?,
            StorylineProjectionBuildOutcome::Built(_)
        ));
        let snapshot_a = probe_canonical_event_store(&source_a)
            .await?
            .expect("source a is canonical");
        let target_a = AutomaticProjectionTarget {
            dataset: "dataset".into(),
            source_path: "a/events.lance".into(),
            source_uri: snapshot_a.source_uri.clone(),
            projection_path: "a/storyline".into(),
            projection_uri: projection_a.to_string_lossy().into_owned(),
            source_snapshot: snapshot_a,
        };
        let current = projection_a.join("CURRENT");
        let mut pointer: serde_json::Value = serde_json::from_slice(&std::fs::read(&current)?)?;
        pointer["committed"]
            .as_object_mut()
            .expect("CURRENT pointer object")
            .remove("projection");
        std::fs::write(&current, serde_json::to_vec(&pointer)?)?;
        let lineage_free = std::fs::read(&current)?;
        let error = inspect_automatic_storyline_projection(&target_a)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("has no canonical lineage"));
        assert_eq!(std::fs::read(&current)?, lineage_free);

        std::fs::write(&current, b"{broken")?;
        let malformed = std::fs::read(&current)?;
        assert!(
            inspect_automatic_storyline_projection(&target_a)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&current)?, malformed);
        Ok(())
    }

    #[tokio::test]
    async fn inventory_reports_malformed_canonical_candidates_without_targeting_them() -> Result<()>
    {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("bad/events.lance");
        std::fs::create_dir_all(&source)?;
        std::fs::write(source.join("_manifest.json"), b"{broken")?;
        let snapshot = DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(temp.path().to_string_lossy())?],
            None,
            CatalogSnapshotOptions::default()
                .with_error_policy(crate::store::CatalogErrorPolicy::Report),
        )
        .await?;

        let inventory = automatic_projection_inventory(&snapshot)?;
        assert!(inventory.targets.is_empty());
        assert_eq!(
            inventory.errors,
            [AutomaticProjectionInventoryError {
                dataset: "dataset".into(),
                source_path: "bad/events.lance".into(),
                projection_path: "bad/storyline".into(),
            }]
        );
        Ok(())
    }

    #[tokio::test]
    async fn maintenance_builds_syncs_and_noops() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let storage = temp.path().join("capture");
        let source = append_note_at(&storage, "run", 0).await?;
        let projection = storage.join("agent/run/storyline");
        let target = target_for(&source, &projection).await?;

        let built = maintain_automatic_storyline_projection(&target).await?;
        assert_eq!(built.mode, AutomaticProjectionMaintenanceMode::Built);
        assert!(built.published());
        assert_eq!(built.fact_rows, 1);

        let unchanged = maintain_automatic_storyline_projection(&target).await?;
        assert_eq!(
            unchanged.mode,
            AutomaticProjectionMaintenanceMode::Unchanged
        );
        assert!(!unchanged.published());
        assert_eq!(unchanged.generation, built.generation);

        append_note_at(&storage, "run", 1).await?;
        let refreshed = target_for(&source, &projection).await?;
        let incremental = maintain_automatic_storyline_projection(&refreshed).await?;
        assert_eq!(
            incremental.mode,
            AutomaticProjectionMaintenanceMode::Incremental
        );
        assert!(incremental.published());
        assert_eq!(incremental.fact_rows, 2);
        assert_ne!(incremental.generation, built.generation);
        Ok(())
    }

    #[tokio::test]
    async fn maintenance_rebuilds_only_owned_outputs() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let storage = temp.path().join("capture");
        let source = append_note(&storage, "run").await?;
        let projection = storage.join("agent/run/storyline");
        let target = target_for(&source, &projection).await?;
        assert_eq!(
            maintain_automatic_storyline_projection(&target).await?.mode,
            AutomaticProjectionMaintenanceMode::Built
        );

        let current = projection.join("CURRENT");
        let mut pointer: serde_json::Value = serde_json::from_slice(&std::fs::read(&current)?)?;
        pointer["committed"]["projection"]["recipe_hash"] = serde_json::json!("blake3:obsolete");
        std::fs::write(&current, serde_json::to_vec(&pointer)?)?;
        let rebuilt = maintain_automatic_storyline_projection(&target).await?;
        assert_eq!(rebuilt.mode, AutomaticProjectionMaintenanceMode::Rebuilt);
        assert!(rebuilt.published());

        let mut pointer: serde_json::Value = serde_json::from_slice(&std::fs::read(&current)?)?;
        pointer["committed"]["projection"]["source"]["fact_version"] = serde_json::json!(999);
        pointer["committed"]["projection"]["source"]["fact_rows"] = serde_json::json!(999);
        std::fs::write(&current, serde_json::to_vec(&pointer)?)?;
        let rebuilt_non_monotonic = maintain_automatic_storyline_projection(&target).await?;
        assert_eq!(
            rebuilt_non_monotonic.mode,
            AutomaticProjectionMaintenanceMode::Rebuilt
        );

        let mut pointer: serde_json::Value = serde_json::from_slice(&std::fs::read(&current)?)?;
        pointer["committed"]["projection"]["source"]["source_uri"] =
            serde_json::json!("/foreign/events.lance");
        std::fs::write(&current, serde_json::to_vec(&pointer)?)?;
        let before = std::fs::read(&current)?;
        let error = maintain_automatic_storyline_projection(&target)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("matching canonical source"));
        assert_eq!(std::fs::read(&current)?, before);
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_maintenance_accepts_one_fresh_winner() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let storage = temp.path().join("capture");
        let source = append_note(&storage, "run").await?;
        let projection = storage.join("agent/run/storyline");
        let target = target_for(&source, &projection).await?;

        let (left, right) = tokio::join!(
            maintain_automatic_storyline_projection(&target),
            maintain_automatic_storyline_projection(&target),
        );
        let reports = [left?, right?];
        assert_eq!(
            reports
                .iter()
                .filter(|report| report.mode == AutomaticProjectionMaintenanceMode::Built)
                .count(),
            1
        );
        assert_eq!(
            reports
                .iter()
                .filter(|report| {
                    report.mode == AutomaticProjectionMaintenanceMode::ConcurrentWinner
                })
                .count(),
            1
        );
        assert_eq!(
            inspect_automatic_storyline_projection(&target).await?.state,
            AutomaticProjectionState::Fresh
        );
        Ok(())
    }
}
