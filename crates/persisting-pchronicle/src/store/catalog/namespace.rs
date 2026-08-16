use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::Serialize;

use super::{DatasetCatalogSnapshot, DiscoveredSource, NamespacePath};

const DEFAULT_PAGE_LIMIT: usize = 100;
const MAX_PAGE_LIMIT: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogPage<T> {
    pub items: Vec<T>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogNamespace {
    pub path: NamespacePath,
    pub mounted: bool,
    pub sql_alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogSourceDescription {
    pub namespace: NamespacePath,
    pub sql_alias: String,
    pub source: DiscoveredSource,
}

impl DatasetCatalogSnapshot {
    /// List direct child namespaces under `parent` with snapshot-bound cursor
    /// pagination. `None` denotes the root namespace.
    pub fn list_namespaces(
        &self,
        parent: Option<&NamespacePath>,
        page_token: Option<&str>,
        limit: Option<usize>,
    ) -> Result<CatalogPage<CatalogNamespace>> {
        let parent_components = parent.map(NamespacePath::components).unwrap_or_default();
        let mut children = BTreeMap::<NamespacePath, CatalogNamespace>::new();
        for dataset in &self.datasets {
            let components = dataset.mount.namespace.components();
            if components.len() <= parent_components.len()
                || !components.starts_with(parent_components)
            {
                continue;
            }
            let path = NamespacePath::new(components[..=parent_components.len()].iter().cloned())?;
            let mounted = components.len() == parent_components.len() + 1;
            children
                .entry(path.clone())
                .and_modify(|child| {
                    if mounted {
                        child.mounted = true;
                        child.sql_alias = Some(dataset.mount.name.clone());
                    }
                })
                .or_insert_with(|| CatalogNamespace {
                    path,
                    mounted,
                    sql_alias: mounted.then(|| dataset.mount.name.clone()),
                });
        }
        self.paginate(children.into_values().collect(), page_token, limit)
    }

    /// List frozen Sources in one mounted namespace without opening them.
    pub fn list_sources(
        &self,
        namespace: &NamespacePath,
        page_token: Option<&str>,
        limit: Option<usize>,
    ) -> Result<CatalogPage<CatalogSourceDescription>> {
        let dataset = self
            .datasets
            .iter()
            .find(|dataset| &dataset.mount.namespace == namespace)
            .with_context(|| format!("namespace '{}' is not mounted", namespace.display_name()))?;
        let items = dataset
            .sources
            .iter()
            .cloned()
            .map(|source| CatalogSourceDescription {
                namespace: namespace.clone(),
                sql_alias: dataset.mount.name.clone(),
                source,
            })
            .collect();
        self.paginate(items, page_token, limit)
    }

    /// Describe one frozen Source by stable namespace and mount-relative key.
    pub fn describe_source(
        &self,
        namespace: &NamespacePath,
        source: &str,
    ) -> Result<Option<CatalogSourceDescription>> {
        let dataset = self
            .datasets
            .iter()
            .find(|dataset| &dataset.mount.namespace == namespace)
            .with_context(|| format!("namespace '{}' is not mounted", namespace.display_name()))?;
        Ok(dataset
            .sources
            .iter()
            .find(|candidate| candidate.file == source)
            .cloned()
            .map(|source| CatalogSourceDescription {
                namespace: namespace.clone(),
                sql_alias: dataset.mount.name.clone(),
                source,
            }))
    }

    fn paginate<T>(
        &self,
        items: Vec<T>,
        page_token: Option<&str>,
        limit: Option<usize>,
    ) -> Result<CatalogPage<T>> {
        let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT);
        anyhow::ensure!(limit > 0, "catalog page limit must be positive");
        anyhow::ensure!(
            limit <= MAX_PAGE_LIMIT,
            "catalog page limit exceeds {MAX_PAGE_LIMIT}"
        );
        let offset = page_token
            .map(|token| parse_page_token(self.snapshot_id(), token))
            .transpose()?
            .unwrap_or(0);
        anyhow::ensure!(offset <= items.len(), "catalog page token is out of range");
        let end = offset.saturating_add(limit).min(items.len());
        let next_page_token = (end < items.len()).then(|| page_token_for(self.snapshot_id(), end));
        Ok(CatalogPage {
            items: items.into_iter().skip(offset).take(limit).collect(),
            next_page_token,
        })
    }
}

fn page_token_for(snapshot_id: &str, offset: usize) -> String {
    format!("{snapshot_id}:{offset}")
}

fn parse_page_token(snapshot_id: &str, token: &str) -> Result<usize> {
    let (token_snapshot, offset) = token
        .rsplit_once(':')
        .context("invalid catalog page token")?;
    anyhow::ensure!(
        token_snapshot == snapshot_id,
        "catalog page token belongs to another snapshot"
    );
    offset.parse().context("invalid catalog page token offset")
}
