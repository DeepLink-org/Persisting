use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use persisting_pchronicle::storage::{DatasetLocation, DatasetMount};
use serde::{Deserialize, Serialize};
use url::Url;

use super::problem::ApiError;

pub(crate) const ACCESS_KEY_HEADER: &str = "x-pchronicle-access-key";
pub(crate) const SECRET_KEY_HEADER: &str = "x-pchronicle-secret-key";
const MAX_CATALOG_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CatalogLibrary {
    pub name: String,
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_key: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CatalogUser {
    pub name: String,
    secret_key: String,
    datasets: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CatalogAcl {
    libraries: BTreeMap<String, CatalogLibrary>,
    users_by_access_key: HashMap<String, CatalogUser>,
    public_datasets: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogFile {
    #[serde(default)]
    meta: Option<CatalogMeta>,
    #[serde(default)]
    users: BTreeMap<String, CatalogUserFile>,
    #[serde(default)]
    datasets: BTreeMap<String, CatalogLibraryFile>,
    #[serde(default)]
    grants: Vec<CatalogGrantFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogMeta {
    version: u32,
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogGrantFile {
    user: String,
    dataset: String,
    #[serde(default)]
    permissions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogLibraryFile {
    uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    access_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogUserFile {
    access_key: String,
    secret_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct IssuedUser {
    pub name: String,
    pub access_key: String,
    pub secret_key: String,
}

impl CatalogAcl {
    pub(crate) fn load(path: &Path) -> Result<Self> {
        Self::parse(&read_catalog_config(path)?)
    }

    pub(crate) fn parse(content: &str) -> Result<Self> {
        let file = parse_catalog_file(content)?;
        Self::from_document(file)
    }

    fn from_document(file: CatalogFile) -> Result<Self> {
        let libraries = build_libraries(&file)?;
        let public_datasets = file
            .grants
            .iter()
            .filter(|grant| grant.user == "*")
            .map(|grant| grant.dataset.clone())
            .collect();
        Ok(Self {
            users_by_access_key: build_users(&file, &libraries)?,
            libraries,
            public_datasets,
        })
    }

    #[cfg(test)]
    pub(crate) fn mounts(&self) -> Result<Vec<DatasetMount>> {
        self.libraries
            .values()
            .map(|library| DatasetMount::new(&library.name, &library.uri))
            .collect()
    }

    pub(crate) fn authenticate(&self, access_key: &str, secret_key: &str) -> Option<&CatalogUser> {
        let user = self.users_by_access_key.get(access_key)?;
        if !secret_keys_match(&user.secret_key, secret_key) {
            return None;
        }
        Some(user)
    }

    fn authenticate_headers(
        &self,
        headers: &axum::http::HeaderMap,
    ) -> std::result::Result<&CatalogUser, ApiError> {
        let (access_key, secret_key) =
            credentials_from_headers(headers).ok_or_else(catalog_unauthorized)?;
        self.authenticate(&access_key, &secret_key)
            .ok_or_else(catalog_unauthorized)
    }

    pub(crate) fn public_for_all(&self) -> Vec<CatalogLibraryPublic> {
        self.public_datasets
            .iter()
            .filter_map(|name| self.libraries.get(name))
            .map(CatalogLibraryPublic::from)
            .collect()
    }

    pub(crate) fn libraries_for_public(&self) -> Vec<CatalogLibrary> {
        self.public_datasets
            .iter()
            .filter_map(|name| self.libraries.get(name))
            .cloned()
            .collect()
    }

    fn visible_for_headers(
        &self,
        headers: &axum::http::HeaderMap,
    ) -> Result<Vec<CatalogLibraryPublic>, ApiError> {
        match credentials_from_headers(headers) {
            Some((access, secret)) => self
                .authenticate(&access, &secret)
                .map(|user| self.list_for(user))
                .ok_or_else(catalog_unauthorized),
            None if !headers.contains_key(ACCESS_KEY_HEADER)
                && !headers.contains_key(SECRET_KEY_HEADER) =>
            {
                Ok(self.public_for_all())
            }
            None => Err(catalog_unauthorized()),
        }
    }

    fn credentials_for_public(&self, dataset: &str) -> Option<(&str, &str)> {
        self.users_by_access_key
            .iter()
            .find_map(|(access_key, user)| {
                user.datasets
                    .iter()
                    .any(|name| name == dataset)
                    .then_some((access_key.as_str(), user.secret_key.as_str()))
            })
    }

    pub(crate) fn list_for(&self, user: &CatalogUser) -> Vec<CatalogLibraryPublic> {
        user.datasets
            .iter()
            .filter_map(|name| self.libraries.get(name))
            .map(CatalogLibraryPublic::from)
            .collect()
    }

    pub(crate) fn ticket_for(&self, user: &CatalogUser, name: &str) -> Option<CatalogLibrary> {
        if !user.datasets.iter().any(|dataset| dataset == name) {
            return None;
        }
        self.libraries.get(name).cloned()
    }
}

pub(crate) fn issue_user(path: &Path, name: &str) -> Result<IssuedUser> {
    let name = canonical_user_name(name)?;
    let mut file = load_editable_catalog(path)?;
    anyhow::ensure!(
        !file.users.contains_key(&name),
        "catalog user '{name}' already exists"
    );
    let existing_keys: HashSet<String> = file
        .users
        .values()
        .map(|user| user.access_key.trim().to_owned())
        .collect();
    let access_key = unique_access_key(&existing_keys);
    let secret_key = generate_secret_key();
    file.users.insert(
        name.clone(),
        CatalogUserFile {
            access_key: access_key.clone(),
            secret_key: secret_key.clone(),
        },
    );
    write_catalog_file(path, &file)?;
    Ok(IssuedUser {
        name,
        access_key,
        secret_key,
    })
}

pub(crate) fn grant_datasets(path: &Path, name: &str, datasets: &[String]) -> Result<Vec<String>> {
    let mut file = load_editable_catalog(path)?;
    let library_names = canonical_library_names(&file)?;
    let users: Vec<String> = if name == "*" {
        file.users.keys().cloned().collect()
    } else {
        vec![canonical_user_name(name)?]
    };
    for user in &users {
        anyhow::ensure!(file.users.contains_key(user), "unknown user '{user}'");
    }
    for dataset in datasets {
        let dataset = granted_library_name(&library_names, dataset)?;
        for user in &users {
            if !file
                .grants
                .iter()
                .any(|grant| grant.user == *user && grant.dataset == dataset)
            {
                file.grants.push(CatalogGrantFile {
                    user: user.clone(),
                    dataset: dataset.clone(),
                    permissions: vec!["read".into(), "query".into(), "analyze".into()],
                });
            }
        }
    }
    let mut seen = HashSet::new();
    let granted = file
        .grants
        .iter()
        .filter(|grant| users.iter().any(|user| user == &grant.user))
        .map(|grant| grant.dataset.clone())
        .filter(|dataset| seen.insert(dataset.clone()))
        .collect();
    write_catalog_file(path, &file)?;
    Ok(granted)
}

pub(crate) fn revoke_datasets(path: &Path, name: &str, datasets: &[String]) -> Result<Vec<String>> {
    let mut file = load_editable_catalog(path)?;
    let users: Vec<String> = if name == "*" {
        file.users.keys().cloned().collect()
    } else {
        vec![canonical_user_name(name)?]
    };
    for user in &users {
        anyhow::ensure!(file.users.contains_key(user), "unknown user '{user}'");
    }
    let to_remove = datasets
        .iter()
        .map(|dataset| DatasetMount::new(dataset, "validation").map(|mount| mount.name))
        .collect::<Result<Vec<_>>>()?;
    for dataset in &to_remove {
        anyhow::ensure!(
            file.grants
                .iter()
                .any(|grant| users.iter().any(|user| user == &grant.user)
                    && grant.dataset == *dataset),
            "catalog user '{name}' does not grant '{dataset}'"
        );
    }
    file.grants.retain(|grant| {
        !(users.iter().any(|user| user == &grant.user)
            && to_remove.iter().any(|dataset| dataset == &grant.dataset))
    });
    let mut seen = HashSet::new();
    let remaining = file
        .grants
        .iter()
        .filter(|grant| users.iter().any(|user| user == &grant.user))
        .map(|grant| grant.dataset.clone())
        .filter(|dataset| seen.insert(dataset.clone()))
        .collect();
    write_catalog_file(path, &file)?;
    Ok(remaining)
}

#[derive(Debug, Clone)]
pub(crate) struct DatasetAddSpec {
    pub name: String,
    pub uri: String,
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub access_key: Option<String>,
    pub secret_key: Option<String>,
}

pub(crate) fn add_dataset(path: &Path, spec: DatasetAddSpec) -> Result<CatalogLibraryPublic> {
    let name = DatasetMount::new(&spec.name, "validation")
        .with_context(|| format!("catalog dataset name '{}'", spec.name))?
        .name;
    anyhow::ensure!(
        name == spec.name,
        "catalog dataset '{}' must match [A-Za-z_][A-Za-z0-9_]* in lowercase",
        spec.name
    );
    let mut file = load_editable_catalog(path)?;
    anyhow::ensure!(
        !file.datasets.contains_key(&name),
        "catalog dataset '{name}' already exists"
    );
    let location = DatasetLocation::parse(&spec.uri)
        .with_context(|| format!("catalog dataset '{name}' URI"))?;
    let uri = location.as_str().to_owned();
    let is_s3 = uri.starts_with("s3://");
    let access_key = spec
        .access_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let secret_key = spec
        .secret_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    match (access_key.as_ref(), secret_key.as_ref(), is_s3) {
        (None, None, false) => {}
        (Some(_), Some(_), true) => {}
        (None, None, true) => anyhow::bail!(
            "catalog dataset '{name}' is s3:// and must set --access-key and --secret-key"
        ),
        (Some(_), Some(_), false) => {
            anyhow::bail!("catalog dataset '{name}' is not s3:// and must not set backend keys")
        }
        _ => anyhow::bail!("catalog dataset '{name}' must set both --access-key and --secret-key"),
    }
    if is_s3 {
        ensure_s3_credentials_match_existing(
            &file,
            access_key.as_deref(),
            secret_key.as_deref(),
            spec.endpoint.as_deref(),
            spec.region.as_deref(),
        )?;
    }
    file.datasets.insert(
        name.clone(),
        CatalogLibraryFile {
            uri: uri.clone(),
            endpoint: spec.endpoint.clone(),
            region: spec.region.clone(),
            access_key,
            secret_key,
        },
    );
    // Validate full document before persist.
    let _ = build_libraries(&file)?;
    write_catalog_file(path, &file)?;
    Ok(CatalogLibraryPublic {
        name,
        uri,
        endpoint: spec.endpoint,
        region: spec.region,
    })
}

pub(crate) fn remove_datasets(path: &Path, names: &[String]) -> Result<Vec<String>> {
    let mut file = load_editable_catalog(path)?;
    let library_names = canonical_library_names(&file)?;
    let mut to_remove = Vec::new();
    for name in names {
        let name = granted_library_name(&library_names, name)?;
        let still_granted = file
            .grants
            .iter()
            .filter(|grant| grant.dataset == name)
            .map(|grant| grant.user.as_str())
            .collect::<Vec<_>>();
        anyhow::ensure!(
            still_granted.is_empty(),
            "catalog dataset '{name}' is still granted to {}",
            still_granted.join(", ")
        );
        to_remove.push(name);
    }
    for name in &to_remove {
        file.datasets.remove(name);
    }
    anyhow::ensure!(
        !file.datasets.is_empty() || file.users.is_empty(),
        "catalog config needs at least one dataset while users remain"
    );
    if !file.datasets.is_empty() {
        let _ = build_libraries(&file)?;
    }
    write_catalog_file(path, &file)?;
    Ok(file.datasets.keys().cloned().collect())
}

pub(crate) fn list_datasets_config(path: &Path) -> Result<Vec<CatalogLibraryPublic>> {
    let file = load_editable_catalog(path)?;
    if file.datasets.is_empty() {
        return Ok(Vec::new());
    }
    let libraries = build_libraries(&file)?;
    Ok(libraries.values().map(CatalogLibraryPublic::from).collect())
}

fn ensure_s3_credentials_match_existing(
    file: &CatalogFile,
    access_key: Option<&str>,
    secret_key: Option<&str>,
    endpoint: Option<&str>,
    region: Option<&str>,
) -> Result<()> {
    for (name, library) in &file.datasets {
        let uri = DatasetLocation::parse(&library.uri)
            .with_context(|| format!("catalog library '{name}' URI"))?
            .as_str()
            .to_owned();
        if !uri.starts_with("s3://") {
            continue;
        }
        anyhow::ensure!(
            library.access_key.as_deref().map(str::trim) == access_key
                && library.secret_key.as_deref().map(str::trim) == secret_key
                && library.endpoint.as_deref() == endpoint
                && library.region.as_deref() == region,
            "catalog s3 datasets must share the same endpoint, region, and backend keys (differs from '{name}')"
        );
    }
    Ok(())
}

fn read_catalog_config(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok("[meta]\nversion = 1\nrevision = 0\n".to_owned());
    }
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("read catalog config metadata {}", path.display()))?;
    anyhow::ensure!(metadata.is_file(), "catalog config must be a regular file");
    anyhow::ensure!(
        metadata.len() <= MAX_CATALOG_CONFIG_BYTES,
        "catalog config exceeds the {MAX_CATALOG_CONFIG_BYTES} byte limit"
    );
    std::fs::read_to_string(path).with_context(|| format!("read catalog config {}", path.display()))
}

fn parse_catalog_file(content: &str) -> Result<CatalogFile> {
    toml::from_str(content).context("parse catalog config")
}

fn load_editable_catalog(path: &Path) -> Result<CatalogFile> {
    let file = parse_catalog_file(&read_catalog_config(path)?)?;
    if file.datasets.is_empty() && file.users.is_empty() && file.grants.is_empty() {
        return Ok(file);
    }
    let libraries = build_libraries(&file)?;
    build_users(&file, &libraries)?;
    Ok(file)
}

fn write_catalog_file(path: &Path, file: &CatalogFile) -> Result<()> {
    let serialized = toml::to_string_pretty(file).context("serialize catalog config")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create catalog config directory {}", parent.display()))?;
    }
    let tmp_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("catalog.toml");
    let tmp = path.with_file_name(format!(".{tmp_name}.tmp"));
    std::fs::write(&tmp, serialized.as_bytes())
        .with_context(|| format!("write catalog config {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("persist catalog config {}", path.display()))?;
    Ok(())
}

fn build_libraries(file: &CatalogFile) -> Result<BTreeMap<String, CatalogLibrary>> {
    anyhow::ensure!(
        !file.datasets.is_empty(),
        "catalog config needs at least one dataset"
    );
    let mut libraries = BTreeMap::new();
    for (name, library) in &file.datasets {
        let mount = DatasetMount::new(name, "validation")
            .with_context(|| format!("catalog library name '{name}'"))?;
        let location = DatasetLocation::parse(&library.uri)
            .with_context(|| format!("catalog library '{name}' URI"))?;
        let uri = location.as_str().to_owned();
        let is_s3 = uri.starts_with("s3://");
        match (library.access_key.as_deref(), library.secret_key.as_deref()) {
            (None, None) => anyhow::ensure!(
                !is_s3,
                "catalog library '{name}' is s3:// and must set access_key and secret_key"
            ),
            (Some(access_key), Some(secret_key)) => {
                anyhow::ensure!(
                    is_s3,
                    "catalog library '{name}' is not s3:// and must not set backend keys"
                );
                let access_key = access_key.trim();
                let secret_key = secret_key.trim();
                anyhow::ensure!(
                    !access_key.is_empty(),
                    "catalog library '{name}' access_key is empty"
                );
                anyhow::ensure!(
                    !secret_key.is_empty(),
                    "catalog library '{name}' secret_key is empty"
                );
            }
            _ => anyhow::bail!("catalog library '{name}' must set both access_key and secret_key"),
        }
        libraries.insert(
            mount.name.clone(),
            CatalogLibrary {
                name: mount.name,
                uri,
                endpoint: library.endpoint.clone(),
                region: library.region.clone(),
                access_key: library
                    .access_key
                    .as_deref()
                    .map(|value| value.trim().to_owned()),
                secret_key: library
                    .secret_key
                    .as_deref()
                    .map(|value| value.trim().to_owned()),
            },
        );
    }
    Ok(libraries)
}

fn build_users(
    file: &CatalogFile,
    libraries: &BTreeMap<String, CatalogLibrary>,
) -> Result<HashMap<String, CatalogUser>> {
    let mut users_by_access_key = HashMap::new();
    let mut datasets_by_user: HashMap<String, Vec<String>> = HashMap::new();
    for grant in &file.grants {
        let grant_users: Vec<&str> = if grant.user == "*" {
            file.users.keys().map(String::as_str).collect()
        } else {
            vec![grant.user.as_str()]
        };
        for grant_user in grant_users {
            anyhow::ensure!(
                file.users.contains_key(grant_user),
                "catalog grant references unknown user '{}'",
                grant_user
            );
            anyhow::ensure!(
                libraries.contains_key(&grant.dataset),
                "catalog grant references unknown dataset '{}'",
                grant.dataset
            );
            let entry = datasets_by_user.entry(grant_user.to_owned()).or_default();
            anyhow::ensure!(
                !entry.contains(&grant.dataset),
                "catalog grant for user '{}' and dataset '{}' is duplicated",
                grant_user,
                grant.dataset
            );
            entry.push(grant.dataset.clone());
        }
    }
    for (name, user) in &file.users {
        let access_key = user.access_key.trim().to_owned();
        let secret_key = user.secret_key.trim().to_owned();
        anyhow::ensure!(
            !access_key.is_empty(),
            "catalog user '{name}' access_key is empty"
        );
        anyhow::ensure!(
            !secret_key.is_empty(),
            "catalog user '{name}' secret_key is empty"
        );
        let catalog_user = CatalogUser {
            name: name.clone(),
            secret_key,
            datasets: datasets_by_user.remove(name).unwrap_or_default(),
        };
        anyhow::ensure!(
            users_by_access_key
                .insert(access_key, catalog_user)
                .is_none(),
            "catalog user access keys must be unique"
        );
    }
    Ok(users_by_access_key)
}

fn canonical_user_name(name: &str) -> Result<String> {
    let mount = DatasetMount::new(name, "validation")
        .with_context(|| format!("catalog user name '{name}'"))?;
    anyhow::ensure!(
        mount.name == name,
        "catalog user '{name}' must match [A-Za-z_][A-Za-z0-9_]* in lowercase"
    );
    Ok(mount.name)
}

fn canonical_library_names(file: &CatalogFile) -> Result<BTreeSet<String>> {
    file.datasets
        .keys()
        .map(|name| {
            DatasetMount::new(name, "validation")
                .map(|mount| mount.name)
                .with_context(|| format!("catalog dataset name '{name}'"))
        })
        .collect()
}

fn granted_library_name(library_names: &BTreeSet<String>, dataset: &str) -> Result<String> {
    let mount = DatasetMount::new(dataset, "validation")
        .with_context(|| format!("catalog dataset name '{dataset}'"))?;
    anyhow::ensure!(
        library_names.contains(&mount.name),
        "unknown dataset '{dataset}'"
    );
    Ok(mount.name)
}

fn unique_access_key(existing: &HashSet<String>) -> String {
    loop {
        let access_key = generate_access_key();
        if !existing.contains(&access_key) {
            return access_key;
        }
    }
}

fn generate_access_key() -> String {
    format!("pcak_{}", encode_hex(&random_bytes(24)))
}

fn generate_secret_key() -> String {
    encode_hex(&random_bytes(32))
}

fn random_bytes(count: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(count);
    while bytes.len() < count {
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    bytes.truncate(count);
    bytes
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CatalogLibraryPublic {
    pub name: String,
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
}

impl From<&CatalogLibrary> for CatalogLibraryPublic {
    fn from(library: &CatalogLibrary) -> Self {
        Self {
            name: library.name.clone(),
            uri: library.uri.clone(),
            endpoint: library.endpoint.clone(),
            region: library.region.clone(),
        }
    }
}

pub(crate) fn apply_library_env(library: &CatalogLibrary) {
    if let Some(endpoint) = library.endpoint.as_deref() {
        unsafe {
            std::env::set_var("AWS_ENDPOINT", endpoint);
            std::env::set_var("AWS_ENDPOINT_URL_S3", endpoint);
            if endpoint.starts_with("http://") {
                std::env::set_var("AWS_ALLOW_HTTP", "true");
            }
        }
    }
    if let Some(region) = library
        .region
        .as_deref()
        .or_else(|| library.uri.starts_with("s3://").then_some("us-west-2"))
    {
        unsafe {
            std::env::set_var("AWS_REGION", region);
            std::env::set_var("AWS_DEFAULT_REGION", region);
        }
    }
    if let (Some(access_key), Some(secret_key)) =
        (library.access_key.as_deref(), library.secret_key.as_deref())
    {
        unsafe {
            std::env::set_var("AWS_ACCESS_KEY_ID", access_key);
            std::env::set_var("AWS_SECRET_ACCESS_KEY", secret_key);
        }
    }
}

pub(super) fn catalog_unauthorized() -> ApiError {
    ApiError::unauthorized("catalog credentials are invalid")
}

pub(crate) fn parse_catalog_pin_target(input: &str) -> Result<String> {
    let input = input.trim();
    let url = Url::parse(input).context("parse catalog pin URL")?;
    anyhow::ensure!(
        url.scheme() == "catalog",
        "catalog pin target must use catalog://"
    );
    anyhow::ensure!(
        url.username().is_empty() && url.password().is_none(),
        "catalog pin URL must not contain embedded credentials"
    );
    anyhow::ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "catalog pin URL must not contain a query string or fragment"
    );
    anyhow::ensure!(
        url.path() == "/" || url.path().is_empty(),
        "catalog pin URL must not contain a path"
    );
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("catalog pin URL must include a host"))?;
    let _: std::net::IpAddr = host
        .parse()
        .with_context(|| format!("catalog pin host '{host}' must be an IP address"))?;
    let port = url
        .port()
        .ok_or_else(|| anyhow!("catalog pin URL must include a port"))?;
    Ok(format!("catalog://{host}:{port}"))
}

pub(crate) fn catalog_http_base(catalog_url: &str) -> Result<String> {
    let normalized = parse_catalog_pin_target(catalog_url)?;
    Ok(normalized.replacen("catalog://", "http://", 1))
}

fn secret_keys_match(expected: &str, provided: &str) -> bool {
    let left = expected.as_bytes();
    let right = provided.as_bytes();
    if left.len() != right.len() {
        let mut acc = 0u8;
        for byte in left {
            acc |= *byte;
        }
        acc == 0 && false
    } else {
        left.iter()
            .zip(right)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }
}

pub(crate) fn credentials_from_headers(
    headers: &axum::http::HeaderMap,
) -> Option<(String, String)> {
    let access = headers
        .get(ACCESS_KEY_HEADER)?
        .to_str()
        .ok()?
        .trim()
        .to_owned();
    let secret = headers
        .get(SECRET_KEY_HEADER)?
        .to_str()
        .ok()?
        .trim()
        .to_owned();
    if access.is_empty() || secret.is_empty() {
        return None;
    }
    Some((access, secret))
}

fn parent_handles_path(path: &str) -> bool {
    let rest = path
        .strip_prefix("/api/v1")
        .or_else(|| path.strip_prefix("/api"))
        .unwrap_or(path);
    rest == "/health"
        || rest == "/ui"
        || rest == "/catalog/datasets"
        || rest.starts_with("/catalog/datasets/")
}

pub(super) async fn list_datasets(
    axum::extract::State(state): axum::extract::State<super::AppState>,
    headers: axum::http::HeaderMap,
) -> Result<axum::Json<Vec<CatalogLibraryPublic>>, ApiError> {
    let acl = state
        .catalog_acl
        .as_ref()
        .ok_or_else(|| ApiError::not_found("catalog is not enabled"))?;
    let libraries = acl.visible_for_headers(&headers)?;
    Ok(axum::Json(libraries))
}

pub(super) async fn get_dataset(
    axum::extract::State(state): axum::extract::State<super::AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<axum::Json<CatalogLibrary>, ApiError> {
    let acl = state
        .catalog_acl
        .as_ref()
        .ok_or_else(|| ApiError::not_found("catalog is not enabled"))?;
    let user = acl.authenticate_headers(&headers)?;
    let ticket = acl
        .ticket_for(user, &name)
        .ok_or_else(|| ApiError::not_found("dataset not found"))?;
    Ok(axum::Json(ticket))
}

pub(super) async fn catalog_data_plane_layer(
    axum::extract::State(state): axum::extract::State<super::AppState>,
    mut request: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    if state.catalog_query_worker || state.catalog_acl.is_none() {
        return next.run(request).await;
    }
    let path = request.uri().path().to_owned();
    if !path.starts_with("/api/") || parent_handles_path(&path) {
        return next.run(request).await;
    }
    // UI metadata is ACL-derived, not an all-dataset storage query. In
    // particular, authenticated callers may have incompatible S3 backends.
    let api_path = path
        .strip_prefix("/api/v1")
        .or_else(|| path.strip_prefix("/api"))
        .unwrap_or(&path);
    let ui_tables = api_path == "/query/tables"
        && url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
            .any(|(key, value)| key == "ui" && (value == "true" || value == "1"));
    let root_tree = api_path == "/explorer/tree"
        && !url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
            .any(|(key, value)| (key == "dataset" || key == "prefix") && !value.is_empty());
    if request.method() == axum::http::Method::GET && (ui_tables || root_tree) {
        let libraries = match state
            .catalog_acl
            .as_ref()
            .unwrap()
            .visible_for_headers(request.headers())
        {
            Ok(libraries) => libraries,
            Err(error) => return error.into_response(),
        };
        if root_tree {
            let mounts = libraries
                .iter()
                .filter_map(|library| DatasetMount::new(&library.name, &library.uri).ok())
                .collect::<Vec<_>>();
            return axum::Json(super::explorer::catalog_tree_from_mount_specs(&mounts))
                .into_response();
        }
        let catalog = super::QueryCatalog {
            snapshot_id: String::new(),
            read_only: true,
            database: String::new(),
            storage_path: String::new(),
            path_column: "_file_",
            datasets: libraries
                .into_iter()
                .map(|library| super::QueryDatasetSummary {
                    browse: None,
                    name: library.name,
                    uri: library.uri,
                    ready_sources: 0,
                    error_sources: 0,
                })
                .collect(),
            tables: super::query_table_summaries(),
        };
        return axum::Json(catalog).into_response();
    }
    if credentials_from_headers(request.headers()).is_none() && path.ends_with("/explorer/tree") {
        let params: BTreeMap<_, _> =
            url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
                .into_owned()
                .collect();
        let dataset = params
            .get("dataset")
            .map(String::as_str)
            .unwrap_or_default();
        tracing::info!(
            target: "pchronicle.serve",
            dataset,
            path,
            "catalog explorer request entering data-plane router"
        );
        let public = state.catalog_acl.as_ref().is_some_and(|acl| {
            dataset.is_empty()
                || acl
                    .public_for_all()
                    .iter()
                    .any(|library| library.name == dataset)
        });
        if !public {
            return dispatch_query_worker(&state, request)
                .await
                .unwrap_or_else(|error| error.into_response());
        }
        if !dataset.is_empty() {
            if let Some((access_key, secret_key)) = state
                .catalog_acl
                .as_ref()
                .and_then(|acl| acl.credentials_for_public(dataset))
            {
                let headers = request.headers_mut();
                if let (Ok(access_key), Ok(secret_key)) = (access_key.parse(), secret_key.parse()) {
                    headers.insert(ACCESS_KEY_HEADER, access_key);
                    headers.insert(SECRET_KEY_HEADER, secret_key);
                }
            }
            tracing::info!(
                target: "pchronicle.serve",
                dataset,
                "serving public catalog explorer request from front browse cache"
            );
            return next.run(request).await;
        }
        let mounts = state
            .catalog_acl
            .as_ref()
            .unwrap()
            .public_for_all()
            .into_iter()
            .filter_map(|library| DatasetMount::new(&library.name, &library.uri).ok())
            .collect::<Vec<_>>();
        return axum::Json(super::explorer::catalog_tree_from_mount_specs(&mounts)).into_response();
    }
    match dispatch_query_worker(&state, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn dispatch_query_worker(
    state: &super::AppState,
    request: axum::http::Request<axum::body::Body>,
) -> Result<axum::response::Response, ApiError> {
    use super::catalog_worker::{WorkerRequest, validate_backends};
    let acl = state
        .catalog_acl
        .as_ref()
        .ok_or_else(|| ApiError::not_found("catalog is not enabled"))?;
    let (access_key, secret_key) =
        credentials_from_headers(request.headers()).ok_or_else(catalog_unauthorized)?;
    let user = acl
        .authenticate(&access_key, &secret_key)
        .ok_or_else(catalog_unauthorized)?;
    let _permit = state.catalog_workers.admit()?;
    let selected = url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
        .filter(|(name, _)| name == "dataset")
        .map(|(_, value)| value.into_owned())
        .collect::<Vec<_>>();
    if selected.len() > 1 {
        return Err(ApiError::invalid_request("duplicate dataset parameter"));
    }
    let selected = selected.first().filter(|name| !name.is_empty());
    if let Some(name) = selected
        && acl.ticket_for(user, name).is_none()
    {
        return Err(ApiError::not_found("dataset not found"));
    }
    let mut mounts: Vec<CatalogLibrary> = user
        .datasets
        .iter()
        .filter_map(|name| acl.ticket_for(user, name))
        .collect();
    if mounts.is_empty() {
        return Err(ApiError::not_found("dataset not found"));
    }
    // Keep one stable worker for all of this user's compatible mounts. Only
    // split scopes when the backend's process-global credentials require it.
    if validate_backends(&mounts).is_err()
        && let Some(name) = selected
    {
        mounts.retain(|mount| &mount.name == name);
    }
    validate_backends(&mounts).map_err(|error| ApiError::invalid_request(error.to_string()))?;
    // Workers use a private working directory; retain the server's interpretation
    // of relative local mounts without opening any store in the parent.
    for mount in &mut mounts {
        if let Some(path) = DatasetLocation::parse(&mount.uri)
            .ok()
            .and_then(|l| l.local_path().map(std::path::Path::to_path_buf))
        {
            mount.uri = std::path::absolute(path)
                .map_err(|error| ApiError::internal("", "catalog_worker", error.into()))?
                .to_string_lossy()
                .into_owned();
        }
    }
    // Never persist credentials in filenames. Include grants and credential
    // epochs so revocation/rotation cannot reopen an older user's cache.
    let identity = serde_json::to_vec(&(
        &access_key,
        &user.name,
        &user.secret_key,
        &user.datasets,
        &mounts,
    ))
    .map_err(|error| ApiError::internal("", "catalog_worker", error.into()))?;
    let scope = blake3::hash(&identity).to_hex().to_string();
    let mut headers = request
        .headers()
        .iter()
        .filter(|(name, _)| matches!(name.as_str(), "content-type" | "accept"))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.to_string(), v.to_owned()))
        })
        .collect::<Vec<_>>();
    if let Some(id) = request.extensions().get::<super::request_log::RequestId>() {
        headers.push(("x-request-id".into(), id.0.clone()));
    }
    let method = request.method().to_string();
    let uri = request.uri().to_string();
    let body = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        axum::body::to_bytes(request.into_body(), 1024 * 1024),
    )
    .await
    .map_err(|_| ApiError::unavailable())?
    .map_err(|_| ApiError::invalid_request("catalog request body exceeds 1 MiB"))?
    .to_vec();
    state
        .catalog_workers
        .execute(
            scope,
            mounts,
            WorkerRequest {
                method,
                uri,
                headers,
                body,
            },
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[datasets.prod]
uri = "s3://bucket/prod"
endpoint = "http://127.0.0.1:9000"
region = "us-west-2"
access_key = "BACKEND_AK"
secret_key = "BACKEND_SK"

[datasets.evals]
uri = "s3://bucket/evals"
endpoint = "http://127.0.0.1:9000"
region = "us-west-2"
access_key = "BACKEND_AK"
secret_key = "BACKEND_SK"

[users.alice]
access_key = "USER_AK"
secret_key = "USER_SK"

[users.bob]
access_key = "BOB_AK"
secret_key = "BOB_SK"

[[grants]]
user = "alice"
dataset = "prod"
permissions = ["read", "query", "analyze"]

[[grants]]
user = "alice"
dataset = "evals"
permissions = ["read", "query", "analyze"]

[[grants]]
user = "bob"
dataset = "evals"
permissions = ["read", "query"]
"#;

    #[test]
    fn wildcard_grant_expands_to_all_users() {
        let acl = CatalogAcl::parse(
            r#"
[datasets.prod]
uri = "/tmp/prod"
[datasets.private]
uri = "/tmp/private"
[users.alice]
access_key = "a"
secret_key = "as"
[users.bob]
access_key = "b"
secret_key = "bs"
[[grants]]
user = "*"
dataset = "prod"
[[grants]]
user = "alice"
dataset = "private"
"#,
        )
        .unwrap();
        assert_eq!(acl.list_for(acl.authenticate("a", "as").unwrap()).len(), 2);
        assert_eq!(acl.list_for(acl.authenticate("b", "bs").unwrap()).len(), 1);
        assert_eq!(acl.public_for_all().len(), 1);
    }

    #[test]
    fn parse_rejects_unknown_grant() {
        let error = CatalogAcl::parse(
            r#"
[datasets.prod]
uri = "s3://bucket/prod"
access_key = "a"
secret_key = "b"
[users.alice]
access_key = "u"
secret_key = "s"

[[grants]]
user = "alice"
dataset = "missing"
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unknown dataset 'missing'"), "{error}");
    }

    #[test]
    fn parse_rejects_duplicate_user_keys() {
        let error = CatalogAcl::parse(
            r#"
[datasets.prod]
uri = "s3://bucket/prod"
access_key = "a"
secret_key = "b"
[users.alice]
access_key = "same"
secret_key = "s1"
[users.bob]
access_key = "same"
secret_key = "s2"
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unique"), "{error}");
    }

    #[test]
    fn parse_accepts_independent_s3_backend_keys() {
        let result = CatalogAcl::parse(
            r#"
[datasets.prod]
uri = "s3://bucket/prod"
access_key = "a"
secret_key = "b"
[datasets.evals]
uri = "s3://bucket/evals"
access_key = "c"
secret_key = "d"
[users.alice]
access_key = "u"
secret_key = "s"

[users.bob]
access_key = "bob-ak"
secret_key = "bob-sk"

[[grants]]
user = "alice"
dataset = "prod"
permissions = ["read", "query"]

[[grants]]
user = "alice"
dataset = "evals"
permissions = ["read", "query"]
"#,
        )
        .unwrap();
        let alice = result.authenticate("u", "s").unwrap();
        for (name, uri, access_key, secret_key) in [
            ("prod", "s3://bucket/prod", "a", "b"),
            ("evals", "s3://bucket/evals", "c", "d"),
        ] {
            let ticket = result.ticket_for(alice, name).unwrap();
            assert_eq!(ticket.name, name);
            assert_eq!(ticket.uri, uri);
            assert_eq!(ticket.access_key.as_deref(), Some(access_key));
            assert_eq!(ticket.secret_key.as_deref(), Some(secret_key));
        }

        let bob = result.authenticate("bob-ak", "bob-sk").unwrap();
        assert!(result.list_for(bob).is_empty());
        assert!(result.ticket_for(bob, "prod").is_none());
        assert!(result.ticket_for(bob, "evals").is_none());
    }

    #[test]
    fn authenticate_and_filter_datasets() {
        let acl = CatalogAcl::parse(SAMPLE).unwrap();
        assert!(acl.authenticate("USER_AK", "wrong").is_none());
        let alice = acl.authenticate("USER_AK", "USER_SK").unwrap();
        let listed: Vec<_> = acl
            .list_for(alice)
            .into_iter()
            .map(|library| library.name)
            .collect();
        assert_eq!(listed, vec!["prod", "evals"]);
        let ticket = acl.ticket_for(alice, "prod").unwrap();
        assert_eq!(ticket.secret_key.as_deref(), Some("BACKEND_SK"));
        assert!(acl.ticket_for(alice, "missing").is_none());
        let bob = acl.authenticate("BOB_AK", "BOB_SK").unwrap();
        assert!(acl.ticket_for(bob, "prod").is_none());
        assert_eq!(acl.list_for(bob)[0].name, "evals");
        assert!(
            acl.list_for(bob)
                .iter()
                .all(|library| library.endpoint.is_some())
        );
    }

    #[test]
    fn canonical_datasets_and_grants_format_is_accepted() {
        let acl = CatalogAcl::parse(
            r#"
[datasets.prod]
uri = "s3://bucket/prod"
access_key = "BACKEND_AK"
secret_key = "BACKEND_SK"

[users.alice]
access_key = "USER_AK"
secret_key = "USER_SK"

[[grants]]
user = "alice"
dataset = "prod"
permissions = ["read", "query"]
"#,
        )
        .unwrap();
        let user = acl.authenticate("USER_AK", "USER_SK").unwrap();
        assert_eq!(acl.list_for(user)[0].name, "prod");
        assert_eq!(
            acl.ticket_for(user, "prod").unwrap().secret_key.as_deref(),
            Some("BACKEND_SK")
        );
    }

    #[test]
    fn duplicate_canonical_grants_are_rejected() {
        let error = CatalogAcl::parse(
            r#"
[datasets.prod]
uri = "./prod"

[users.alice]
access_key = "USER_AK"
secret_key = "USER_SK"

[[grants]]
user = "alice"
dataset = "prod"

[[grants]]
user = "alice"
dataset = "prod"
"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("duplicated"));
    }

    #[test]
    fn public_list_omits_backend_secrets() {
        let acl = CatalogAcl::parse(SAMPLE).unwrap();
        let alice = acl.authenticate("USER_AK", "USER_SK").unwrap();
        let json = serde_json::to_string(&acl.list_for(alice)).unwrap();
        assert!(!json.contains("BACKEND_SK"));
        assert!(!json.contains("BACKEND_AK"));
    }

    #[test]
    fn catalog_pin_target_accepts_any_ip_with_port() {
        assert!(parse_catalog_pin_target("catalog://127.0.0.1:8081").is_ok());
        assert!(parse_catalog_pin_target("catalog://8.8.8.8:8081").is_ok());
        assert!(parse_catalog_pin_target("catalog://10.12.111.136:8000").is_ok());
        assert!(parse_catalog_pin_target("catalog://127.0.0.1").is_err());
        assert!(parse_catalog_pin_target("s3://bucket/prod").is_err());
    }

    #[test]
    fn parent_keeps_health_and_catalog_ticket_routes() {
        assert!(parent_handles_path("/api/health"));
        assert!(parent_handles_path("/api/v1/catalog/datasets"));
        assert!(parent_handles_path("/api/v1/catalog/datasets/prod"));
        assert!(!parent_handles_path("/api/catalog"));
        assert!(!parent_handles_path("/api/explorer/runs"));
        assert!(!parent_handles_path("/api/query/tables"));
    }

    async fn catalog_front() -> axum::Router {
        let acl = CatalogAcl::parse(SAMPLE).unwrap();
        crate::server::PreparedWarehouse::prepare_catalog_front(acl)
            .await
            .unwrap()
            .router()
    }

    #[tokio::test]
    async fn prepare_catalog_does_not_mount_datasets_in_parent() {
        let temporary = tempfile::tempdir().unwrap();
        let left = temporary.path().join("left");
        let right = temporary.path().join("right");
        std::fs::create_dir_all(&left).unwrap();
        std::fs::create_dir_all(&right).unwrap();
        let catalog = temporary.path().join("catalog.toml");
        std::fs::write(
            &catalog,
            format!(
                r#"
[datasets.left]
uri = "{}"

[datasets.right]
uri = "{}"
"#,
                left.display(),
                right.display()
            ),
        )
        .unwrap();

        let acl = CatalogAcl::load(&catalog).unwrap();
        let config = crate::server::ChronicleServerConfig::mounted(acl.mounts().unwrap()).unwrap();
        let warehouse = crate::server::PreparedWarehouse::prepare_catalog(acl, config)
            .await
            .unwrap();
        assert!(warehouse.dataset_names().is_empty());
        assert!(warehouse.state.catalog.read().await.is_none());
        use tower::ServiceExt;
        let response = warehouse
            .router()
            .oneshot(catalog_request("/api/explorer/runs", None, None))
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn prepare_catalog_keeps_public_mounts_for_browse_cache() {
        let temporary = tempfile::tempdir().unwrap();
        let dataset = temporary.path().join("shared");
        std::fs::create_dir_all(&dataset).unwrap();
        let catalog = temporary.path().join("catalog.toml");
        std::fs::write(
            &catalog,
            format!(
                "[datasets.shared]\nuri = \"{}\"\n\n[[grants]]\nuser = \"*\"\ndataset = \"shared\"\n",
                dataset.display()
            ),
        )
        .unwrap();

        let acl = CatalogAcl::load(&catalog).unwrap();
        let config = crate::server::ChronicleServerConfig::front_only();
        let warehouse = crate::server::PreparedWarehouse::prepare_catalog(acl, config)
            .await
            .unwrap();
        assert!(warehouse.state.config.datasets.is_empty());
        assert_eq!(warehouse.state.browse_mounts.len(), 1);
        assert_eq!(warehouse.state.browse_mounts[0].name, "shared");
    }

    #[tokio::test]
    async fn catalog_warehouse_exposes_home_links_on_ui_route() {
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let temporary = tempfile::tempdir().unwrap();
        let dataset = temporary.path().join("left");
        std::fs::create_dir_all(&dataset).unwrap();
        let catalog = temporary.path().join("catalog.toml");
        std::fs::write(
            &catalog,
            format!(
                r#"
[datasets.left]
uri = "{}"
"#,
                dataset.display()
            ),
        )
        .unwrap();

        let acl = CatalogAcl::load(&catalog).unwrap();
        let mut config =
            crate::server::ChronicleServerConfig::mounted(acl.mounts().unwrap()).unwrap();
        config.home_links = vec![crate::server::parse_home_link("Realtime=/litefuse").unwrap()];
        let warehouse = crate::server::PreparedWarehouse::prepare_catalog(acl, config)
            .await
            .unwrap();
        let response = warehouse
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/ui")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "links": [{"label": "Realtime", "href": "/litefuse"}]
            })
        );
    }

    async fn catalog_body(response: axum::response::Response) -> (axum::http::StatusCode, String) {
        use http_body_util::BodyExt;

        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    fn catalog_request(
        uri: &str,
        access_key: Option<&str>,
        secret_key: Option<&str>,
    ) -> axum::http::Request<axum::body::Body> {
        let mut builder = axum::http::Request::builder().uri(uri);
        if let Some(access_key) = access_key {
            builder = builder.header(ACCESS_KEY_HEADER, access_key);
        }
        if let Some(secret_key) = secret_key {
            builder = builder.header(SECRET_KEY_HEADER, secret_key);
        }
        builder.body(axum::body::Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn ui_metadata_handles_mixed_backends_without_mounting_workers() {
        use tower::ServiceExt;
        let acl =
            CatalogAcl::parse(&SAMPLE.replacen("127.0.0.1:9000", "127.0.0.1:9001", 1)).unwrap();
        assert!(
            super::super::catalog_worker::validate_backends(
                &acl.libraries.values().cloned().collect::<Vec<_>>()
            )
            .is_err()
        );
        let mut state = super::super::app_state(super::super::ChronicleServerConfig::front_only());
        state.catalog_acl = Some(std::sync::Arc::new(acl));
        let warehouse = super::super::PreparedWarehouse { state };
        let app = warehouse.router();
        for path in [
            "/api/query/tables?ui=true",
            "/api/v1/query/tables?ui=1",
            "/api/explorer/tree?dataset=&prefix=",
            "/api/v1/explorer/tree",
        ] {
            for (access, secret, expected) in [
                (Some("USER_AK"), Some("USER_SK"), 2),
                (Some("BOB_AK"), Some("BOB_SK"), 1),
                (None, None, 0),
            ] {
                let (status, body) = catalog_body(
                    app.clone()
                        .oneshot(catalog_request(path, access, secret))
                        .await
                        .unwrap(),
                )
                .await;
                assert_eq!(status, axum::http::StatusCode::OK, "{path}: {body}");
                let json: serde_json::Value = serde_json::from_str(&body).unwrap();
                let entries = json[if path.contains("query/tables") {
                    "datasets"
                } else {
                    "children"
                }]
                .as_array()
                .unwrap();
                assert_eq!(entries.len(), expected, "{body}");
                if expected == 1 {
                    assert_eq!(entries[0]["name"], "evals");
                }
                assert!(!body.contains("BACKEND_AK") && !body.contains("BACKEND_SK"));
            }
            for secret in [Some("wrong"), None] {
                let (status, _) = catalog_body(
                    app.clone()
                        .oneshot(catalog_request(path, Some("USER_AK"), secret))
                        .await
                        .unwrap(),
                )
                .await;
                assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
            }
        }
        assert!(warehouse.state.browse.get().is_none());
        assert!(warehouse.state.catalog.read().await.is_none());
    }

    #[tokio::test]
    async fn catalog_list_allows_anonymous_public_datasets_and_rejects_invalid_credentials() {
        use tower::ServiceExt;

        let app = catalog_front().await;
        let (status, _) = catalog_body(
            app.clone()
                .oneshot(catalog_request("/api/v1/catalog/datasets", None, None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);

        let (status, body) = catalog_body(
            app.clone()
                .oneshot(catalog_request(
                    "/api/v1/catalog/datasets",
                    Some("USER_AK"),
                    Some("wrong"),
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
        assert!(!body.contains("USER_SK"));
        assert!(!body.contains("BACKEND_SK"));

        let (status, body) = catalog_body(
            app.oneshot(catalog_request(
                "/api/v1/catalog/datasets",
                Some("USER_AK"),
                Some("USER_SK"),
            ))
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert!(body.contains("\"name\":\"prod\""));
        assert!(!body.contains("BACKEND_AK"));
        assert!(!body.contains("BACKEND_SK"));
        assert!(!body.contains("access_key"));
        assert!(!body.contains("secret_key"));
    }

    #[tokio::test]
    async fn catalog_ticket_hides_unauthorized_names_as_not_found() {
        use tower::ServiceExt;

        let app = catalog_front().await;
        let (status, body) = catalog_body(
            app.clone()
                .oneshot(catalog_request(
                    "/api/v1/catalog/datasets/prod",
                    Some("USER_AK"),
                    Some("USER_SK"),
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert!(body.contains("BACKEND_SK"));
        assert!(body.contains("BACKEND_AK"));

        let (status, _) = catalog_body(
            app.clone()
                .oneshot(catalog_request(
                    "/api/v1/catalog/datasets/prod",
                    Some("BOB_AK"),
                    Some("BOB_SK"),
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);

        let (status, _) = catalog_body(
            app.oneshot(catalog_request(
                "/api/v1/catalog/datasets/missing",
                Some("USER_AK"),
                Some("USER_SK"),
            ))
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn catalog_data_plane_requires_headers_without_spawning_worker() {
        use tower::ServiceExt;

        let app = catalog_front().await;
        let (status, _) = catalog_body(
            app.clone()
                .oneshot(catalog_request("/api/query/tables", None, None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);

        let (status, _) = catalog_body(
            app.oneshot(catalog_request("/api/health", None, None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
    }

    const LIBRARIES_ONLY: &str = r#"
[datasets.prod]
uri = "s3://bucket/prod"
endpoint = "http://127.0.0.1:9000"
region = "us-west-2"
access_key = "BACKEND_AK"
secret_key = "BACKEND_SK"

[datasets.evals]
uri = "s3://bucket/evals"
endpoint = "http://127.0.0.1:9000"
region = "us-west-2"
access_key = "BACKEND_AK"
secret_key = "BACKEND_SK"
"#;

    fn assert_issued_key_format(issued: &IssuedUser) {
        assert!(
            issued.access_key.starts_with("pcak_"),
            "{}",
            issued.access_key
        );
        let hex = &issued.access_key["pcak_".len()..];
        assert_eq!(hex.len(), 48, "{}", issued.access_key);
        assert!(
            hex.chars().all(|character| character.is_ascii_hexdigit()),
            "{}",
            issued.access_key
        );
        assert_eq!(issued.secret_key.len(), 64, "{}", issued.secret_key);
        assert!(
            issued
                .secret_key
                .chars()
                .all(|character| character.is_ascii_hexdigit()),
            "{}",
            issued.secret_key
        );
        assert_ne!(issued.access_key, issued.secret_key);
    }

    #[test]
    fn catalog_parent_does_not_install_backend_credentials() {
        use clap::Parser;

        let temporary = tempfile::tempdir().unwrap();
        let catalog = temporary.path().join("catalog.toml");
        std::fs::write(
            &catalog,
            r#"
[datasets.rfs]
uri = "s3://test/test"
endpoint = "http://127.0.0.1:9000"
region = "us-east-1"
access_key = "123"
secret_key = "123"
"#,
        )
        .unwrap();
        let catalog_arg = catalog.to_string_lossy().into_owned();
        let cli = crate::Cli::try_parse_from([
            "pchronicle",
            "serve",
            "--listen",
            "127.0.0.1:0",
            "--catalog-config",
            &catalog_arg,
        ])
        .unwrap();
        let keys = [
            "AWS_REGION",
            "AWS_DEFAULT_REGION",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_ENDPOINT_URL_S3",
        ];
        let before: Vec<_> = keys.iter().map(std::env::var_os).collect();
        crate::apply_catalog_backend_env_before_runtime(&cli).unwrap();
        let after: Vec<_> = keys.iter().map(std::env::var_os).collect();
        assert!(
            before == after,
            "parent must not mutate storage credentials"
        );
    }

    #[test]
    fn apply_library_env_exports_region_for_opendal() {
        let previous_region = std::env::var("AWS_REGION").ok();
        let previous_default = std::env::var("AWS_DEFAULT_REGION").ok();
        unsafe {
            std::env::remove_var("AWS_REGION");
            std::env::remove_var("AWS_DEFAULT_REGION");
        }
        apply_library_env(&CatalogLibrary {
            name: "rfs".into(),
            uri: "s3://test/test".into(),
            endpoint: Some("http://127.0.0.1:9000".into()),
            region: Some("us-east-1".into()),
            access_key: Some("123".into()),
            secret_key: Some("123".into()),
        });
        assert_eq!(std::env::var("AWS_REGION").unwrap(), "us-east-1");
        assert_eq!(std::env::var("AWS_DEFAULT_REGION").unwrap(), "us-east-1");
        assert_eq!(std::env::var("AWS_ACCESS_KEY_ID").unwrap(), "123");
        assert_eq!(
            std::env::var("AWS_ENDPOINT_URL_S3").unwrap(),
            "http://127.0.0.1:9000"
        );
        unsafe {
            match previous_region {
                Some(value) => std::env::set_var("AWS_REGION", value),
                None => std::env::remove_var("AWS_REGION"),
            }
            match previous_default {
                Some(value) => std::env::set_var("AWS_DEFAULT_REGION", value),
                None => std::env::remove_var("AWS_DEFAULT_REGION"),
            }
        }
    }

    #[test]
    fn parse_allows_catalog_without_users() {
        let acl = CatalogAcl::parse(LIBRARIES_ONLY).unwrap();
        assert_eq!(
            acl.mounts()
                .unwrap()
                .iter()
                .map(|mount| mount.name.as_str())
                .collect::<Vec<_>>(),
            vec!["evals", "prod"]
        );
    }

    #[test]
    fn add_and_remove_dataset_rewrites_catalog_without_touching_users() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("catalog.toml");
        std::fs::write(&path, LIBRARIES_ONLY).unwrap();

        let added = add_dataset(
            &path,
            DatasetAddSpec {
                name: "local".into(),
                uri: "/tmp/local-warehouse".into(),
                endpoint: None,
                region: None,
                access_key: None,
                secret_key: None,
            },
        )
        .unwrap();
        assert_eq!(added.name, "local");
        assert_eq!(added.uri, "/tmp/local-warehouse");

        let listed = list_datasets_config(&path).unwrap();
        assert!(listed.iter().any(|item| item.name == "local"));

        let remaining = remove_datasets(&path, &["local".into()]).unwrap();
        assert!(!remaining.iter().any(|name| name == "local"));
        assert!(remaining.contains(&"prod".to_string()));
    }

    #[test]
    fn remove_dataset_rejects_when_grants_still_reference_it() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("catalog.toml");
        std::fs::write(&path, SAMPLE).unwrap();
        let error = remove_datasets(&path, &["prod".into()])
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("still granted") || error.contains("grant"),
            "{error}"
        );
    }

    #[test]
    fn issue_bootstraps_first_user_without_grants() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("catalog.toml");
        std::fs::write(&path, LIBRARIES_ONLY).unwrap();

        let issued = issue_user(&path, "alice").unwrap();
        assert_eq!(issued.name, "alice");
        assert_issued_key_format(&issued);

        let acl = CatalogAcl::load(&path).unwrap();
        let user = acl
            .authenticate(&issued.access_key, &issued.secret_key)
            .unwrap();
        assert_eq!(user.name, "alice");
        assert!(acl.list_for(user).is_empty());
        let stored = path_text(&path);
        assert!(!stored.contains("datasets = ["), "{stored}");
        assert!(stored.contains(&issued.access_key), "{stored}");
        assert!(stored.contains(&issued.secret_key), "{stored}");
    }

    #[test]
    fn issue_rejects_existing_user() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("catalog.toml");
        std::fs::write(&path, SAMPLE).unwrap();
        let error = issue_user(&path, "alice").unwrap_err().to_string();
        assert!(error.contains("already exists"), "{error}");
        assert_eq!(path_text(&path), SAMPLE);
    }

    #[test]
    fn grant_is_additive_and_rejects_unknown_names() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("catalog.toml");
        std::fs::write(&path, LIBRARIES_ONLY).unwrap();
        issue_user(&path, "alice").unwrap();

        let missing_user = grant_datasets(&path, "bob", &["prod".into()])
            .unwrap_err()
            .to_string();
        assert!(
            missing_user.contains("unknown user 'bob'"),
            "{missing_user}"
        );

        let missing_library = grant_datasets(&path, "alice", &["missing".into()])
            .unwrap_err()
            .to_string();
        assert!(
            missing_library.contains("unknown dataset 'missing'"),
            "{missing_library}"
        );

        assert_eq!(
            grant_datasets(&path, "alice", &["prod".into()]).unwrap(),
            vec!["prod".to_owned()]
        );
        assert_eq!(
            grant_datasets(&path, "alice", &["prod".into(), "evals".into()]).unwrap(),
            vec!["prod".to_owned(), "evals".to_owned()]
        );

        let acl = CatalogAcl::load(&path).unwrap();
        let alice = acl
            .users_by_access_key
            .values()
            .find(|user| user.name == "alice")
            .unwrap();
        let listed: Vec<_> = acl
            .list_for(alice)
            .into_iter()
            .map(|library| library.name)
            .collect();
        assert_eq!(listed, vec!["prod", "evals"]);
    }

    #[test]
    fn revoke_removes_only_granted_datasets() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("catalog.toml");
        std::fs::write(&path, SAMPLE).unwrap();

        let missing_user = revoke_datasets(&path, "carol", &["prod".into()])
            .unwrap_err()
            .to_string();
        assert!(
            missing_user.contains("unknown user 'carol'"),
            "{missing_user}"
        );

        let missing_grant = revoke_datasets(&path, "bob", &["prod".into()])
            .unwrap_err()
            .to_string();
        assert!(
            missing_grant.contains("does not grant 'prod'"),
            "{missing_grant}"
        );

        assert_eq!(
            revoke_datasets(&path, "alice", &["prod".into()]).unwrap(),
            vec!["evals".to_owned()]
        );
        assert_eq!(
            revoke_datasets(&path, "alice", &["evals".into()]).unwrap(),
            Vec::<String>::new()
        );
    }

    fn path_text(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }
}
