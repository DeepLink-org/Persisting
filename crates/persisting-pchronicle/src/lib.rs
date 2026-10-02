#![recursion_limit = "256"]

//! pChronicle：Persisting 的结构化 Agent 轨迹存储与查询层。
//!
//! # 权威边界
//!
//! [`model::StorylineDocument`] 是轨迹处理模型。ATIF、ACTF、OpenAI Msg 和 AgenticMD
//! 经 Storyline 互转；Storyline 可以直接写入三表 Lance 并通过 DataFusion 查询。
//!
//! # 公共入口
//!
//! - [`model`]：Storyline 与 LLM payload 权威类型；
//! - [`document`]：磁盘格式、Storyline 语义 codec 与统一读取入口；Codex / Claude Code 会话 JSONL 为 decode-only；
//! - [`storage`]：Catalog、Lance store 与 append；
//! - [`query`]：DataFusion 查询引擎与能力快照。
//!
//! 外围 wire DTO、低层 parser、Markdown AST、Arrow codec、provider、manifest 与锁均不公开。
//! Storyline 全文检索随 `lance-store` 提供；向量与混合检索仍由 `search` feature 启用。

mod agenticmd;
pub mod analysis_compile;
mod atif;
mod convert;
pub mod document;
mod format;
mod formats;
mod input;
#[cfg(feature = "search")]
mod messages;
pub mod model;
#[cfg(feature = "search")]
mod operations;
pub mod query;
#[cfg(feature = "lance-store")]
pub mod search;
pub mod storage;
mod store;

#[cfg(feature = "lance-store")]
pub(crate) use document::{QueryCapabilities, QueryTables};
#[cfg(feature = "lance-store")]
pub(crate) use format::DocumentFormat;
pub(crate) use formats::StorylineDocument;
#[cfg(feature = "lance-store")]
pub(crate) use formats::storyline::StorylineAgent;
pub(crate) use formats::storyline::StorylineTurn;
#[cfg(any(feature = "lance-store", test))]
pub(crate) use formats::storyline::{StoryLink, StorylineToolCall};
pub(crate) use input::{InputIssue, InputResult};
pub type Result<T> = anyhow::Result<T>;
#[cfg(feature = "search")]
pub use messages::*;
#[cfg(feature = "search")]
pub use operations::bridge::{
    search_add, search_add_batch, search_import_lance, search_index, search_index_delete,
    search_index_list, search_index_rebuild, search_index_reorder, search_query,
};
#[cfg(feature = "search")]
pub use operations::dispatch::invoke_request_body;
#[cfg(feature = "search")]
pub use search::agent as agent_search;
#[cfg(feature = "search")]
pub const PERSISTING_VECTOR_INDEX_NAME: &str = search::search_lance::PERSISTING_VECTOR_INDEX_NAME;
#[cfg(feature = "search")]
pub const PERSISTING_FTS_INDEX_NAME: &str = search::search_lance::PERSISTING_FTS_INDEX_NAME;

#[cfg(all(test, feature = "lance-store"))]
mod tests;
