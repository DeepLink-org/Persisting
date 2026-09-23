//! User-facing product terminology.
//!
//! Keep storage and API names such as `Storyline`, `CanonicalEvent`, and
//! `CatalogEventProvenance` in their owning modules. The UI uses the simpler
//! vocabulary below so implementation details do not leak into navigation and
//! everyday workflows.
//!
//! This branch defaults to Chinese. English originals are kept in [`en`] for a
//! future locale switch (no runtime switch in this branch).

/// English product terminology (source of truth for a future `en` locale).
#[allow(dead_code)]
pub mod en {
    pub const DATASETS: &str = "Datasets";
    pub const RUNS: &str = "Runs";
    pub const ANALYSIS: &str = "Analysis";
    pub const STORAGE: &str = "Storage";
    pub const ASSISTANT: &str = "Assistant";
    pub const TIMELINE: &str = "Timeline";
    pub const STEPS: &str = "Steps";
    pub const RECORDED_EVENTS: &str = "Recorded events";
    pub const RECONSTRUCTED_EVENTS: &str = "Reconstructed events";
    pub const REQUESTS: &str = "Requests";
    pub const KEYS: &str = "Keys";
    pub const SETTINGS: &str = "Settings";
}

pub const DATASETS: &str = "数据集";
pub const RUNS: &str = "运行";
pub const ANALYSIS: &str = "分析";
pub const STORAGE: &str = "存储";
pub const ASSISTANT: &str = "助手";
pub const TIMELINE: &str = "时间线";
pub const STEPS: &str = "步骤";
pub const RECORDED_EVENTS: &str = "已记录事件";
pub const RECONSTRUCTED_EVENTS: &str = "重建事件";
pub const REQUESTS: &str = "请求";
pub const KEYS: &str = "密钥";
pub const SETTINGS: &str = "设置";
