use super::*;

use std::collections::BTreeMap;

use persisting_pchronicle::storage::{
    automatic_projection_inventory, maintain_automatic_storyline_projection,
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProjectionSupervisorOptions {
    pub(crate) interval: Duration,
    pub(crate) max_backoff: Duration,
    pub(crate) max_concurrent: usize,
    /// Delay runtime refreshes after an existing canonical source changes.
    /// Startup projections and newly discovered sources are not delayed.
    pub(crate) projection_idle: Duration,
}

impl Default for ProjectionSupervisorOptions {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            max_concurrent: 16,
            projection_idle: Duration::ZERO,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProjectionIterationReport {
    pub(crate) succeeded: usize,
    pub(crate) failed: usize,
    pub(crate) publications: usize,
    pub(crate) catalog_refreshes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectionDiagnostic {
    pub(crate) source_path: String,
    pub(crate) projection_path: String,
    pub(crate) status: &'static str,
    pub(crate) retry_ms: u64,
}

#[derive(Debug, Clone, Copy)]
struct RetryState {
    failures: u32,
    next_attempt: tokio::time::Instant,
}

pub(crate) struct ProjectionSupervisor {
    config: server::ChronicleServerConfig,
    warehouse: Option<server::PreparedWarehouse>,
    options: ProjectionSupervisorOptions,
    diagnostics: tokio::sync::mpsc::Sender<ProjectionDiagnostic>,
    retries: BTreeMap<String, RetryState>,
    source_watermarks: BTreeMap<String, (u64, u64, u64)>,
    source_idle_since: BTreeMap<String, tokio::time::Instant>,
    catalog_retry: Option<RetryState>,
    observed_snapshot_id: Option<String>,
    catalog_dirty: bool,
}

impl ProjectionSupervisor {
    pub(crate) fn new(
        config: server::ChronicleServerConfig,
        warehouse: Option<server::PreparedWarehouse>,
        diagnostics: tokio::sync::mpsc::Sender<ProjectionDiagnostic>,
    ) -> Self {
        Self {
            config,
            warehouse,
            options: ProjectionSupervisorOptions::default(),
            diagnostics,
            retries: BTreeMap::new(),
            source_watermarks: BTreeMap::new(),
            source_idle_since: BTreeMap::new(),
            catalog_retry: None,
            observed_snapshot_id: None,
            catalog_dirty: false,
        }
    }

    pub(crate) fn with_projection_idle(
        config: server::ChronicleServerConfig,
        warehouse: Option<server::PreparedWarehouse>,
        projection_idle: Duration,
        diagnostics: tokio::sync::mpsc::Sender<ProjectionDiagnostic>,
    ) -> Self {
        let mut supervisor = Self::new(config, warehouse, diagnostics);
        supervisor.options.projection_idle = projection_idle;
        supervisor
    }

    pub(crate) fn set_warehouse(&mut self, warehouse: Option<server::PreparedWarehouse>) {
        self.warehouse = warehouse;
        self.catalog_dirty = false;
        self.catalog_retry = None;
    }

    async fn discover(&self) -> Result<DatasetCatalogSnapshot> {
        let mut options = self.config.catalog_options;
        options.error_policy = CatalogErrorPolicy::Report;
        DatasetCatalogSnapshot::discover(
            self.config.datasets.clone(),
            self.config.default_dataset.clone(),
            options,
        )
        .await
    }

    pub(crate) async fn converge_before_readiness(&mut self) -> Result<()> {
        let snapshot = self.discover().await?;
        let inventory = automatic_projection_inventory(&snapshot)?;
        let initial_watermarks = inventory
            .targets
            .iter()
            .map(|target| {
                (
                    target.source_uri.clone(),
                    (
                        target.source_snapshot.fact_version,
                        target.source_snapshot.fact_rows,
                        target.source_snapshot.layout_revision,
                    ),
                )
            })
            .collect();
        let mut failures = inventory.errors.len();
        let outcomes = stream::iter(inventory.targets)
            .map(|target| async move {
                maintain_automatic_storyline_projection(&target)
                    .await
                    .map(|_| ())
            })
            .buffer_unordered(self.options.max_concurrent.max(1))
            .collect::<Vec<_>>()
            .await;
        failures =
            failures.saturating_add(outcomes.iter().filter(|result| result.is_err()).count());
        anyhow::ensure!(
            failures == 0,
            "automatic Storyline projection startup failed for {failures} source(s)"
        );

        let converged = self.discover().await?;
        self.source_watermarks = initial_watermarks;
        self.source_idle_since.clear();
        self.observed_snapshot_id = Some(converged.snapshot_id().to_string());
        self.catalog_dirty = false;
        Ok(())
    }

    pub(crate) async fn run_iteration(
        &mut self,
        now: tokio::time::Instant,
    ) -> ProjectionIterationReport {
        let mut report = ProjectionIterationReport::default();
        let snapshot = match self.discover().await {
            Ok(snapshot) => snapshot,
            Err(_) => {
                report.failed = 1;
                self.send_diagnostic("catalog", "", "error", self.options.interval);
                return report;
            }
        };
        if self.observed_snapshot_id.as_deref() != Some(snapshot.snapshot_id()) {
            self.observed_snapshot_id = Some(snapshot.snapshot_id().to_string());
        }
        let inventory = match automatic_projection_inventory(&snapshot) {
            Ok(inventory) => inventory,
            Err(_) => {
                report.failed = 1;
                self.send_diagnostic("catalog", "", "error", self.options.interval);
                return report;
            }
        };

        for error in inventory.errors {
            let key = format!("error:{}/{}", error.dataset, error.source_path);
            if self.retry_is_due(&key, now) {
                report.failed = report.failed.saturating_add(1);
                let delay = self.record_failure(key, now);
                self.send_diagnostic(&error.source_path, &error.projection_path, "error", delay);
            }
        }

        let due = inventory
            .targets
            .into_iter()
            .filter(|target| {
                let key = target.source_uri.clone();
                let watermark = (
                    target.source_snapshot.fact_version,
                    target.source_snapshot.fact_rows,
                    target.source_snapshot.layout_revision,
                );
                let previous = self.source_watermarks.insert(key.clone(), watermark);
                if let Some(previous) = previous {
                    if previous != watermark {
                        self.source_idle_since.entry(key.clone()).or_insert(now);
                    }
                } else {
                    // A new canonical events.lance source is a structural
                    // Catalog change. Existing sources only update their
                    // watermark and remain queryable through live reads.
                    self.catalog_dirty = true;
                }
                let idle_elapsed = self
                    .source_idle_since
                    .get(&key)
                    .map(|since| now.saturating_duration_since(*since))
                    .unwrap_or(self.options.projection_idle);
                let idle_ready = self.options.projection_idle.is_zero()
                    || idle_elapsed >= self.options.projection_idle;
                idle_ready && self.retry_is_due(&key, now)
            })
            .collect::<Vec<_>>();
        let outcomes = stream::iter(due)
            .map(|target| async move {
                let result = maintain_automatic_storyline_projection(&target).await;
                (target, result)
            })
            .buffer_unordered(self.options.max_concurrent.max(1))
            .collect::<Vec<_>>()
            .await;
        for (target, outcome) in outcomes {
            match outcome {
                Ok(maintenance) => {
                    self.retries.remove(&target.source_uri);
                    self.source_idle_since.remove(&target.source_uri);
                    report.succeeded = report.succeeded.saturating_add(1);
                    if maintenance.published() {
                        report.publications = report.publications.saturating_add(1);
                        self.catalog_dirty = true;
                    }
                }
                Err(_) => {
                    report.failed = report.failed.saturating_add(1);
                    let delay = self.record_failure(target.source_uri, now);
                    self.send_diagnostic(
                        &target.source_path,
                        &target.projection_path,
                        "error",
                        delay,
                    );
                }
            }
        }

        let catalog_due = self
            .catalog_retry
            .is_none_or(|retry| retry.next_attempt <= now);
        if self.catalog_dirty && catalog_due {
            if let Some(warehouse) = self.warehouse.clone() {
                match warehouse.refresh_catalog().await {
                    Ok(snapshot_id) => {
                        self.observed_snapshot_id = Some(snapshot_id);
                        self.catalog_dirty = false;
                        self.catalog_retry = None;
                        report.catalog_refreshes = 1;
                    }
                    Err(_) => {
                        let failures = self
                            .catalog_retry
                            .map_or(1, |retry| retry.failures.saturating_add(1));
                        let delay = self.retry_delay(failures);
                        self.catalog_retry = Some(RetryState {
                            failures,
                            next_attempt: now + delay,
                        });
                        self.send_diagnostic("catalog", "", "error", delay);
                    }
                }
            }
        }
        report
    }

    pub(crate) async fn run(mut self, mut stop: tokio::sync::watch::Receiver<bool>) -> Result<()> {
        loop {
            tokio::select! {
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return Ok(());
                    }
                }
                _ = tokio::time::sleep(self.options.interval) => {
                    self.run_iteration(tokio::time::Instant::now()).await;
                }
            }
        }
    }

    fn retry_is_due(&self, key: &str, now: tokio::time::Instant) -> bool {
        self.retries
            .get(key)
            .is_none_or(|retry| retry.next_attempt <= now)
    }

    fn record_failure(&mut self, key: String, now: tokio::time::Instant) -> Duration {
        let failures = self
            .retries
            .get(&key)
            .map_or(1, |retry| retry.failures.saturating_add(1));
        let delay = self.retry_delay(failures);
        self.retries.insert(
            key,
            RetryState {
                failures,
                next_attempt: now + delay,
            },
        );
        delay
    }

    fn retry_delay(&self, failures: u32) -> Duration {
        let exponent = failures.saturating_sub(1).min(20);
        let multiplier = 1u32.checked_shl(exponent).unwrap_or(u32::MAX);
        self.options
            .interval
            .saturating_mul(multiplier)
            .min(self.options.max_backoff)
    }

    fn send_diagnostic(
        &self,
        source_path: &str,
        projection_path: &str,
        status: &'static str,
        retry: Duration,
    ) {
        let retry_ms = retry.as_millis().try_into().unwrap_or(u64::MAX);
        let _ = self.diagnostics.try_send(ProjectionDiagnostic {
            source_path: source_path.to_string(),
            projection_path: projection_path.to_string(),
            status,
            retry_ms,
        });
    }
}

pub(crate) fn sanitize_log_field(value: &str) -> String {
    value.chars().flat_map(char::escape_default).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_pchronicle::storage::{
        automatic_projection_inventory, build_storyline_projection,
        inspect_automatic_storyline_projection, AutomaticProjectionState, RawEventLanceStore,
        StoryCoords,
    };

    async fn append_note(storage: &Path, run_id: &str, seq: u64) -> Result<PathBuf> {
        let coords = StoryCoords::new(
            storage.to_string_lossy(),
            "agent",
            run_id,
            Some(run_id.into()),
        );
        RawEventLanceStore
            .append_events(
                &coords,
                &[persisting_pchronicle::model::EventRecord {
                    identity: Default::default(),
                    seq,
                    source: "test".into(),
                    kind: "note".into(),
                    timestamp: None,
                    session_id: Some(run_id.into()),
                    agent_id: Some("agent".into()),
                    parent_uuid: None,
                    trace_id: None,
                    call_id: None,
                    subagent_id: None,
                    parent_agent_id: None,
                    branch: None,
                    parent_call_id: None,
                    payload: serde_json::json!({"content": format!("{run_id}-{seq}")}),
                }],
            )
            .await?;
        persisting_pchronicle::storage::raw_event_lance_path(&coords)
    }

    fn config(root: &Path) -> Result<server::ChronicleServerConfig> {
        server::ChronicleServerConfig::mounted(vec![DatasetMount::default(root.to_string_lossy())?])
    }

    #[tokio::test]
    async fn startup_converges_every_initial_projection_before_returning() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let storage = temp.path().join("capture");
        append_note(&storage, "b", 0).await?;
        append_note(&storage, "a", 0).await?;
        let config = config(&storage.join("agent"))?;
        let (diagnostics, _receiver) = tokio::sync::mpsc::channel(16);
        let mut supervisor = ProjectionSupervisor::new(config.clone(), None, diagnostics);

        supervisor.converge_before_readiness().await?;

        let snapshot = DatasetCatalogSnapshot::discover(
            config.datasets,
            config.default_dataset,
            config.catalog_options,
        )
        .await?;
        let inventory = automatic_projection_inventory(&snapshot)?;
        assert_eq!(inventory.targets.len(), 2);
        for target in inventory.targets {
            assert_eq!(
                inspect_automatic_storyline_projection(&target).await?.state,
                AutomaticProjectionState::Fresh
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn startup_rejects_a_foreign_deterministic_destination() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let storage = temp.path().join("capture");
        let source_a = append_note(&storage, "a", 0).await?;
        append_note(&storage, "b", 0).await?;
        let projection_b = storage.join("agent/b/storyline");
        build_storyline_projection(
            source_a.to_string_lossy(),
            projection_b.to_string_lossy(),
            "a/events.lance",
        )
        .await?;
        let before = std::fs::read(projection_b.join("CURRENT"))?;
        let (diagnostics, _receiver) = tokio::sync::mpsc::channel(16);
        let mut supervisor =
            ProjectionSupervisor::new(config(&storage.join("agent"))?, None, diagnostics);

        let error = supervisor.converge_before_readiness().await.unwrap_err();
        assert!(error.to_string().contains("startup failed for 1 source"));
        assert_eq!(std::fs::read(projection_b.join("CURRENT"))?, before);
        Ok(())
    }

    #[tokio::test]
    async fn runtime_discovers_sources_and_coalesces_catalog_refreshes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("dataset");
        std::fs::create_dir(&root)?;
        let config = config(&root)?;
        let (diagnostics, _receiver) = tokio::sync::mpsc::channel(16);
        let mut supervisor = ProjectionSupervisor::new(config.clone(), None, diagnostics);
        supervisor.converge_before_readiness().await?;
        let warehouse = server::PreparedWarehouse::prepare(config).await?;
        let initial_snapshot = warehouse.current_snapshot_id().await.unwrap();
        supervisor.set_warehouse(Some(warehouse.clone()));

        let source_a = append_note(&root, "a", 0).await?;
        let source_b = append_note(&root, "b", 0).await?;
        let report = supervisor.run_iteration(tokio::time::Instant::now()).await;
        assert_eq!(report.succeeded, 2);
        assert_eq!(report.failed, 0);
        assert_eq!(report.publications, 2);
        assert_eq!(report.catalog_refreshes, 1);
        assert_ne!(
            warehouse.current_snapshot_id().await.unwrap(),
            initial_snapshot
        );
        for source in [source_a, source_b] {
            let snapshot = persisting_pchronicle::storage::probe_canonical_event_store(
                source.to_string_lossy(),
            )
            .await?
            .unwrap();
            let target = persisting_pchronicle::storage::AutomaticProjectionTarget {
                dataset: "dataset".into(),
                source_path: format!(
                    "{}/events.lance",
                    source
                        .parent()
                        .unwrap()
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                ),
                source_uri: snapshot.source_uri.clone(),
                projection_path: format!(
                    "{}/storyline",
                    source
                        .parent()
                        .unwrap()
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                ),
                projection_uri: source
                    .parent()
                    .unwrap()
                    .join("storyline")
                    .to_string_lossy()
                    .into_owned(),
                source_snapshot: snapshot,
            };
            assert_eq!(
                inspect_automatic_storyline_projection(&target).await?.state,
                AutomaticProjectionState::Fresh
            );
        }

        append_note(&root, "a", 1).await?;
        let report = supervisor.run_iteration(tokio::time::Instant::now()).await;
        assert_eq!(report.publications, 1);
        assert_eq!(report.catalog_refreshes, 1);
        Ok(())
    }

    #[tokio::test]
    async fn projection_idle_defers_existing_source_until_quiet_window() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("dataset");
        std::fs::create_dir(&root)?;
        let config = config(&root)?;
        let (diagnostics, _receiver) = tokio::sync::mpsc::channel(16);
        let mut supervisor = ProjectionSupervisor::with_projection_idle(
            config,
            None,
            Duration::from_secs(30 * 60),
            diagnostics,
        );
        supervisor.converge_before_readiness().await?;

        append_note(&root, "run", 0).await?;
        let initial = supervisor.run_iteration(tokio::time::Instant::now()).await;
        assert_eq!(initial.publications, 1);

        append_note(&root, "run", 1).await?;
        let changed_at = tokio::time::Instant::now();
        let deferred = supervisor.run_iteration(changed_at).await;
        assert_eq!(deferred.publications, 0);

        let before_idle = supervisor
            .run_iteration(changed_at + Duration::from_secs(30 * 60 - 1))
            .await;
        assert_eq!(before_idle.publications, 0);

        let refreshed = supervisor
            .run_iteration(changed_at + Duration::from_secs(30 * 60))
            .await;
        assert_eq!(refreshed.publications, 1);
        Ok(())
    }

    #[tokio::test]
    async fn runtime_backoff_is_per_source_and_does_not_delay_healthy_sources() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("dataset");
        let good_source = append_note(&root, "good", 0).await?;
        append_note(&root, "bad", 0).await?;
        let bad_projection = root.join("agent/bad/storyline");
        build_storyline_projection(
            good_source.to_string_lossy(),
            bad_projection.to_string_lossy(),
            "good/events.lance",
        )
        .await?;
        let (diagnostics, mut receiver) = tokio::sync::mpsc::channel(16);
        let mut supervisor =
            ProjectionSupervisor::new(config(&root.join("agent"))?, None, diagnostics);
        supervisor.options.interval = Duration::from_millis(10);
        supervisor.options.max_backoff = Duration::from_millis(40);
        let now = tokio::time::Instant::now();

        let first = supervisor.run_iteration(now).await;
        assert_eq!(first.succeeded, 1);
        assert_eq!(first.failed, 1);
        let diagnostic = receiver.try_recv()?;
        assert_eq!(diagnostic.source_path, "bad/events.lance");
        assert_eq!(diagnostic.retry_ms, 10);

        let immediate = supervisor.run_iteration(now).await;
        assert_eq!(immediate.succeeded, 1);
        assert_eq!(immediate.failed, 0);
        let retried = supervisor
            .run_iteration(now + Duration::from_millis(10))
            .await;
        assert_eq!(retried.failed, 1);
        assert_eq!(receiver.try_recv()?.retry_ms, 20);
        assert_eq!(supervisor.retry_delay(20), Duration::from_millis(40));
        Ok(())
    }

    #[tokio::test]
    async fn failed_catalog_refresh_stays_dirty_and_retries_independently() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("dataset");
        std::fs::create_dir(&root)?;
        let config = config(&root)?;
        let (diagnostics, _receiver) = tokio::sync::mpsc::channel(16);
        let mut supervisor = ProjectionSupervisor::new(config.clone(), None, diagnostics);
        supervisor.options.interval = Duration::from_millis(10);
        supervisor.options.max_backoff = Duration::from_millis(40);
        supervisor.converge_before_readiness().await?;
        let warehouse = server::PreparedWarehouse::prepare(config).await?;
        supervisor.set_warehouse(Some(warehouse));

        append_note(&root, "run", 0).await?;
        std::fs::create_dir(root.join("broken"))?;
        std::fs::write(root.join("broken/CURRENT"), "{")?;
        let now = tokio::time::Instant::now();
        let failed = supervisor.run_iteration(now).await;
        assert_eq!(failed.publications, 1);
        assert_eq!(failed.catalog_refreshes, 0);
        assert!(supervisor.catalog_dirty);
        assert_eq!(supervisor.catalog_retry.unwrap().failures, 1);

        std::fs::remove_dir_all(root.join("broken"))?;
        let deferred = supervisor.run_iteration(now).await;
        assert_eq!(deferred.catalog_refreshes, 0);
        assert!(supervisor.catalog_dirty);
        let recovered = supervisor
            .run_iteration(now + Duration::from_millis(10))
            .await;
        assert_eq!(recovered.catalog_refreshes, 1);
        assert!(!supervisor.catalog_dirty);
        assert!(supervisor.catalog_retry.is_none());
        Ok(())
    }

    #[test]
    fn diagnostics_escape_newlines_and_terminal_control_sequences() {
        assert_eq!(sanitize_log_field("bad\n\u{1b}[31m"), "bad\\n\\u{1b}[31m");
        assert!(!sanitize_log_field("bad\n\u{1b}[31m").contains('\n'));
        assert!(!sanitize_log_field("bad\n\u{1b}[31m").contains('\u{1b}'));
    }
}
