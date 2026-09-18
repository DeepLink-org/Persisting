use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{OnceCell, watch};

use super::{CatalogAcl, MAX_CATALOG_CONFIG_BYTES};
use crate::server::ui_cache::BrowseCoordinator;

const RELOAD_INTERVAL: Duration = Duration::from_secs(3);
const ERROR_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// Authentication and public browsing use the same immutable request snapshot.
pub(crate) struct CatalogSnapshot {
    pub(crate) acl: CatalogAcl,
    browse: Arc<OnceCell<BrowseCoordinator>>,
}

impl CatalogSnapshot {
    pub(crate) async fn browse(&self) -> &BrowseCoordinator {
        self.browse
            .get_or_init(|| BrowseCoordinator::start(self.acl.public_mounts()))
            .await
    }
}

pub(crate) struct CatalogState {
    current: watch::Receiver<Arc<CatalogSnapshot>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for CatalogState {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl CatalogState {
    pub(crate) fn new(acl: CatalogAcl, path: Option<PathBuf>) -> Self {
        let snapshot = Arc::new(CatalogSnapshot {
            acl,
            browse: Arc::new(OnceCell::new()),
        });
        let (sender, current) = watch::channel(snapshot);
        let task = path.map(|path| tokio::spawn(reload_loop(path, sender)));
        Self { current, task }
    }

    pub(crate) fn snapshot(&self) -> Arc<CatalogSnapshot> {
        self.current.borrow().clone()
    }
}

// Return only fixed diagnostic categories: TOML errors can include source lines
// containing credentials, and validation errors can include user-supplied text.
fn read_update(
    path: &Path,
    previous: Option<blake3::Hash>,
) -> Result<Option<(blake3::Hash, CatalogAcl)>, &'static str> {
    let file = std::fs::File::open(path).map_err(|_| "read_failed")?;
    let metadata = file.metadata().map_err(|_| "read_failed")?;
    if !metadata.is_file() || metadata.len() > MAX_CATALOG_CONFIG_BYTES {
        return Err("invalid_file");
    }
    let mut content = String::new();
    file.take(MAX_CATALOG_CONFIG_BYTES + 1)
        .read_to_string(&mut content)
        .map_err(|_| "read_failed")?;
    if content.len() as u64 > MAX_CATALOG_CONFIG_BYTES {
        return Err("invalid_file");
    }
    let hash = blake3::hash(content.as_bytes());
    if previous == Some(hash) {
        return Ok(None);
    }
    let document = super::parse_catalog_file(&content).map_err(|_| "invalid_toml")?;
    let acl = CatalogAcl::from_document(document).map_err(|_| "invalid_catalog")?;
    Ok(Some((hash, acl)))
}

async fn reload_loop(path: PathBuf, sender: watch::Sender<Arc<CatalogSnapshot>>) {
    let mut interval = tokio::time::interval(RELOAD_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut fingerprint = None;
    let mut last_error_log = None;
    loop {
        interval.tick().await;
        let path = path.clone();
        let result = tokio::task::spawn_blocking(move || read_update(&path, fingerprint))
            .await
            .unwrap_or(Err("reload_task_failed"));
        let result = match result {
            Ok(Some((hash, acl))) => {
                let previous = sender.borrow().clone();
                if acl.libraries != previous.acl.libraries {
                    Err("dataset_changes_require_restart")
                } else {
                    fingerprint = Some(hash);
                    if acl != previous.acl {
                        let browse = if acl.public_datasets == previous.acl.public_datasets {
                            previous.browse.clone()
                        } else {
                            Arc::new(OnceCell::new())
                        };
                        sender.send_replace(Arc::new(CatalogSnapshot { acl, browse }));
                        tracing::info!(target: "pchronicle.serve", "catalog ACL reloaded");
                    }
                    Ok(())
                }
            }
            Ok(None) => Ok(()),
            Err(reason) => Err(reason),
        };
        match result {
            Ok(()) => {
                if last_error_log.take().is_some() {
                    tracing::info!(target: "pchronicle.serve", "catalog config reload recovered");
                }
            }
            Err(reason) => {
                if last_error_log.is_none_or(|last: Instant| last.elapsed() >= ERROR_LOG_INTERVAL) {
                    tracing::error!(
                        target: "pchronicle.serve",
                        reason,
                        "catalog config reload rejected; keeping last valid ACL; requested grants and revocations have not taken effect"
                    );
                    last_error_log = Some(Instant::now());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_errors_never_include_config_values() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("catalog.toml");
        std::fs::write(&path, "secret_key = \"SECRET_MUST_NOT_BE_LOGGED\" invalid").unwrap();
        assert_eq!(read_update(&path, None).unwrap_err(), "invalid_toml");
        std::fs::write(&path, "[datasets.prod]\nuri = \"/tmp/data\"\n[users.SECRET_MUST_NOT_BE_LOGGED]\naccess_key = \"\"\nsecret_key = \"secret\"\n").unwrap();
        assert_eq!(read_update(&path, None).unwrap_err(), "invalid_catalog");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(read_update(&path, None).unwrap_err(), "read_failed");
    }

    #[test]
    fn reload_detects_atomic_replacement_and_bounds_input() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("catalog.toml");
        std::fs::write(&path, super::super::tests::SAMPLE).unwrap();
        let (hash, _) = read_update(&path, None).unwrap().unwrap();
        assert!(read_update(&path, Some(hash)).unwrap().is_none());
        let replacement = path.with_extension("tmp");
        std::fs::write(
            &replacement,
            super::super::tests::SAMPLE.replace("USER_SK", "NEXT_SK"),
        )
        .unwrap();
        std::fs::rename(replacement, &path).unwrap();
        let (_, acl) = read_update(&path, Some(hash)).unwrap().unwrap();
        assert!(acl.authenticate("USER_AK", "NEXT_SK").is_some());
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_CATALOG_CONFIG_BYTES + 1)
            .unwrap();
        assert_eq!(read_update(&path, Some(hash)).unwrap_err(), "invalid_file");
    }

    #[tokio::test]
    async fn dropping_catalog_stops_reload_task() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("catalog.toml");
        std::fs::write(&path, super::super::tests::SAMPLE).unwrap();
        let state = CatalogState::new(CatalogAcl::load(&path).unwrap(), Some(path));
        let task = state.task.as_ref().unwrap().abort_handle();
        drop(state);
        tokio::task::yield_now().await;
        assert!(task.is_finished());
    }
}
