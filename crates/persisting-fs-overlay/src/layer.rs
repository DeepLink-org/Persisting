// SPDX-License-Identifier: GPL-2.0-or-later
//
// Layer management: represents a single overlay layer (lower or upper).

use crate::datasource::{DataSource, StatOverrideMode};
use log::warn;
use std::os::fd::RawFd;

/// A single overlay layer.
pub struct OvlLayer {
    /// The data source providing filesystem access for this layer.
    pub ds: Box<dyn DataSource>,

    /// Whether this is a lower (read-only) layer.
    pub low: bool,
}

impl OvlLayer {
    /// Create a new layer from an initialized DataSource.
    pub fn from_datasource(ds: Box<dyn DataSource>, low: bool) -> Self {
        OvlLayer { ds, low }
    }

    pub fn root_fd(&self) -> RawFd {
        self.ds.root_fd()
    }
    pub fn st_dev(&self) -> libc::dev_t {
        self.ds.st_dev()
    }
    pub fn stat_override_mode(&self) -> StatOverrideMode {
        self.ds.stat_override_mode()
    }
}

/// Initialize layers from the parsed configuration.
/// Returns (upper_layer_option, vec_of_all_layers) where upper is also included
/// at index 0 of the vec if present.
pub fn init_layers(
    lowerdir: &str,
    upperdir: Option<&str>,
    nfs_filehandles_config: i32,
    xattr_permissions: i32,
) -> Result<Vec<OvlLayer>, String> {
    use crate::config;
    use crate::direct;

    let mut layers = Vec::new();
    let dirs = config::parse_lowerdir(lowerdir);

    // Upper layer first (if present)
    if let Some(upper) = upperdir {
        let mut ds = direct::new();
        ds.load_data_source(upper)
            .map_err(|e| format!("failed to load upper layer {}: {}", upper, e))?;

        // Apply xattr_permissions config to set stat override mode
        if ds.stat_override_mode() == StatOverrideMode::None {
            match xattr_permissions {
                1 => ds.set_stat_override(StatOverrideMode::Privileged),
                2 => ds.set_stat_override(StatOverrideMode::Containers),
                _ => {}
            }
        }

        // Check NFS file handle requirements
        if nfs_filehandles_config == 2 && ds.nfs_filehandles() != 1 {
            return Err("xino=on requires NFS file handle support on all layers".into());
        }

        layers.push(OvlLayer::from_datasource(Box::new(ds), false));
    }

    // Lower layers
    for dir in &dirs {
        let plugin = config::parse_plugin_path(dir);
        if plugin.is_some() {
            warn!("plugin layers not yet implemented: {}", dir);
            continue;
        }

        let mut ds = direct::new();
        ds.load_data_source(dir)
            .map_err(|e| format!("failed to load lower layer {}: {}", dir, e))?;

        if nfs_filehandles_config == 2 && ds.nfs_filehandles() != 1 {
            return Err(format!(
                "xino=on requires NFS file handle support on layer {}",
                dir
            ));
        }

        layers.push(OvlLayer::from_datasource(Box::new(ds), true));
    }

    if layers.is_empty() {
        return Err("no layers specified".into());
    }

    Ok(layers)
}

/// Check if all layers share the same device, enabling inode passthrough.
pub fn all_same_device(layers: &[OvlLayer]) -> bool {
    if layers.is_empty() {
        return true;
    }
    let dev = layers[0].st_dev();
    layers.iter().all(|l| l.st_dev() == dev)
}
