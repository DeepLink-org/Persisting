//! `chronicle.manifest` Dataset sidecar (RFC-0015).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

pub const CHRONICLE_MANIFEST_FILE: &str = "chronicle.manifest";
pub const CHRONICLE_MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const COMPACT_JSONL_FORMAT: &str = "compact-jsonl/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestKind {
    Leaf,
    Branch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestIdentity {
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestStats {
    pub record_count: u64,
    pub failed_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChronicleManifest {
    pub schema_version: u32,
    pub kind: ManifestKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<ManifestIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<ManifestStats>,
}

impl ChronicleManifest {
    pub fn leaf_compact_jsonl(fingerprint: impl Into<String>, record_count: u64) -> Self {
        Self {
            schema_version: CHRONICLE_MANIFEST_SCHEMA_VERSION,
            kind: ManifestKind::Leaf,
            format: Some(COMPACT_JSONL_FORMAT.into()),
            identity: Some(ManifestIdentity {
                fingerprint: fingerprint.into(),
            }),
            stats: Some(ManifestStats {
                record_count,
                failed_count: 0,
                min_timestamp: None,
                max_timestamp: None,
                total_tokens: None,
            }),
        }
    }

    pub fn branch() -> Self {
        Self {
            schema_version: CHRONICLE_MANIFEST_SCHEMA_VERSION,
            kind: ManifestKind::Branch,
            format: None,
            identity: None,
            stats: None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == CHRONICLE_MANIFEST_SCHEMA_VERSION,
            "unsupported chronicle.manifest schema_version {}; expected {}",
            self.schema_version,
            CHRONICLE_MANIFEST_SCHEMA_VERSION
        );
        match self.kind {
            ManifestKind::Leaf => {
                let format = self
                    .format
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .context("leaf chronicle.manifest requires format")?;
                ensure!(
                    !format.is_empty(),
                    "leaf chronicle.manifest format must not be empty"
                );
            }
            ManifestKind::Branch => {
                ensure!(
                    self.format.is_none(),
                    "branch chronicle.manifest must not set format"
                );
            }
        }
        if let Some(_stats) = &self.stats {
            let identity = self
                .identity
                .as_ref()
                .context("chronicle.manifest stats require [identity].fingerprint")?;
            ensure!(
                !identity.fingerprint.trim().is_empty(),
                "chronicle.manifest fingerprint must not be empty"
            );
        }
        Ok(())
    }

    pub fn is_compact_jsonl_leaf(&self) -> bool {
        self.kind == ManifestKind::Leaf
            && self.format.as_deref() == Some(COMPACT_JSONL_FORMAT)
            && self.validate().is_ok()
    }
}

pub fn manifest_path(root: impl AsRef<Path>) -> PathBuf {
    root.as_ref().join(CHRONICLE_MANIFEST_FILE)
}

pub fn lance_version_fingerprint(version: u64) -> String {
    format!("lance:version:{version}")
}

pub fn load_manifest(root: impl AsRef<Path>) -> Result<Option<ChronicleManifest>> {
    let path = manifest_path(root);
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)
        .with_context(|| format!("read chronicle.manifest {}", path.display()))?;
    let manifest: ChronicleManifest = toml::from_str(&text)
        .with_context(|| format!("parse chronicle.manifest {}", path.display()))?;
    manifest.validate()?;
    Ok(Some(manifest))
}

pub fn try_load_manifest(root: impl AsRef<Path>) -> Option<ChronicleManifest> {
    match load_manifest(root) {
        Ok(manifest) => manifest,
        Err(error) => {
            tracing::warn!(
                target: "persisting_pchronicle::chronicle_manifest",
                error = %error,
                "ignoring invalid chronicle.manifest"
            );
            None
        }
    }
}

pub fn atomic_write_manifest(root: impl AsRef<Path>, manifest: &ChronicleManifest) -> Result<()> {
    manifest.validate()?;
    let root = root.as_ref();
    fs::create_dir_all(root)
        .with_context(|| format!("create chronicle.manifest parent {}", root.display()))?;
    let path = manifest_path(root);
    let temporary = root.join(format!(
        ".{}.tmp-{}",
        CHRONICLE_MANIFEST_FILE,
        std::process::id()
    ));
    let encoded = toml::to_string_pretty(manifest).context("encode chronicle.manifest")?;
    {
        let mut file = fs::File::create(&temporary)
            .with_context(|| format!("create chronicle.manifest temp {}", temporary.display()))?;
        file.write_all(encoded.as_bytes())
            .with_context(|| format!("write chronicle.manifest temp {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync chronicle.manifest temp {}", temporary.display()))?;
    }
    fs::rename(&temporary, &path).with_context(|| {
        let _ = fs::remove_file(&temporary);
        format!(
            "publish chronicle.manifest {} from {}",
            path.display(),
            temporary.display()
        )
    })?;
    Ok(())
}

pub fn write_compact_jsonl_manifest(
    root: impl AsRef<Path>,
    lance_version: u64,
    record_count: u64,
) -> Result<()> {
    let manifest = ChronicleManifest::leaf_compact_jsonl(
        lance_version_fingerprint(lance_version),
        record_count,
    );
    atomic_write_manifest(root, &manifest)
}

/// True when a compact-jsonl leaf manifesto matches one Lance version.
pub fn compact_jsonl_manifest_matches(manifest: &ChronicleManifest, lance_version: u64) -> bool {
    manifest.is_compact_jsonl_leaf()
        && manifest.identity.as_ref().is_some_and(|identity| {
            identity.fingerprint == lance_version_fingerprint(lance_version)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn leaf_round_trip_and_validation() {
        let manifest = ChronicleManifest::leaf_compact_jsonl("lance:version:3", 12);
        manifest.validate().unwrap();
        assert!(manifest.is_compact_jsonl_leaf());
        let encoded = toml::to_string_pretty(&manifest).unwrap();
        let decoded: ChronicleManifest = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded, manifest);
    }

    #[test]
    fn branch_rejects_format() {
        let mut manifest = ChronicleManifest::branch();
        manifest.format = Some(COMPACT_JSONL_FORMAT.into());
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn atomic_write_creates_readable_file() {
        let dir = tempdir().unwrap();
        let manifest = ChronicleManifest::leaf_compact_jsonl("lance:version:1", 4);
        atomic_write_manifest(dir.path(), &manifest).unwrap();
        let loaded = load_manifest(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.stats.unwrap().record_count, 4);
        assert_eq!(loaded.identity.unwrap().fingerprint, "lance:version:1");
    }

    #[test]
    fn load_missing_returns_none() {
        let dir = tempdir().unwrap();
        assert!(load_manifest(dir.path()).unwrap().is_none());
    }

    #[test]
    fn reject_unsupported_schema_version() {
        let err = ChronicleManifest {
            schema_version: 99,
            kind: ManifestKind::Branch,
            format: None,
            identity: None,
            stats: None,
        }
        .validate()
        .unwrap_err();
        assert!(err.to_string().contains("schema_version"));
    }

    #[test]
    fn stats_require_fingerprint() {
        let err = ChronicleManifest {
            schema_version: 1,
            kind: ManifestKind::Leaf,
            format: Some(COMPACT_JSONL_FORMAT.into()),
            identity: None,
            stats: Some(ManifestStats {
                record_count: 1,
                failed_count: 0,
                min_timestamp: None,
                max_timestamp: None,
                total_tokens: None,
            }),
        }
        .validate()
        .unwrap_err();
        assert!(err.to_string().contains("identity"));
    }
}
