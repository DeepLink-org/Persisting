//! `layers/manifest.json` — sidecar registry for logical Vortex joins.

use anyhow::{Context, Result};
use vortex::layout::scan::logical::{LayerManifest, LayerManifestEntry};

use super::{join_on_session_call_id, layer_field_name, layer_file_name, manifest_path};
use crate::trajectory::TrajectorySession;

pub fn load_manifest(session: &TrajectorySession) -> Result<LayerManifest> {
    let path = manifest_path(session)?;
    if !path.is_file() {
        return Ok(LayerManifest::default());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read manifest {}", path.display()))?;
    LayerManifest::from_json(&raw).context("parse layers/manifest.json")
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
    }
}
