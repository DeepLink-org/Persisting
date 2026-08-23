//! pChronicle 的只读 DataFusion 查询入口与能力快照。

pub type Result<T> = anyhow::Result<T>;

#[cfg(feature = "lance-store")]
pub use crate::document::{FilterPushdown, QueryCapabilities, QueryTables};
#[cfg(feature = "lance-store")]
pub use crate::store::{
    ChronicleQueryEngine, ChronicleQueryExecutionOptions, ExternalTableFormat, ExternalTableSpec,
    FileTrajectoryQueryMetricsSnapshot, IntrospectedField, IntrospectedTable, QueryBackendInfo,
    QuerySnapshot, QueryWriteOutcome,
    CATALOG_SOURCES_TABLE, CATALOG_TRAJECTORIES_TABLE, DATAFUSION_EVENTS_TABLE,
    DATAFUSION_RUNS_TABLE, DATAFUSION_STEPS_TABLE, DATAFUSION_TOOL_CALLS_TABLE, SOURCE_FILE_COLUMN,
};
