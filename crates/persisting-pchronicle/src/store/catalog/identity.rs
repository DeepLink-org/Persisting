use anyhow::Result;
use serde::Serialize;

use crate::ChronicleFormat;

use super::DEFAULT_DATASET_NAME;

/// Stable hierarchical name for a mounted trajectory namespace.
///
/// Namespace identity is deliberately independent from the DataFusion schema
/// alias used to query it. A namespace component follows the portable Lance
/// Namespace character set, while the SQL alias remains a valid unquoted SQL
/// identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct NamespacePath(Vec<String>);

impl NamespacePath {
    pub fn new(components: impl IntoIterator<Item = impl Into<String>>) -> Result<Self> {
        let components = components
            .into_iter()
            .map(Into::into)
            .collect::<Vec<String>>();
        anyhow::ensure!(!components.is_empty(), "namespace path must not be empty");
        for component in &components {
            validate_namespace_component(component)?;
        }
        Ok(Self(components))
    }

    pub fn single(component: impl Into<String>) -> Result<Self> {
        Self::new([component.into()])
    }

    pub fn components(&self) -> &[String] {
        &self.0
    }

    pub fn display_name(&self) -> String {
        self.0.join("/")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DatasetMount {
    /// DataFusion schema alias. This is not the stable namespace identity.
    pub name: String,
    pub uri: String,
    pub namespace: NamespacePath,
    #[serde(skip)]
    pub(super) format_hint: Option<ChronicleFormat>,
}

impl DatasetMount {
    /// Mount one single-component namespace using a derived SQL alias.
    pub fn new(name: impl Into<String>, uri: impl Into<String>) -> Result<Self> {
        let namespace_name = name.into();
        let namespace = NamespacePath::single(namespace_name.clone())?;
        let sql_alias = normalize_sql_alias(&namespace_name)?;
        Self::namespaced(namespace, sql_alias, uri)
    }

    /// Mount an explicit namespace under a separately chosen SQL schema alias.
    pub fn namespaced(
        namespace: NamespacePath,
        sql_alias: impl Into<String>,
        uri: impl Into<String>,
    ) -> Result<Self> {
        let name = normalize_sql_alias(&sql_alias.into())?;
        let uri = uri.into();
        anyhow::ensure!(!uri.trim().is_empty(), "dataset URI must not be empty");
        Ok(Self {
            name,
            uri,
            namespace,
            format_hint: None,
        })
    }

    pub fn default(uri: impl Into<String>) -> Result<Self> {
        Self::new(DEFAULT_DATASET_NAME, uri)
    }

    pub fn with_format_hint(mut self, format: ChronicleFormat) -> Self {
        self.format_hint = Some(format);
        self
    }
}

/// Typed revision descriptor for a frozen source.
///
/// `snapshot_ref` remains a SQL/display projection of this value; internal
/// consistency and snapshot hashing use the typed form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CatalogSourceRevision {
    Storyline {
        generation: String,
    },
    Events {
        fact_version: u64,
        fact_rows: u64,
        layout_revision: u64,
    },
    LocalFile {
        fingerprint: String,
    },
    Object {
        version: Option<String>,
        etag: Option<String>,
        size_bytes: u64,
        last_modified: String,
        location: String,
    },
}

impl CatalogSourceRevision {
    pub fn snapshot_ref(&self) -> String {
        match self {
            Self::Storyline { generation } => generation.clone(),
            Self::Events {
                layout_revision, ..
            } => format!("manifest-revision:{layout_revision}"),
            Self::LocalFile { fingerprint } => fingerprint.clone(),
            Self::Object {
                version: Some(version),
                ..
            } => format!("version:{version}"),
            Self::Object {
                etag: Some(etag), ..
            } => format!("etag:{etag}"),
            Self::Object {
                size_bytes,
                last_modified,
                location,
                ..
            } => format!("object:{size_bytes}:{last_modified}:{location}"),
        }
    }
}

pub(super) fn normalize_sql_alias(name: &str) -> Result<String> {
    let name = name.trim().to_ascii_lowercase();
    let mut characters = name.chars();
    let valid_start = characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    let valid_rest =
        characters.all(|character| character == '_' || character.is_ascii_alphanumeric());
    anyhow::ensure!(
        valid_start && valid_rest,
        "Dataset SQL alias '{name}' must match [A-Za-z_][A-Za-z0-9_]*"
    );
    anyhow::ensure!(
        !matches!(name.as_str(), "public" | "information_schema"),
        "Dataset SQL alias '{name}' is reserved"
    );
    Ok(name)
}

fn validate_namespace_component(component: &str) -> Result<()> {
    anyhow::ensure!(
        !component.is_empty(),
        "namespace component must not be empty"
    );
    anyhow::ensure!(
        component.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        }),
        "namespace component '{component}' contains unsupported characters"
    );
    Ok(())
}
