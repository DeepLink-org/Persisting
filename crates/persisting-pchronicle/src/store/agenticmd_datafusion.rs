//! Storyline-schema DataFusion provider for one AgenticMD document.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use datafusion::catalog::Session;
use datafusion::datasource::{MemTable, TableProvider};
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown, TableType};
use datafusion::physical_plan::ExecutionPlan;
use datafusion::prelude::SessionContext;
use lance::deps::arrow_schema::SchemaRef;

use crate::formats::StorylineDocument;
use crate::store::split_storyline;

use super::{
    datafusion_bridge::from_datafusion, story_runs_arrow_schema, story_runs_to_batch,
    story_steps_arrow_schema, story_steps_to_batch, story_tool_calls_arrow_schema,
    story_tool_calls_to_batch, StorylineDataFusionTableNames,
};

#[derive(Debug)]
pub(crate) struct AgenticMdDataSource {
    runs: Arc<dyn TableProvider>,
    steps: Arc<dyn TableProvider>,
    tool_calls: Arc<dyn TableProvider>,
}

impl AgenticMdDataSource {
    pub(crate) fn new(story: &StorylineDocument) -> Result<Self> {
        let tables = split_storyline(story)?;
        Ok(Self {
            runs: unsupported_filter_table(
                story_runs_arrow_schema(),
                story_runs_to_batch(std::slice::from_ref(&tables.run))?,
            )?,
            steps: unsupported_filter_table(
                story_steps_arrow_schema(),
                story_steps_to_batch(&tables.steps)?,
            )?,
            tool_calls: unsupported_filter_table(
                story_tool_calls_arrow_schema(),
                story_tool_calls_to_batch(&tables.tool_calls)?,
            )?,
        })
    }

    pub(crate) fn register(&self, context: &SessionContext) -> Result<()> {
        let names = StorylineDataFusionTableNames::default();
        context
            .register_table(&names.runs, self.runs.clone())
            .map_err(|error| from_datafusion("register AgenticMD runs table", error))?;
        context
            .register_table(&names.steps, self.steps.clone())
            .map_err(|error| from_datafusion("register AgenticMD steps table", error))?;
        context
            .register_table(&names.tool_calls, self.tool_calls.clone())
            .map_err(|error| from_datafusion("register AgenticMD tool_calls table", error))?;
        Ok(())
    }
}

fn unsupported_filter_table(
    schema: SchemaRef,
    batch: lance::deps::arrow_array::RecordBatch,
) -> Result<Arc<dyn TableProvider>> {
    let table = MemTable::try_new(schema, vec![vec![batch]])?;
    Ok(Arc::new(AgenticMdTableProvider {
        inner: Arc::new(table),
    }))
}

#[derive(Debug)]
struct AgenticMdTableProvider {
    inner: Arc<MemTable>,
}

#[async_trait]
impl TableProvider for AgenticMdTableProvider {
    fn schema(&self) -> SchemaRef {
        self.inner.schema()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        _filters: &[Expr],
        _limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        self.inner.scan(state, projection, &[], None).await
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> datafusion::common::Result<Vec<TableProviderFilterPushDown>> {
        Ok(vec![
            TableProviderFilterPushDown::Unsupported;
            filters.len()
        ])
    }
}
