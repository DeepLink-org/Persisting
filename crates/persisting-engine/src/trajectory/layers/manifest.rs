//! `layers/manifest.json` — local JSON registry of sidecar datasets for logical joins.
//!
//! Join semantics are application-defined (keys listed in each entry); this is not
//! an external columnar layout API.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{join_on_session_call_id, layer_field_name, layer_file_name, manifest_path};
use crate::trajectory::TrajectorySession;

/// One sidecar layer joined onto the primary event log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LayerManifestEntry {
    /// Top-level logical field name exposed after the join (e.g. `judge_default`).
    pub name: String,
    /// Path to the layer dataset, relative to the manifest directory.
    pub path: String,
    /// Column names present in both primary and layer used for the left join.
    pub join_on: Vec<String>,
}

/// Manifest listing all sidecar layers for a logical dataset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LayerManifest {
    /// Sidecar layers in stable order.
    pub layers: Vec<LayerManifestEntry>,
}

impl LayerManifest {
    pub fn from_json(json: &str) -> Result<Self> {
        serde_json::from_str(json).context("parse layers/manifest.json")
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("serialize layers/manifest.json")
    }
}

pub fn load_manifest(session: &TrajectorySession) -> Result<LayerManifest> {
    let path = manifest_path(session)?;
    if !path.is_file() {
        return Ok(LayerManifest::default());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read manifest {}", path.display()))?;
    LayerManifest::from_json(&raw)
}

pub fn save_manifest(session: &TrajectorySession, manifest: &LayerManifest) -> Result<()> {
    let path = manifest_path(session)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create manifest parent {}", parent.display()))?;
    }
    let json = manifest.to_json()?;
    std::fs::write(&path, json).with_context(|| format!("write manifest {}", path.display()))
}

/// Upsert a layer entry for `rubric_id` with join keys `[session_id, call_id]`.
pub fn register_layer(manifest: &mut LayerManifest, rubric_id: &str) -> String {
    let name = layer_field_name(rubric_id);
    let path = layer_file_name(rubric_id);
    let join_on = join_on_session_call_id();
    if let Some(entry) = manifest.layers.iter_mut().find(|e| e.name == name) {
        entry.path = path;
        entry.join_on = join_on;
        return name;
    }
    manifest.layers.push(LayerManifestEntry {
        name: name.clone(),
        path,
        join_on,
    });
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_layer_upserts_by_name() {
        let mut m = LayerManifest::default();
        let n1 = register_layer(&mut m, "default");
        let n2 = register_layer(&mut m, "default");
        assert_eq!(n1, n2);
        assert_eq!(m.layers.len(), 1);
        assert_eq!(m.layers[0].join_on, join_on_session_call_id());
        assert!(m.layers[0].path.ends_with(".lance"));
    }
}
