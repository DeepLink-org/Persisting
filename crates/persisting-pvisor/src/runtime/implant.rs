use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Optional in-process FUSE overlay root for one Attempt.
#[derive(Debug, Clone, Default)]
pub struct OverlayHint {
    /// Shared read-only lower layers (host paths).
    pub lower_dirs: Vec<PathBuf>,
    /// Writable upper directory for this Attempt.
    pub upper_dir: Option<PathBuf>,
    /// Work directory required by overlay implementations.
    pub work_dir: Option<PathBuf>,
    /// Merged mount point visible to the Agent as cwd/root when set.
    pub merged_dir: Option<PathBuf>,
}

/// Environment + cwd plan injected beside the Agent process.
#[derive(Debug, Clone, Default)]
pub struct ImplantPlan {
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub overlay: OverlayHint,
    pub notes: Vec<String>,
}

impl ImplantPlan {
    pub fn marker_env() -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert("PERSISTING_PVISOR_RUNTIME".into(), "1".into());
        env.insert("PERSISTING_PVISOR_ROLE".into(), "supervisor".into());
        env
    }

    pub fn as_metadata_json(&self) -> serde_json::Value {
        json!({
            "env_keys": self.env.keys().cloned().collect::<Vec<_>>(),
            "cwd": self.cwd.as_ref().map(|p| p.display().to_string()),
            "overlay_merged": self.overlay.merged_dir.as_ref().map(|p| p.display().to_string()),
            "notes": self.notes,
        })
    }
}
