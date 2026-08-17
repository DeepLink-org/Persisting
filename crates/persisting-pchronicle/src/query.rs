//! pChronicle 的只读查询入口。

pub use crate::error::{classify_error, Error, ErrorCode, Result};

#[cfg(feature = "lance-store")]
pub use crate::document::{FilterPushdown, QueryCapabilities, QueryTables};
#[cfg(feature = "lance-store")]
pub use crate::store::{
    ChronicleQueryEngine, ChronicleQueryExecutionOptions, ExternalTableFormat, ExternalTableSpec,
    FileTrajectoryDataSource, FileTrajectoryDataSourceOptions, FileTrajectoryFormat,
    FileTrajectoryQueryMetrics, FileTrajectoryQueryMetricsSnapshot, QueryBackendInfo,
    QuerySnapshot, RawEventDataSource, RawEventDataSourceOptions, RawEventTableProvider,
    StorylineDataFusionTableNames, StorylineDataSource, StorylineDataSourceOptions,
    StorylineTableKind, StorylineTableProvider, CATALOG_SOURCES_TABLE, CATALOG_TRAJECTORIES_TABLE,
    DATAFUSION_EVENTS_TABLE, DATAFUSION_RUNS_TABLE, DATAFUSION_STEPS_TABLE,
    DATAFUSION_TOOL_CALLS_TABLE, SOURCE_FILE_COLUMN,
};
