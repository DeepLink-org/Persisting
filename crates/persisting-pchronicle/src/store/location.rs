//! Dataset URI facade: one parse/exists/put path for local and object stores.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use url::Url;

use super::opendal_store::Store as OpendalStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatasetLocationKind {
    Local,
    ObjectStore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetLocation {
    uri: String,
    kind: DatasetLocationKind,
    local_path: Option<PathBuf>,
}

impl DatasetLocation {
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        anyhow::ensure!(!input.is_empty(), "Dataset URI must not be empty");
        if !input.contains("://") {
            return Ok(Self::local(input.to_string(), PathBuf::from(input)));
        }

        let url = Url::parse(input).context("parse Dataset URI")?;
        anyhow::ensure!(
            url.username().is_empty() && url.password().is_none(),
            "Dataset URI must not contain embedded credentials"
        );
        anyhow::ensure!(
            url.query().is_none(),
            "Dataset URI must not contain a query string or signed credentials"
        );
        anyhow::ensure!(
            url.fragment().is_none(),
            "Dataset URI must not contain a fragment"
        );
        match url.scheme() {
            "s3" | "az" | "gs" | "memory" | "shared-memory" => {
                if let Some(port) = url.port() {
                    let endpoint_hint = if url.scheme() == "s3" {
                        "; S3-compatible endpoints must be configured separately with \
                         AWS_ENDPOINT_URL_S3 (or AWS_ENDPOINT), for example \
                         AWS_ENDPOINT_URL_S3=http://127.0.0.1:9000 and s3://bucket/prefix"
                    } else {
                        "; configure the object-store endpoint separately instead of putting a \
                         port in the Dataset URI"
                    };
                    return Err(anyhow!(
                        "{} Dataset URI must use the bucket as host without port {port}{endpoint_hint}",
                        url.scheme()
                    ));
                }
                let bucket = url
                    .host_str()
                    .ok_or_else(|| anyhow!("object-store URI must name a bucket"))?;
                validate_object_store_bucket(url.scheme(), bucket)?;
                Ok(Self {
                    uri: trim_trailing_slashes(input),
                    kind: DatasetLocationKind::ObjectStore,
                    local_path: None,
                })
            }
            "file" => {
                anyhow::ensure!(
                    url.host_str().is_none(),
                    "local Dataset URI must not contain a host"
                );
                let path = url
                    .to_file_path()
                    .map_err(|_| anyhow!("convert file Dataset URI to a local path"))?;
                Ok(Self {
                    uri: trim_trailing_slashes(input),
                    kind: DatasetLocationKind::Local,
                    local_path: Some(path),
                })
            }
            "local" => {
                anyhow::ensure!(
                    url.host_str().is_none(),
                    "local Dataset URI must not contain a host"
                );
                Ok(Self {
                    uri: trim_trailing_slashes(input),
                    kind: DatasetLocationKind::Local,
                    local_path: Some(PathBuf::from(url.path())),
                })
            }
            other => Err(anyhow!("unsupported Dataset URI scheme '{other}'")),
        }
    }

    fn local(uri: String, path: PathBuf) -> Self {
        Self {
            uri,
            kind: DatasetLocationKind::Local,
            local_path: Some(path),
        }
    }

    pub fn into_existing(self) -> Result<Self> {
        let Some(path) = self.local_path.clone() else {
            return Ok(self);
        };
        if self.uri.contains("://") {
            return Ok(self);
        }
        let canonical = std::fs::canonicalize(&path).context("canonicalize local Dataset path")?;
        Ok(Self::local(
            canonical.to_string_lossy().into_owned(),
            canonical,
        ))
    }

    pub fn into_create_target(self) -> Result<Self> {
        let Some(path) = self.local_path.clone() else {
            return Ok(self);
        };
        anyhow::ensure!(
            path.file_name().is_some(),
            "import output must name a new Dataset directory"
        );
        anyhow::ensure!(!path.exists(), "import output already exists");
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let parent =
            std::fs::canonicalize(parent).context("canonicalize import output parent directory")?;
        anyhow::ensure!(parent.is_dir(), "import output parent is not a directory");
        let filename = path
            .file_name()
            .context("import output must name a Dataset directory")?;
        let resolved = parent.join(filename);
        Ok(Self::local(
            resolved.to_string_lossy().into_owned(),
            resolved,
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.uri
    }

    pub fn kind(&self) -> DatasetLocationKind {
        self.kind
    }

    pub fn is_object_store(&self) -> bool {
        self.kind == DatasetLocationKind::ObjectStore
    }

    pub fn local_path(&self) -> Option<&Path> {
        self.local_path.as_deref()
    }

    pub async fn exists(&self) -> Result<bool> {
        if let Some(path) = &self.local_path {
            return Ok(path.exists());
        }
        let store = OpendalStore::from_uri(&self.uri).await?;
        store.exists().await
    }

    pub async fn put_bytes(&self, bytes: &[u8], overwrite: bool) -> Result<()> {
        if let Some(path) = &self.local_path {
            return put_local_bytes(path, bytes, overwrite);
        }
        let store = OpendalStore::from_uri(&self.uri).await?;
        // DatasetLocation represents a prefix; use a stable marker inside it.
        let path = ".dataset-marker";
        if overwrite {
            store.write_overwrite(path, bytes.to_vec()).await?;
        } else {
            store.write_create(path, bytes.to_vec()).await?;
        }
        Ok(())
    }

    /// Remove the complete Dataset represented by this local directory or
    /// object-store prefix.
    pub async fn remove_all(&self) -> Result<()> {
        if let Some(path) = &self.local_path {
            anyhow::ensure!(path.exists(), "Dataset does not exist: {}", self.uri);
            anyhow::ensure!(
                path.file_name().is_some(),
                "refusing to drop a filesystem root as a Dataset"
            );
            anyhow::ensure!(path.is_dir(), "Dataset is not a directory: {}", self.uri);
            std::fs::remove_dir_all(path)
                .with_context(|| format!("drop local Dataset {}", path.display()))?;
            return Ok(());
        }

        let url = Url::parse(&self.uri).context("parse Dataset URI for drop")?;
        anyhow::ensure!(
            !url.path().trim_matches('/').is_empty(),
            "refusing to drop an entire object-store bucket; name a Dataset prefix"
        );
        let store = OpendalStore::from_uri(&self.uri).await?;
        store.remove_all().await?;
        Ok(())
    }
}

fn validate_object_store_bucket(scheme: &str, bucket: &str) -> Result<()> {
    if matches!(scheme, "memory" | "shared-memory") {
        return Ok(());
    }
    anyhow::ensure!(
        (3..=63).contains(&bucket.len()),
        "{scheme} bucket name '{bucket}' is invalid; names must be 3-63 characters"
    );
    let charset_ok = bucket
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '.');
    let edges_ok = bucket
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphanumeric())
        && bucket
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_ascii_alphanumeric());
    anyhow::ensure!(
        charset_ok && edges_ok && !bucket.contains(".."),
        "{scheme} bucket name '{bucket}' is invalid; use lowercase letters, numbers, dots, and hyphens"
    );
    Ok(())
}

#[cfg(test)]
fn object_store_error_detail(error: &impl std::fmt::Display) -> String {
    let text = error.to_string();
    match xml_tag(&text, "Code") {
        Some(code) if !code.is_empty() => match xml_tag(&text, "Message") {
            Some(message) if !message.is_empty() && message != code => {
                format!("{code}: {message}")
            }
            _ => code.to_string(),
        },
        _ => text,
    }
}

#[cfg(test)]
fn xml_tag<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)?;
    Some(text[start..start + end].trim())
}

fn trim_trailing_slashes(input: &str) -> String {
    let minimum = input.find("://").map_or(1, |index| {
        index
            + if input.starts_with("local://") || input.starts_with("file://") {
                4
            } else {
                3
            }
    });
    let mut normalized = input.to_string();
    while normalized.len() > minimum && normalized.ends_with('/') {
        normalized.pop();
    }
    normalized
}

fn put_local_bytes(path: &Path, bytes: &[u8], overwrite: bool) -> Result<()> {
    let filename = path.file_name().context("export output must name a file")?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent =
        std::fs::canonicalize(parent).context("canonicalize export output parent directory")?;
    anyhow::ensure!(parent.is_dir(), "export output parent is not a directory");
    let output = parent.join(filename);
    if output.exists() {
        anyhow::ensure!(overwrite, "export output already exists; pass --overwrite");
        anyhow::ensure!(output.is_file(), "export output exists and is not a file");
    }
    let staging_path = parent.join(format!(
        ".pchronicle-object-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    {
        let mut staging = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staging_path)
            .context("create local object staging file")?;
        staging
            .write_all(bytes)
            .context("write local object staging file")?;
        staging
            .sync_all()
            .context("sync local object staging file")?;
    }
    let publish = if overwrite {
        std::fs::rename(&staging_path, &output).context("replace local object atomically")
    } else {
        publish_exclusive(&staging_path, &output)
    };
    if publish.is_err() {
        let _ = std::fs::remove_file(&staging_path);
    }
    publish?;
    File::open(&parent)
        .and_then(|directory| directory.sync_all())
        .context("sync local object parent directory")?;
    Ok(())
}

fn publish_exclusive(from: &Path, to: &Path) -> Result<()> {
    match OpenOptions::new().create_new(true).write(true).open(to) {
        Ok(mut file) => {
            let bytes = std::fs::read(from).context("read staged object")?;
            file.write_all(&bytes).context("write exclusive object")?;
            file.sync_all().context("sync exclusive object")?;
            let _ = std::fs::remove_file(from);
            Ok(())
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            anyhow::bail!("export output already exists; pass --overwrite")
        }
        Err(error) => Err(error).context("publish exclusive local object"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parse_strips_object_store_trailing_slashes() {
        let location = DatasetLocation::parse("s3://bucket/prefix/").unwrap();
        assert_eq!(location.as_str(), "s3://bucket/prefix");
        assert!(location.is_object_store());
    }

    #[test]
    fn parse_rejects_embedded_credentials() {
        let error = DatasetLocation::parse("s3://user:secret@bucket/prefix")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("must not contain embedded credentials"),
            "{error}"
        );
    }

    #[test]
    fn parse_rejects_short_s3_bucket_name() {
        let error = DatasetLocation::parse("s3://dd/data/")
            .unwrap_err()
            .to_string();
        assert!(error.contains("must be 3-63 characters"), "{error}");
        assert!(error.contains("'dd'"), "{error}");
    }

    #[test]
    fn parse_rejects_s3_endpoint_port_in_dataset_uri() {
        let error = DatasetLocation::parse("s3://127.0.0.1:9000/dd/test")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("must use the bucket as host without port 9000"),
            "{error}"
        );
        assert!(error.contains("AWS_ENDPOINT_URL_S3"), "{error}");
        assert!(error.contains("s3://bucket/prefix"), "{error}");
    }

    #[test]
    fn object_store_error_detail_extracts_s3_code() {
        let raw = "Generic S3 error: Server returned non-2xx status code: 400 Bad Request: \
            <?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>InvalidBucketName</Code></Error>";
        assert_eq!(object_store_error_detail(&raw), "InvalidBucketName");
    }

    #[test]
    fn create_target_requires_missing_local_child() {
        let temp = tempdir().unwrap();
        let output = temp.path().join("dataset");
        let location = DatasetLocation::parse(output.to_str().unwrap())
            .unwrap()
            .into_create_target()
            .unwrap();
        assert_eq!(
            location.local_path().unwrap(),
            std::fs::canonicalize(temp.path()).unwrap().join("dataset")
        );

        std::fs::create_dir(&output).unwrap();
        let error = DatasetLocation::parse(output.to_str().unwrap())
            .unwrap()
            .into_create_target()
            .unwrap_err()
            .to_string();
        assert!(error.contains("already exists"), "{error}");
    }

    #[tokio::test]
    async fn put_bytes_is_create_only_without_overwrite() {
        let temp = tempdir().unwrap();
        let output = temp.path().join("out.json");
        let location = DatasetLocation::parse(output.to_str().unwrap()).unwrap();
        location.put_bytes(b"one", false).await.unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"one");
        let error = location
            .put_bytes(b"two", false)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("already exists"), "{error}");
        location.put_bytes(b"two", true).await.unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"two");
    }

    #[tokio::test]
    async fn missing_shared_memory_prefix_does_not_exist() {
        let location = DatasetLocation::parse(&format!(
            "shared-memory://pchronicle-location-{}/missing",
            uuid::Uuid::new_v4().simple()
        ))
        .unwrap();
        assert!(!location.exists().await.unwrap());
    }

    #[tokio::test]
    async fn remove_all_drops_local_dataset_directory() {
        let temp = tempdir().unwrap();
        let dataset = temp.path().join("dataset");
        std::fs::create_dir(&dataset).unwrap();
        std::fs::write(dataset.join("source.json"), b"{}").unwrap();
        let location = DatasetLocation::parse(dataset.to_str().unwrap()).unwrap();

        location.remove_all().await.unwrap();

        assert!(!dataset.exists());
    }

    #[tokio::test]
    async fn remove_all_rejects_object_store_bucket_root() {
        let location = DatasetLocation::parse("memory://bucket").unwrap();
        let error = location.remove_all().await.unwrap_err().to_string();
        assert!(error.contains("entire object-store bucket"), "{error}");
    }
}
