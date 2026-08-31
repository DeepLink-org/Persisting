//! Shared JSON CAS for per-key records.
//!
//! Local backend: exclusive flock, then tmp `create_new` + rename.
//! Object backend: conditional `PutMode::Create` / `PutMode::Update` with retries.

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use futures::TryStreamExt;
use lance::io::ObjectStore;
use object_store::path::Path as ObjectPath;
use object_store::{Error as ObjectStoreError, ObjectStoreExt, PutMode, UpdateVersion};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) const CAS_RETRIES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Mutation<Record, T> {
    Unchanged(T),
    Replace(Record, T),
}

enum Backend {
    Local(PathBuf),
    Object {
        store: Arc<ObjectStore>,
        root: ObjectPath,
    },
}

pub(crate) struct CasStore {
    root_uri: String,
    name: &'static str,
    backend: Backend,
}

impl std::fmt::Debug for CasStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CasStore")
            .field("root_uri", &self.root_uri)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl CasStore {
    pub async fn open(root: impl AsRef<str>, directory: &str, name: &'static str) -> Result<Self> {
        let root = root.as_ref().trim();
        if root.is_empty() {
            bail!("{name} root must not be empty");
        }
        if !root.contains("://") {
            let path = PathBuf::from(root).join(directory);
            tokio::fs::create_dir_all(&path)
                .await
                .with_context(|| format!("create {name} root {}", path.display()))?;
            return Ok(Self {
                root_uri: root.to_string(),
                name,
                backend: Backend::Local(path),
            });
        }
        let (store, object_root) = ObjectStore::from_uri(root)
            .await
            .with_context(|| format!("open {name} object store {root}"))?;
        Ok(Self {
            root_uri: root.to_string(),
            name,
            backend: Backend::Object {
                store,
                root: object_root.join(directory),
            },
        })
    }

    pub fn root_uri(&self) -> &str {
        &self.root_uri
    }

    pub async fn get<Record>(&self, key: &str) -> Result<Option<Record>>
    where
        Record: DeserializeOwned + Send + 'static,
    {
        match &self.backend {
            Backend::Local(root) => {
                let path = record_path(root, key);
                let name = self.name;
                tokio::task::spawn_blocking(move || read_local_record(&path, name)).await?
            }
            Backend::Object { store, root } => {
                Ok(
                    read_object_record(store, &object_record_path(root, key), self.name)
                        .await?
                        .map(|(record, _)| record),
                )
            }
        }
    }

    pub async fn list<Record>(&self) -> Result<Vec<Record>>
    where
        Record: DeserializeOwned + Send + 'static,
    {
        match &self.backend {
            Backend::Local(root) => {
                let root = root.clone();
                let name = self.name;
                tokio::task::spawn_blocking(move || list_local_records(&root, name)).await?
            }
            Backend::Object { store, root } => {
                let prefix = root.clone();
                let objects = store
                    .inner
                    .list(Some(&prefix))
                    .try_collect::<Vec<_>>()
                    .await?;
                let mut out = Vec::new();
                for object in objects {
                    if object.location.extension() != Some("json") {
                        continue;
                    }
                    if let Some((record, _)) =
                        read_object_record(store, &object.location, self.name).await?
                    {
                        out.push(record);
                    }
                }
                Ok(out)
            }
        }
    }

    pub async fn mutate<Record, T, F>(&self, key: &str, mutate: F) -> Result<T>
    where
        Record: Serialize + DeserializeOwned + Send + 'static,
        T: Send + 'static,
        F: Fn(Option<&Record>) -> Result<Mutation<Record, T>> + Send + Sync + 'static,
    {
        match &self.backend {
            Backend::Local(root) => {
                let path = record_path(root, key);
                let lock_path = path.with_extension("lock");
                let name = self.name;
                tokio::task::spawn_blocking(move || {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let lock = OpenOptions::new()
                        .create(true)
                        .truncate(false)
                        .read(true)
                        .write(true)
                        .open(&lock_path)?;
                    lock.lock_exclusive()?;
                    let current = read_local_record(&path, name)?;
                    let outcome = mutate(current.as_ref())?;
                    let value = match outcome {
                        Mutation::Unchanged(value) => value,
                        Mutation::Replace(record, value) => {
                            write_local_record(&path, &record, name)?;
                            value
                        }
                    };
                    FileExt::unlock(&lock)?;
                    Ok(value)
                })
                .await?
            }
            Backend::Object { store, root } => {
                let path = object_record_path(root, key);
                for _ in 0..CAS_RETRIES {
                    let current = read_object_record(store, &path, self.name).await?;
                    let outcome = mutate(current.as_ref().map(|(record, _)| record))?;
                    let (record, value) = match outcome {
                        Mutation::Unchanged(value) => return Ok(value),
                        Mutation::Replace(record, value) => (record, value),
                    };
                    let mode = match current {
                        None => PutMode::Create,
                        Some((_, version)) => PutMode::Update(version),
                    };
                    let bytes = serde_json::to_vec_pretty(&record)?;
                    match store.inner.put_opts(&path, bytes.into(), mode.into()).await {
                        Ok(_) => return Ok(value),
                        Err(ObjectStoreError::AlreadyExists { .. })
                        | Err(ObjectStoreError::Precondition { .. }) => continue,
                        Err(error) => return Err(error.into()),
                    }
                }
                bail!(
                    "{} CAS contention exceeded {CAS_RETRIES} retries",
                    self.name
                )
            }
        }
    }
}

pub use persisting_events::unix_now_ms;

pub(crate) fn encoded_id(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                encoded.push(char::from(byte));
            }
            other => encoded.push_str(&format!("~{other:02x}")),
        }
    }
    encoded
}

fn record_path(root: &Path, key: &str) -> PathBuf {
    root.join(format!("{}.json", encoded_id(key)))
}

fn object_record_path(root: &ObjectPath, key: &str) -> ObjectPath {
    root.clone().join(format!("{}.json", encoded_id(key)))
}

fn list_local_records<Record: DeserializeOwned>(
    root: &Path,
    name: &'static str,
) -> Result<Vec<Record>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        if let Some(record) = read_local_record(&entry.path(), name)? {
            out.push(record);
        }
    }
    Ok(out)
}

fn read_local_record<Record: DeserializeOwned>(
    path: &Path,
    name: &'static str,
) -> Result<Option<Record>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
        format!("decode {name} record {}", path.display())
    })?))
}

fn write_local_record<Record: Serialize>(
    path: &Path,
    record: &Record,
    name: &'static str,
) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{name} path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("record"),
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(record)?)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

async fn read_object_record<Record: DeserializeOwned>(
    store: &Arc<ObjectStore>,
    path: &ObjectPath,
    name: &'static str,
) -> Result<Option<(Record, UpdateVersion)>> {
    let result = match store.inner.get(path).await {
        Ok(result) => result,
        Err(ObjectStoreError::NotFound { .. }) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let version = UpdateVersion {
        e_tag: result.meta.e_tag.clone(),
        version: result.meta.version.clone(),
    };
    let bytes = result.bytes().await?;
    let record =
        serde_json::from_slice(&bytes).with_context(|| format!("decode {name} object {path}"))?;
    Ok(Some((record, version)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Rec {
        revision: u64,
        key: String,
        value: String,
    }

    #[test]
    fn encoded_id_keeps_safe_chars_and_escapes_the_rest() {
        assert_eq!(encoded_id("abc-XYZ_1.2"), "abc-XYZ_1.2");
        assert_eq!(encoded_id("a/b"), "a~2fb");
        assert_eq!(encoded_id("a~b"), "a~7eb");
        assert_eq!(encoded_id(""), "");
    }

    #[test]
    fn unix_now_ms_matches_system_clock_and_is_non_decreasing() {
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        let first = unix_now_ms();
        let second = unix_now_ms();
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        assert!(first >= before, "got {first} before {before}");
        assert!(second >= first, "got {second} after {first}");
        assert!(after >= second, "got {after} after {second}");
    }

    #[tokio::test]
    async fn local_mutate_replace_then_get_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().to_str().unwrap(), "records", "Test store")
            .await
            .unwrap();
        assert!(store.get::<Rec>("run/1").await.unwrap().is_none());
        store
            .mutate::<Rec, _, _>("run/1", |_| {
                Ok(Mutation::Replace(
                    Rec {
                        revision: 1,
                        key: "run/1".into(),
                        value: "one".into(),
                    },
                    (),
                ))
            })
            .await
            .unwrap();
        assert_eq!(
            store.get::<Rec>("run/1").await.unwrap().unwrap(),
            Rec {
                revision: 1,
                key: "run/1".into(),
                value: "one".into(),
            }
        );
        let path = dir.path().join("records").join("run~2f1.json");
        assert!(path.is_file());
    }

    #[tokio::test]
    async fn local_unchanged_does_not_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().to_str().unwrap(), "records", "Test store")
            .await
            .unwrap();
        store
            .mutate::<Rec, _, _>("k", |_| {
                Ok(Mutation::Replace(
                    Rec {
                        revision: 1,
                        key: "k".into(),
                        value: "v".into(),
                    },
                    (),
                ))
            })
            .await
            .unwrap();
        let path = dir.path().join("records").join("k.json");
        let before = std::fs::read(&path).unwrap();
        store
            .mutate::<Rec, _, _>("k", |_| Ok(Mutation::Unchanged(())))
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[tokio::test]
    async fn local_write_uses_create_new_and_does_not_truncate_existing_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().to_str().unwrap(), "records", "Test store")
            .await
            .unwrap();
        store
            .mutate::<Rec, _, _>("k", |_| {
                Ok(Mutation::Replace(
                    Rec {
                        revision: 1,
                        key: "k".into(),
                        value: "v".into(),
                    },
                    (),
                ))
            })
            .await
            .unwrap();
        let tmp = dir
            .path()
            .join("records")
            .join(format!(".k.json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, b"stale").unwrap();
        let err = store
            .mutate::<Rec, _, _>("k", |_| {
                Ok(Mutation::Replace(
                    Rec {
                        revision: 2,
                        key: "k".into(),
                        value: "next".into(),
                    },
                    (),
                ))
            })
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("File exists")
                || err
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists)
                || format!("{err:#}").contains("already exists"),
            "{err:#}"
        );
        assert_eq!(
            store.get::<Rec>("k").await.unwrap().unwrap().revision,
            1,
            "failed create_new must not clobber the committed record"
        );
    }

    #[tokio::test]
    async fn local_list_returns_json_records() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().to_str().unwrap(), "records", "Test store")
            .await
            .unwrap();
        for key in ["b", "a"] {
            let owned = key.to_string();
            store
                .mutate::<Rec, _, _>(key, move |_| {
                    Ok(Mutation::Replace(
                        Rec {
                            revision: 1,
                            key: owned.clone(),
                            value: owned.clone(),
                        },
                        (),
                    ))
                })
                .await
                .unwrap();
        }
        let mut records = store.list::<Rec>().await.unwrap();
        records.sort_by(|left, right| left.key.cmp(&right.key));
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].key, "a");
        assert_eq!(records[1].key, "b");
    }

    #[tokio::test]
    async fn object_store_backend_uses_conditional_create_and_update() {
        let root = format!(
            "shared-memory://cas-store-{}-{}/root",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );
        let store = CasStore::open(&root, "records", "Test store")
            .await
            .unwrap();
        store
            .mutate::<Rec, _, _>("run-object", |_| {
                Ok(Mutation::Replace(
                    Rec {
                        revision: 1,
                        key: "run-object".into(),
                        value: "one".into(),
                    },
                    (),
                ))
            })
            .await
            .unwrap();
        store
            .mutate::<Rec, _, _>("run-object", |current| {
                let mut record = current.cloned().unwrap();
                record.revision = 2;
                record.value = "two".into();
                Ok(Mutation::Replace(record, ()))
            })
            .await
            .unwrap();
        let record = store.get::<Rec>("run-object").await.unwrap().unwrap();
        assert_eq!(record.revision, 2);
        assert_eq!(record.value, "two");
        let listed = store.list::<Rec>().await.unwrap();
        assert_eq!(listed.len(), 1);
    }

    #[cfg(feature = "proptest")]
    mod proptests {
        use proptest::prelude::*;

        use super::*;

        fn decode_id(encoded: &str) -> Vec<u8> {
            let bytes = encoded.as_bytes();
            let mut decoded = Vec::new();
            let mut index = 0;
            while index < bytes.len() {
                if bytes[index] == b'~' {
                    let value = u8::from_str_radix(
                        std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap(),
                        16,
                    )
                    .unwrap();
                    decoded.push(value);
                    index += 3;
                } else {
                    decoded.push(bytes[index]);
                    index += 1;
                }
            }
            decoded
        }

        proptest! {
            #[test]
            fn encoded_ids_roundtrip_arbitrary_utf8(value in any::<String>()) {
                let encoded = encoded_id(&value);
                let decoded = String::from_utf8(decode_id(&encoded)).unwrap();
                prop_assert_eq!(decoded, value);
            }

            #[test]
            fn encoded_ids_use_only_safe_filename_alphabet(value in any::<String>()) {
                let encoded = encoded_id(&value);
                let bytes = encoded.as_bytes();
                let mut index = 0;
                while index < bytes.len() {
                    if bytes[index] == b'~' {
                        prop_assert!(index + 2 < bytes.len());
                        prop_assert!(bytes[index + 1].is_ascii_hexdigit());
                        prop_assert!(bytes[index + 2].is_ascii_hexdigit());
                        index += 3;
                    } else {
                        prop_assert!(bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'-' | b'_' | b'.'));
                        index += 1;
                    }
                }
            }
        }
    }
}
