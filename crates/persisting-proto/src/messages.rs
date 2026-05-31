//! Request/response types shared by engine, CLI, and Python bindings.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

const DEFAULT_EMBEDDING_DIM: usize = 384;

fn default_embedding_dim() -> usize {
    DEFAULT_EMBEDDING_DIM
}

fn default_query_mode() -> String {
    "hybrid".to_string()
}

fn default_k() -> usize {
    10
}

fn default_vector_column() -> String {
    "embedding".to_string()
}

fn default_text_column() -> String {
    "text".to_string()
}

fn default_metric() -> String {
    "cosine".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchAddRequest {
    pub dataset: String,
    pub id: Option<String>,
    pub text: String,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    #[serde(default = "default_embedding_dim")]
    pub embedding_dim: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchAddResponse {
    pub dataset: String,
    pub id: String,
    pub embedding_dim: usize,
    pub embedding_preview: Vec<f32>,
    pub status: String,
    pub note: String,
}

/// 多行追加：引擎一次写入一个 Arrow `RecordBatch`（避免逐行 `SearchAdd` 反复打开/提交 Lance）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchAddBatchRequest {
    pub rows: Vec<SearchAddRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchAddBatchResponse {
    pub dataset: String,
    pub added: usize,
    pub embedding_dim: usize,
    pub embedding_preview: Vec<f32>,
    pub status: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQueryRequest {
    pub dataset: String,
    pub query: String,
    #[serde(default = "default_query_mode")]
    pub mode: String,
    #[serde(default = "default_k")]
    pub k: usize,
    #[serde(default = "default_embedding_dim")]
    pub embedding_dim: usize,
    /// Column for FTS / hybrid (`scan.full_text_search` / `QueryFilter::Fts`), default `text`.
    #[serde(default = "default_text_column")]
    pub text_column: String,
    pub filter: Option<String>,
    pub nprobes: Option<usize>,
    pub minimum_nprobes: Option<usize>,
    pub maximum_nprobes: Option<usize>,
    pub adaptive_nprobes_margin: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQueryResponse {
    pub dataset: String,
    pub mode: String,
    pub k: usize,
    pub query_embedding_preview: Vec<f32>,
    pub results: Vec<serde_json::Value>,
    pub status: String,
    pub note: String,
}

/// Vector + FTS index build request. IVF/PQ fields align with Lance
/// [`lance_index::vector::ivf::IvfBuildParams`], [`lance_index::vector::pq::PQBuildParams`],
/// and the hyperparameters used when `lance-tools index reorder` trains a temporary IVF-PQ pivot
/// (`rust/lance-tools/src/reorder.rs`: `vector_index_ivf_pq_for_reorder`, `ReorderArgs`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchIndexRequest {
    pub dataset: String,
    #[serde(default = "default_vector_column")]
    pub vector_column: String,
    #[serde(default = "default_text_column")]
    pub text_column: String,
    #[serde(default = "default_metric")]
    pub metric: String,
    /// IVF partition count (`IvfBuildParams::num_partitions`).
    pub num_partitions: Option<usize>,
    /// Max k-means iterations for IVF centroid training (`IvfBuildParams::max_iters`, default 50 in Lance).
    #[serde(default)]
    pub ivf_max_iters: Option<usize>,
    /// K-means balance loss weight for IVF training (`IvfBuildParams::balance_factor`; reorder uses `0.0`).
    #[serde(default)]
    pub ivf_balance_factor: Option<f32>,
    /// Enable oversized-cluster split postprocess after IVF k-means (reorder: `--ivf-balance-postprocess`).
    #[serde(default)]
    pub ivf_balance_postprocess: Option<bool>,
    /// Threshold ratio for that postprocess (reorder default `2.5` when postprocess is on).
    #[serde(default)]
    pub ivf_postprocess_max_cluster_ratio: Option<f32>,
    /// IVF training sample rate (`IvfBuildParams::sample_rate`, Lance default 256).
    #[serde(default)]
    pub ivf_sample_rate: Option<usize>,
    /// If set, partition count is derived from target row count per partition (`IvfBuildParams::target_partition_size`).
    #[serde(default)]
    pub ivf_target_partition_size: Option<usize>,
    /// Rows per batch when shuffling IVF training data.
    #[serde(default)]
    pub ivf_shuffle_partition_batches: Option<usize>,
    /// Concurrency for IVF shuffle.
    #[serde(default)]
    pub ivf_shuffle_partition_concurrency: Option<usize>,
    /// PQ sub-vector count (`PQBuildParams::num_sub_vectors`, Lance default 16).
    #[serde(default)]
    pub pq_num_sub_vectors: Option<usize>,
    /// Bits per PQ sub-code (`PQBuildParams::num_bits`, Lance default 8).
    #[serde(default)]
    pub pq_num_bits: Option<u8>,
    /// Max k-means iterations for PQ codebook (`PQBuildParams::max_iters`; reorder uses 50).
    #[serde(default)]
    pub pq_max_iters: Option<usize>,
    /// PQ k-means restarts (`PQBuildParams::kmeans_redos`, default 1).
    #[serde(default)]
    pub pq_kmeans_redos: Option<usize>,
    /// Sample rate for PQ codebook training (`PQBuildParams::sample_rate`, default 256).
    #[serde(default)]
    pub pq_sample_rate: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchIndexResponse {
    pub dataset: String,
    pub status: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchIndexListRequest {
    pub dataset: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchIndexListEntry {
    pub name: String,
    pub uuid: String,
    pub dataset_version: u64,
    pub index_version: i32,
    /// Indexed column paths (from manifest field ids).
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchIndexListResponse {
    pub dataset: String,
    pub indices: Vec<SearchIndexListEntry>,
    pub status: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchIndexDeleteRequest {
    pub dataset: String,
    pub index_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchIndexDeleteResponse {
    pub dataset: String,
    pub index_name: String,
    pub status: String,
    pub note: String,
}

fn default_rebuild_retrain() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchIndexRebuildRequest {
    pub dataset: String,
    /// When `None`, applies to **all** indices (Lance `OptimizeOptions::index_names = None`).
    pub index_name: Option<String>,
    /// When `true`, calls `OptimizeOptions` with `retrain` (v3 vector indices; see Lance docs).
    #[serde(default = "default_rebuild_retrain")]
    pub retrain: bool,
    /// Used when `retrain` is `false`: maps to `OptimizeOptions::num_indices_to_merge`.
    #[serde(default)]
    pub merge_num_indices: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchIndexRebuildResponse {
    pub dataset: String,
    pub index_name: Option<String>,
    pub retrain: bool,
    pub status: String,
    pub note: String,
}

/// IVF 物理重排（按 IVF 分区重写物理布局并重映射索引 row id；不重新训练 PQ）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchIndexReorderRequest {
    pub dataset: String,
    pub pivot_index: String,
    pub target: Option<String>,
    pub in_place: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchIndexReorderResponse {
    pub dataset: String,
    pub pivot_index: String,
    pub target: Option<String>,
    pub in_place: bool,
    pub status: String,
    pub note: String,
    /// 引擎 reorder 流水线写入的进度日志（UTF-8）。
    pub log: String,
}

/// Import from an existing on-disk Lance dataset (`.../dataset_dir` with `data.lance/` child).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchImportLanceRequest {
    pub target_dataset: String,
    /// Path or URI accepted by [`lance::Dataset::open`].
    pub source_lance: String,
    #[serde(default = "default_text_column")]
    pub source_text_column: String,
    #[serde(default)]
    pub source_id_column: Option<String>,
    #[serde(default = "default_embedding_dim")]
    pub embedding_dim: usize,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchImportLanceResponse {
    pub target_dataset: String,
    pub source_lance: String,
    pub source_row_count: usize,
    pub source_field_names: Vec<String>,
    pub embedding_dim: usize,
    pub limit: Option<usize>,
    pub status: String,
    pub note: String,
}

// ---------------------------------------------------------------------------
// Trajectory
// ---------------------------------------------------------------------------

/// Physical storage layer for a session trajectory (Vortex canonical + TLV Markdown view).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryStorageFormat {
    /// Read: Vortex if both layers exist else the available layer. Append: always Vortex.
    /// Stats: summarize both layers when present.
    #[default]
    Auto,
    /// Vortex raw event log (canonical).
    Vortex,
    /// TLV Markdown session file (materialized / human-readable view).
    #[serde(rename = "markdown", alias = "tlv")]
    Markdown,
    /// Legacy alias: append writes [`Vortex`](Self::Vortex) only; read/stats like [`Auto`](Self::Auto).
    #[serde(alias = "both")]
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryAppendRequest {
    pub storage: String,
    /// Stable agent identity (directory segment under `storage/`).
    pub agent_id: String,
    /// Session / run scope within the agent (directory segment under `storage/agent_id/`).
    pub session_id: String,
    /// When set, nested subagent sessions live under `{root_session_id}/subagents/{session_id}/`.
    #[serde(default)]
    pub root_session_id: Option<String>,
    /// Newline-separated engine lines (RON) for event log append.
    pub records_ronl: String,
    #[serde(default)]
    pub storage_format: TrajectoryStorageFormat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryAppendResponse {
    pub storage: String,
    pub agent_id: String,
    pub session_id: String,
    pub accepted_records: usize,
    pub dataset: String,
    pub status: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryReplayRequest {
    pub storage: String,
    pub agent_id: String,
    pub session_id: String,
    #[serde(default)]
    pub offset: usize,
    pub limit: Option<usize>,
    #[serde(default)]
    pub storage_format: TrajectoryStorageFormat,
    /// When set, read nested session at `{root_session_id}/subagents/{session_id}/`.
    #[serde(default)]
    pub root_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryReplayResponse {
    pub storage: String,
    pub agent_id: String,
    pub session_id: String,
    /// JSON object lines (one per step) when populated.
    pub records: Vec<String>,
    pub status: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryStatsRequest {
    pub storage: String,
    pub agent_id: String,
    pub session_id: String,
    #[serde(default)]
    pub storage_format: TrajectoryStorageFormat,
    /// When set, read nested session at `{root_session_id}/subagents/{session_id}/`.
    #[serde(default)]
    pub root_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryStatsResponse {
    pub storage: String,
    pub agent_id: String,
    pub session_id: String,
    pub dataset: String,
    /// Row count in the event log (`0` when file is missing).
    pub row_count: usize,
    /// Reserved for future versioning metadata (`None` for Vortex).
    pub manifest_version: Option<u64>,
    pub status: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryMaterializeRequest {
    pub storage: String,
    pub agent_id: String,
    pub session_id: String,
    #[serde(default)]
    pub root_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryMaterializeResponse {
    pub storage: String,
    pub agent_id: String,
    pub session_id: String,
    pub markdown_path: String,
    pub event_rows: usize,
    pub markdown_blocks: usize,
    pub skipped_events: usize,
    pub status: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryTruncateRequest {
    pub storage: String,
    pub agent_id: String,
    pub session_id: String,
    #[serde(default)]
    pub root_session_id: Option<String>,
    /// Keep the first N event log rows (ordered by `seq`).
    pub keep_rows: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryTruncateResponse {
    pub storage: String,
    pub agent_id: String,
    pub session_id: String,
    pub kept_rows: usize,
    pub removed_rows: usize,
    pub status: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryExtractRequest {
    pub storage: String,
    pub agent_id: String,
    pub session_id: String,
    #[serde(default)]
    pub root_session_id: Option<String>,
    pub out_dir: String,
    /// When set on a capture run root story, copy the full run including `subagents/`.
    #[serde(default)]
    pub include_subagents: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryExtractResponse {
    pub storage: String,
    pub agent_id: String,
    pub session_id: String,
    pub out_dir: String,
    pub files_copied: usize,
    pub status: String,
    pub note: String,
}

// ---------------------------------------------------------------------------
// RPC envelope
// ---------------------------------------------------------------------------

/// Increment when bincode [`RpcRequest`] / [`RpcResponse`] layout or semantics change.
pub const PROTOCOL_VERSION: u32 = 11;

/// RON C ABI：稳定导出为 **`persisting_engine_submit` / `job_poll` / `job_take_result` / `job_release`**（见 **`invoke_abi`**）；
/// 当信封、job 状态布局或上述符号契约不兼容变化时递增。与 [`PROTOCOL_VERSION`] 独立。
pub const RON_ABI_VERSION: u32 = 6;

pub mod error_codes {
    pub const VERSION_MISMATCH: u32 = 10;
    pub const MALFORMED_REQUEST: u32 = 11;
    pub const APPLICATION_ERROR: u32 = 12;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub version: u32,
    pub body: RequestBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RequestBody {
    SearchAdd(SearchAddRequest),
    SearchQuery(SearchQueryRequest),
    SearchIndex(SearchIndexRequest),
    SearchIndexList(SearchIndexListRequest),
    SearchIndexDelete(SearchIndexDeleteRequest),
    SearchIndexRebuild(SearchIndexRebuildRequest),
    SearchIndexReorder(SearchIndexReorderRequest),
    TrajectoryAppend(TrajectoryAppendRequest),
    TrajectoryReplay(TrajectoryReplayRequest),
    TrajectoryStats(TrajectoryStatsRequest),
    TrajectoryMaterialize(TrajectoryMaterializeRequest),
    TrajectoryTruncate(TrajectoryTruncateRequest),
    TrajectoryExtract(TrajectoryExtractRequest),
    SearchImportLance(SearchImportLanceRequest),
    SearchAddBatch(SearchAddBatchRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub version: u32,
    pub body: ResponseBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResponseBody {
    SearchAdd(SearchAddResponse),
    SearchQuery(SearchQueryResponse),
    SearchIndex(SearchIndexResponse),
    SearchIndexList(SearchIndexListResponse),
    SearchIndexDelete(SearchIndexDeleteResponse),
    SearchIndexRebuild(SearchIndexRebuildResponse),
    SearchIndexReorder(SearchIndexReorderResponse),
    TrajectoryAppend(TrajectoryAppendResponse),
    TrajectoryReplay(TrajectoryReplayResponse),
    TrajectoryStats(TrajectoryStatsResponse),
    TrajectoryMaterialize(TrajectoryMaterializeResponse),
    TrajectoryTruncate(TrajectoryTruncateResponse),
    TrajectoryExtract(TrajectoryExtractResponse),
    SearchImportLance(SearchImportLanceResponse),
    SearchAddBatch(SearchAddBatchResponse),
    Error { code: u32, message: String },
}
