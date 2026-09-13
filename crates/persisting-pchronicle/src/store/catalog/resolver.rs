//! One resolver for atomic Datasets and recursive DatasetMounts.
//!
//! A Dataset is a single queryable leaf. A DatasetMount is a named path whose
//! recursive contents may contain many leaves. The mode is explicit so a UI
//! cache can never silently become an authoritative CLI resolution.
use super::{
    CatalogSnapshotOptions, CatalogSourceKind, DatasetCatalogSnapshot, DatasetMount, QueryScope,
};
use crate::store::DatasetLocation;
use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolveMode {
    /// Resolve only source paths supplied by the UI index.
    Cached,
    /// Discover and pin the requested scope from the backing location.
    Fresh,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveTarget {
    /// Resolve all configured mounts together in one query snapshot.
    Catalog,
    Dataset {
        mount: String,
        file: String,
    },
    Mount {
        mount: String,
        prefix: Option<String>,
    },
}

/// One atomic, queryable dataset leaf. It is deliberately separate from a
/// DatasetMount, which is a recursive namespace and may contain many leaves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dataset {
    location: DatasetLocation,
}

/// A persisted, manifest-derived identity for one atomic dataset under a
/// mount. It contains no open handles and is safe to use as a UI hint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedDataset {
    mount: String,
    file: String,
}

impl CachedDataset {
    pub fn new(mount: impl Into<String>, file: impl Into<String>) -> Self {
        Self {
            mount: mount.into(),
            file: file.into(),
        }
    }

    pub fn mount(&self) -> &str {
        &self.mount
    }
    pub fn file(&self) -> &str {
        &self.file
    }
}

impl Dataset {
    pub fn new(uri: impl AsRef<str>) -> Result<Self> {
        Ok(Self {
            location: DatasetLocation::parse(uri.as_ref())?,
        })
    }

    pub fn uri(&self) -> &str {
        self.location.as_str()
    }

    pub async fn resolve(&self, options: CatalogSnapshotOptions) -> Result<DatasetCatalogSnapshot> {
        let mount = DatasetMount::new("dataset", self.uri())?;
        let snapshot = DatasetCatalogSnapshot::discover_scoped(
            vec![mount],
            Some("dataset".into()),
            options,
            QueryScope {
                dataset: "dataset".into(),
                source_file: Some(".".into()),
            },
        )
        .await?;
        anyhow::ensure!(
            snapshot.datasets().iter().all(|dataset| {
                dataset
                    .sources
                    .iter()
                    .all(|source| source.kind != CatalogSourceKind::Directory)
            }),
            "atomic Dataset resolved to a recursive DatasetMount"
        );
        let leaves = snapshot
            .datasets()
            .iter()
            .flat_map(|dataset| dataset.sources.iter())
            .filter(|source| source.kind != CatalogSourceKind::Directory)
            .count();
        anyhow::ensure!(
            leaves == 1,
            "atomic Dataset must resolve to exactly one leaf"
        );
        Ok(snapshot)
    }
}

#[derive(Clone, Debug)]
pub struct DatasetResolver {
    mounts: Vec<DatasetMount>,
    default_dataset: Option<String>,
    options: CatalogSnapshotOptions,
}

impl DatasetResolver {
    pub fn new(
        mounts: Vec<DatasetMount>,
        default_dataset: Option<String>,
        options: CatalogSnapshotOptions,
    ) -> Self {
        Self {
            mounts,
            default_dataset,
            options,
        }
    }

    pub async fn resolve(
        &self,
        target: ResolveTarget,
        mode: ResolveMode,
        cached_datasets: &[CachedDataset],
    ) -> Result<DatasetCatalogSnapshot> {
        let scope = match target {
            ResolveTarget::Catalog => {
                return match mode {
                    ResolveMode::Fresh => {
                        DatasetCatalogSnapshot::discover(
                            self.mounts.clone(),
                            self.default_dataset.clone(),
                            self.options,
                        )
                        .await
                    }
                    ResolveMode::Cached => {
                        bail!("cached Catalog resolution requires a scoped mount")
                    }
                };
            }
            ResolveTarget::Dataset { mount, file } => QueryScope {
                dataset: mount,
                source_file: Some(file),
            },
            ResolveTarget::Mount { mount, prefix } => QueryScope {
                dataset: mount,
                source_file: prefix,
            },
        };
        let cached_files: Vec<_> = cached_datasets
            .iter()
            .filter(|dataset| dataset.mount == scope.dataset)
            .map(|dataset| dataset.file.clone())
            .collect();
        match mode {
            ResolveMode::Fresh => {
                DatasetCatalogSnapshot::discover_scoped(
                    self.mounts.clone(),
                    self.default_dataset.clone(),
                    self.options,
                    scope,
                )
                .await
            }
            ResolveMode::Cached if cached_files.is_empty() => {
                bail!("cached Dataset resolution requires at least one indexed source")
            }
            ResolveMode::Cached => {
                DatasetCatalogSnapshot::discover_scoped_from_cached_files(
                    self.mounts.clone(),
                    self.default_dataset.clone(),
                    self.options,
                    scope,
                    cached_files,
                )
                .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_dataset_keeps_location_identity() {
        let dataset = Dataset::new("memory://resolver-test").unwrap();
        assert_eq!(dataset.uri(), "memory://resolver-test");
    }

    #[tokio::test]
    async fn cached_resolution_never_silently_discovers() {
        let mount = DatasetMount::new("prod", "memory://resolver-test").unwrap();
        let resolver = DatasetResolver::new(vec![mount], None, CatalogSnapshotOptions::default());
        let error = resolver
            .resolve(
                ResolveTarget::Mount {
                    mount: "prod".into(),
                    prefix: None,
                },
                ResolveMode::Cached,
                &[],
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cached Dataset resolution"));
    }
}
