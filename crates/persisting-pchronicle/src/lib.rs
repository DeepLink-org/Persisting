//! pChronicle：Persisting 的结构化 Agent 轨迹存储与查询层。
//!
//! # 权威边界
//!
//! Canonical Event 是 append-only 运行时事实；[`model::StorylineDocument`] 是与 ATIF v1.7
//! 对齐的交换和规范化查询模型。两者通过单向投影连接，Storyline 不承诺反建原始事件事实。
//! ATIF、ACTF、OpenAI Msg 和 AgenticMD 只经 Storyline 互转；AgenticMD 是 Storyline 的
//! Markdown 编码，不是另一套领域模型。
//!
//! ```text
//! events.lance ──单向投影──► StorylineDocument ──► Storyline 三表 Lance
//!                                  ├──◄──► AgenticMD
//!                                  ├──◄──► ATIF
//!                                  ├──◄──► OpenAI Msg
//!                                  └──◄──► ACTF
//! ```
//!
//! # 公共入口
//!
//! - [`model`]：Storyline、Canonical Event 与 LLM payload 权威类型；
//! - [`document`]：六种磁盘格式、Storyline 语义 codec 与统一读取入口；
//! - [`storage`]：Catalog、Lance store、append、投影和 revision；
//! - [`query`]：DataFusion 查询引擎与能力快照。
//!
//! 外围 wire DTO、低层 parser、Markdown AST、Arrow codec、provider、manifest 与锁均不公开。
//! `search` 是单独的 feature，其既有 API 不属于本次门面收敛范围。

mod agenticmd;
#[cfg(feature = "lance-store")]
mod append_queue;
mod atif;
mod convert;
#[cfg(feature = "lance-store")]
mod discovery;
pub mod document;
mod error;
mod format;
mod formats;
mod input;
mod interop;
mod layout;
#[cfg(feature = "search")]
mod messages;
pub mod model;
#[cfg(feature = "search")]
mod operations;
#[cfg(feature = "lance-store")]
mod projection;
pub mod query;
#[cfg(feature = "lance-store")]
mod revision;
#[cfg(feature = "search")]
pub mod search;
pub mod storage;
mod store;

#[cfg(feature = "lance-store")]
pub(crate) use document::{QueryCapabilities, QueryTables};
pub(crate) use error::{Error, Result};
pub(crate) use format::DocumentFormat;
pub(crate) use formats::storyline::StorylineTurn;
#[cfg(any(feature = "lance-store", test))]
pub(crate) use formats::storyline::{FieldPresence, StoryLink, StorylineToolCall};
#[cfg(feature = "lance-store")]
pub(crate) use formats::storyline::{StorylineAgent, StorylinePresence};
pub(crate) use formats::{EventIdentity, EventRecord, StorylineDocument};
pub(crate) use input::{InputIssue, InputIssueKind, InputResult};
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
#[cfg(feature = "lance-store")]
pub(crate) use store::{
    event_record_to_event_row, event_row_to_event_record, event_rows_from_batch, EventRow,
    TRAJECTORY_AGENT_ID_COL, TRAJECTORY_CALL_ID_COL, TRAJECTORY_COLS, TRAJECTORY_EVENT_ID_COL,
    TRAJECTORY_KIND_COL, TRAJECTORY_MODEL_COL, TRAJECTORY_PARENT_CALL_ID_COL,
    TRAJECTORY_PAYLOAD_JSON_COL, TRAJECTORY_SEQ_COL, TRAJECTORY_SESSION_ID_COL,
    TRAJECTORY_SOURCE_COL, TRAJECTORY_TIMESTAMP_COL, TRAJECTORY_TRACE_ID_COL,
};
#[cfg(feature = "search")]
pub const PERSISTING_VECTOR_INDEX_NAME: &str = search::search_lance::PERSISTING_VECTOR_INDEX_NAME;
#[cfg(feature = "search")]
pub const PERSISTING_FTS_INDEX_NAME: &str = search::search_lance::PERSISTING_FTS_INDEX_NAME;

#[cfg(all(test, feature = "lance-store"))]
mod tests;
