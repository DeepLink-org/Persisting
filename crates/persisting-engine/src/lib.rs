//! Persisting engine: Lance-backed search plus stable RPC/C ABI boundaries.

pub mod abi;
pub mod jobs;
pub mod rpc;
pub mod search;

// Compatibility module paths. Implementations are physically grouped under
// `rpc` so trajectory adapters cannot be mistaken for Engine-owned storage.
pub use rpc::{bridge, dispatch, ron_api, trajectory};

/// 兼容旧路径：`persisting_engine::agent_search`（例如 PyO3 绑定）。
pub use search::agent as agent_search;

/// IVF-PQ 向量索引逻辑名（与 CLI / `search index build` 一致）。
pub const PERSISTING_VECTOR_INDEX_NAME: &str = crate::search::lance::PERSISTING_VECTOR_INDEX_NAME;
/// FTS 倒排索引逻辑名（`text_column` 上）。
pub const PERSISTING_FTS_INDEX_NAME: &str = crate::search::lance::PERSISTING_FTS_INDEX_NAME;

pub use persisting_proto::PROTOCOL_VERSION;
pub use rpc::bridge::{
    search_add, search_add_batch, search_import_lance, search_index, search_index_delete,
    search_index_list, search_index_rebuild, search_index_reorder, search_query, trajectory_append,
    trajectory_replay, trajectory_stats,
};
pub use rpc::dispatch::{dispatch_bytes, handle_rpc_request, invoke_request_body};
pub use rpc::ron_api::invoke_ron;
