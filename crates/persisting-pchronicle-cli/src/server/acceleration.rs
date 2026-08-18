//! Generation-scoped, rebuildable query acceleration for the Web server.
//!
//! The persistent Catalog remains the source of truth. This module derives an
//! in-memory value-to-source routing index from one immutable Catalog snapshot
//! and only uses it to add conservative `_file_` predicates. Unsupported SQL,
//! failed index builds, and non-selective lookups retain the original query.
//! Each cell records one terminal build result for its owning Catalog generation;
//! the status `*_failed` and `*_unavailable` booleans distinguish that result
//! from a cell that has not been requested yet without exposing diagnostics.

use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::hash::BuildHasher;
use std::sync::Arc;

use anyhow::{Context, Result};
use datafusion::arrow::array::{Array, StringArray};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::sql::sqlparser::ast::{
    BinaryOperator, Expr, Ident, ObjectName, Statement, TableFactor, Value,
};
use datafusion::sql::sqlparser::dialect::GenericDialect;
use datafusion::sql::sqlparser::parser::Parser;
use futures::TryStreamExt;
use persisting_pchronicle::query::ChronicleQueryEngine;
use persisting_pchronicle::storage::DatasetCatalogSnapshot;
use serde::Serialize;
use serde_json::Value as JsonValue;
use tokio::sync::{Mutex, OnceCell};

use super::RunSummary;

const MAX_INJECTED_SOURCES: usize = 512;
const MAX_ROUTING_INDEX_ROWS: usize = 1_000_000;
const MAX_ROUTING_INDEX_VALUES: usize = 1_000_000;
const RUN_COLUMNS: &[&str] = &["run_id", "session_id", "agent_id", "agent_model_name"];
const STEP_COLUMNS: &[&str] = &["run_id", "session_id"];
const EVENT_IDENTITY_COLUMNS: &[&str] = &["event_id", "trace_id"];
const EVENT_PARTITION_COLUMNS: &[&str] = &["session_id", "agent_id"];

#[derive(Debug, Default)]
pub(crate) struct ServerAcceleration {
    build_gate: Mutex<()>,
    run_summaries: OnceCell<RequiredAcceleration<CachedRunSummaries>>,
    runs: OnceCell<OptionalAcceleration<CachedRoutingIndex>>,
    event_identities: OnceCell<OptionalAcceleration<CachedRoutingIndex>>,
    event_partitions: OnceCell<OptionalAcceleration<CachedRoutingIndex>>,
}

type CachedRunSummaries = Arc<Vec<RunSummary>>;
type CachedRoutingIndex = Arc<SourceRoutingIndex>;

#[derive(Debug)]
enum RequiredAcceleration<T> {
    Ready(T),
    Failed(SharedAccelerationFailure),
}

#[derive(Debug)]
enum OptionalAcceleration<T> {
    Ready(T),
    Unavailable,
}

#[derive(Clone, Debug)]
struct SharedAccelerationFailure(Arc<dyn std::error::Error + Send + Sync>);

impl SharedAccelerationFailure {
    fn new(error: anyhow::Error) -> Self {
        Self(Arc::from(error.into_boxed_dyn_error()))
    }
}

impl std::fmt::Display for SharedAccelerationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("cached acceleration build failure")
    }
}

impl std::error::Error for SharedAccelerationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct AccelerationStatus {
    pub(crate) run_summaries_ready: bool,
    /// The generation's required summary build reached a terminal failure.
    pub(crate) run_summaries_failed: bool,
    pub(crate) run_index: Option<RoutingIndexStatus>,
    /// The generation's optional run index reached a terminal unavailable state.
    pub(crate) run_index_unavailable: bool,
    pub(crate) event_identity_index: Option<RoutingIndexStatus>,
    /// The generation's optional identity index reached a terminal unavailable state.
    pub(crate) event_identity_index_unavailable: bool,
    pub(crate) event_partition_index: Option<RoutingIndexStatus>,
    /// The generation's optional partition index reached a terminal unavailable state.
    pub(crate) event_partition_index_unavailable: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct RoutingIndexStatus {
    pub(crate) rows: usize,
    pub(crate) sources: usize,
    pub(crate) values: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoutingOutcome {
    Applied,
    AlreadyPruned,
    NotApplicable,
    NotSelective,
    IndexUnavailable,
}

impl RoutingOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::AlreadyPruned => "already_pruned",
            Self::NotApplicable => "not_applicable",
            Self::NotSelective => "not_selective",
            Self::IndexUnavailable => "index_unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoutedQuery {
    pub(crate) sql: String,
    pub(crate) outcome: RoutingOutcome,
    pub(crate) candidate_sources: Option<usize>,
}

impl RoutedQuery {
    fn unchanged(sql: &str, outcome: RoutingOutcome) -> Self {
        Self {
            sql: sql.to_string(),
            outcome,
            candidate_sources: None,
        }
    }
}

impl ServerAcceleration {
    pub(crate) fn status(&self) -> AccelerationStatus {
        let (run_summaries_ready, run_summaries_failed) =
            required_acceleration_status(&self.run_summaries);
        let (run_index, run_index_unavailable) = cached_index_status(&self.runs);
        let (event_identity_index, event_identity_index_unavailable) =
            cached_index_status(&self.event_identities);
        let (event_partition_index, event_partition_index_unavailable) =
            cached_index_status(&self.event_partitions);
        AccelerationStatus {
            run_summaries_ready,
            run_summaries_failed,
            run_index,
            run_index_unavailable,
            event_identity_index,
            event_identity_index_unavailable,
            event_partition_index,
            event_partition_index_unavailable,
        }
    }

    pub(crate) async fn run_summaries(
        &self,
        snapshot: &DatasetCatalogSnapshot,
        engine: &ChronicleQueryEngine,
    ) -> Result<Arc<Vec<RunSummary>>> {
        self.run_summaries_with(|| async { build_run_summaries(snapshot, engine).await })
            .await
    }

    async fn run_summaries_with<F, Fut>(&self, build: F) -> Result<CachedRunSummaries>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<CachedRunSummaries>>,
    {
        match self
            .run_summaries
            .get_or_init(|| async {
                let _admission = self.build_gate.lock().await;
                match build().await {
                    Ok(summaries) => RequiredAcceleration::Ready(summaries),
                    Err(error) => {
                        RequiredAcceleration::Failed(SharedAccelerationFailure::new(error))
                    }
                }
            })
            .await
        {
            RequiredAcceleration::Ready(summaries) => Ok(summaries.clone()),
            RequiredAcceleration::Failed(error) => Err(anyhow::Error::new(error.clone())),
        }
    }

    pub(crate) async fn route_sql(
        &self,
        snapshot: &DatasetCatalogSnapshot,
        engine: &ChronicleQueryEngine,
        sql: &str,
    ) -> RoutedQuery {
        self.route_sql_with(snapshot, sql, |kind| async move {
            match kind {
                RoutingIndexKind::Runs => build_run_index(snapshot, engine).await,
                RoutingIndexKind::EventIdentities => {
                    build_event_identity_index(snapshot, engine).await
                }
                RoutingIndexKind::EventPartitions => {
                    build_event_partition_index(snapshot, engine).await
                }
            }
        })
        .await
    }

    async fn route_sql_with<F, Fut>(
        &self,
        snapshot: &DatasetCatalogSnapshot,
        sql: &str,
        build: F,
    ) -> RoutedQuery
    where
        F: FnOnce(RoutingIndexKind) -> Fut,
        Fut: Future<Output = Result<CachedRoutingIndex>>,
    {
        let Some(mut query) = AnalyzedQuery::parse(snapshot, sql) else {
            return RoutedQuery::unchanged(sql, RoutingOutcome::NotApplicable);
        };
        if query.already_pruned {
            return RoutedQuery::unchanged(sql, RoutingOutcome::AlreadyPruned);
        }
        if query.constraints.is_empty() {
            return RoutedQuery::unchanged(sql, RoutingOutcome::NotApplicable);
        }

        let (cell, name) = match query.index_kind {
            RoutingIndexKind::Runs => (&self.runs, "run"),
            RoutingIndexKind::EventIdentities => (&self.event_identities, "event_identity"),
            RoutingIndexKind::EventPartitions => (&self.event_partitions, "event_partition"),
        };
        let Some(index) = self
            .optional_index_with(cell, name, || build(query.index_kind))
            .await
        else {
            return RoutedQuery::unchanged(sql, RoutingOutcome::IndexUnavailable);
        };

        let Some(candidates) = index.candidates(&query.dataset, &query.constraints) else {
            return RoutedQuery::unchanged(sql, RoutingOutcome::NotApplicable);
        };
        let source_count = index.source_count(&query.dataset);
        if candidates.len() > MAX_INJECTED_SOURCES
            || (!candidates.is_empty() && candidates.len() >= source_count)
        {
            return RoutedQuery::unchanged(sql, RoutingOutcome::NotSelective);
        }

        let candidate_sources = candidates.len();
        query.inject_source_predicate(candidates);
        RoutedQuery {
            sql: query.statement.to_string(),
            outcome: RoutingOutcome::Applied,
            candidate_sources: Some(candidate_sources),
        }
    }

    async fn optional_index_with<F, Fut>(
        &self,
        cell: &OnceCell<OptionalAcceleration<CachedRoutingIndex>>,
        name: &'static str,
        build: F,
    ) -> Option<CachedRoutingIndex>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<CachedRoutingIndex>>,
    {
        match cell
            .get_or_init(|| async {
                let _admission = self.build_gate.lock().await;
                match build().await {
                    Ok(index) => OptionalAcceleration::Ready(index),
                    Err(error) => {
                        tracing::error!(
                            error = ?error,
                            acceleration_index = name,
                            "pChronicle acceleration index build failed"
                        );
                        OptionalAcceleration::Unavailable
                    }
                }
            })
            .await
        {
            OptionalAcceleration::Ready(index) => Some(index.clone()),
            OptionalAcceleration::Unavailable => None,
        }
    }
}

fn required_acceleration_status<T>(cell: &OnceCell<RequiredAcceleration<T>>) -> (bool, bool) {
    match cell.get() {
        None => (false, false),
        Some(RequiredAcceleration::Ready(_)) => (true, false),
        Some(RequiredAcceleration::Failed(_)) => (false, true),
    }
}

fn cached_index_status(
    cell: &OnceCell<OptionalAcceleration<CachedRoutingIndex>>,
) -> (Option<RoutingIndexStatus>, bool) {
    match cell.get() {
        None => (None, false),
        Some(OptionalAcceleration::Ready(index)) => (Some(index.status()), false),
        Some(OptionalAcceleration::Unavailable) => (None, true),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoutingIndexKind {
    Runs,
    EventIdentities,
    EventPartitions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RoutingConstraint {
    column: String,
    values: Vec<String>,
}

struct AnalyzedQuery {
    statement: Statement,
    dataset: String,
    index_kind: RoutingIndexKind,
    constraints: Vec<RoutingConstraint>,
    qualifier: Ident,
    already_pruned: bool,
}

impl AnalyzedQuery {
    fn parse(snapshot: &DatasetCatalogSnapshot, sql: &str) -> Option<Self> {
        let mut statements = Parser::parse_sql(&GenericDialect, sql).ok()?;
        if statements.len() != 1 {
            return None;
        }
        let statement = statements.pop()?;
        let Statement::Query(query) = &statement else {
            return None;
        };
        if query.with.is_some() {
            return None;
        }
        let select = query.body.as_select()?;
        if select.from.len() != 1 || !select.from[0].joins.is_empty() {
            return None;
        }
        let TableFactor::Table {
            name,
            alias,
            args,
            with_hints,
            version,
            with_ordinality,
            partitions,
            json_path,
            sample,
            index_hints,
            ..
        } = &select.from[0].relation
        else {
            return None;
        };
        if args.is_some()
            || !with_hints.is_empty()
            || version.is_some()
            || *with_ordinality
            || !partitions.is_empty()
            || json_path.is_some()
            || sample.is_some()
            || !index_hints.is_empty()
            || alias
                .as_ref()
                .is_some_and(|alias| !alias.columns.is_empty())
        {
            return None;
        }
        let (dataset, table) = resolve_table(snapshot, name)?;
        let indexed_columns = match table.as_str() {
            "events" => [EVENT_IDENTITY_COLUMNS, EVENT_PARTITION_COLUMNS].concat(),
            "runs" | "trajectories" => RUN_COLUMNS.to_vec(),
            "steps" | "tool_calls" => STEP_COLUMNS.to_vec(),
            _ => return None,
        };
        let qualifier = alias
            .as_ref()
            .map(|alias| alias.name.clone())
            .or_else(|| name.0.last().and_then(|part| part.as_ident()).cloned())?;
        let already_pruned = select
            .selection
            .as_ref()
            .is_some_and(contains_file_predicate);
        let mut constraints = Vec::new();
        if let Some(selection) = &select.selection {
            collect_required_constraints(selection, &mut constraints);
        }
        constraints.retain(|constraint| indexed_columns.contains(&constraint.column.as_str()));
        let index_kind = match table.as_str() {
            "events"
                if constraints.iter().any(|constraint| {
                    EVENT_IDENTITY_COLUMNS.contains(&constraint.column.as_str())
                }) =>
            {
                RoutingIndexKind::EventIdentities
            }
            "events" => RoutingIndexKind::EventPartitions,
            _ => RoutingIndexKind::Runs,
        };
        let selected_columns = match index_kind {
            RoutingIndexKind::Runs => indexed_columns.as_slice(),
            RoutingIndexKind::EventIdentities => EVENT_IDENTITY_COLUMNS,
            RoutingIndexKind::EventPartitions => EVENT_PARTITION_COLUMNS,
        };
        constraints.retain(|constraint| selected_columns.contains(&constraint.column.as_str()));
        Some(Self {
            statement,
            dataset,
            index_kind,
            constraints,
            qualifier,
            already_pruned,
        })
    }

    fn inject_source_predicate(&mut self, files: Vec<String>) {
        let Statement::Query(query) = &mut self.statement else {
            unreachable!("analyzed statement is a query")
        };
        let select = query
            .body
            .as_mut()
            .as_select_mut()
            .expect("analyzed query body is a SELECT");
        let source_column =
            Expr::CompoundIdentifier(vec![self.qualifier.clone(), Ident::new("_file_")]);
        let source_predicate = match files.as_slice() {
            [] => Expr::Value(Value::Boolean(false).into()),
            [file] => Expr::BinaryOp {
                left: Box::new(source_column),
                op: BinaryOperator::Eq,
                right: Box::new(string_literal(file)),
            },
            files => Expr::InList {
                expr: Box::new(source_column),
                list: files.iter().map(|file| string_literal(file)).collect(),
                negated: false,
            },
        };
        select.selection = Some(match select.selection.take() {
            Some(selection) => Expr::BinaryOp {
                left: Box::new(Expr::Nested(Box::new(selection))),
                op: BinaryOperator::And,
                right: Box::new(source_predicate),
            },
            None => source_predicate,
        });
    }
}

trait SetExprExt {
    fn as_select_mut(&mut self) -> Option<&mut datafusion::sql::sqlparser::ast::Select>;
}

impl SetExprExt for datafusion::sql::sqlparser::ast::SetExpr {
    fn as_select_mut(&mut self) -> Option<&mut datafusion::sql::sqlparser::ast::Select> {
        match self {
            Self::Select(select) => Some(select),
            _ => None,
        }
    }
}

fn string_literal(value: &str) -> Expr {
    Expr::Value(Value::SingleQuotedString(value.to_string()).into())
}

fn resolve_table(snapshot: &DatasetCatalogSnapshot, name: &ObjectName) -> Option<(String, String)> {
    let parts = name
        .0
        .iter()
        .map(|part| {
            part.as_ident()
                .map(|ident| ident.value.to_ascii_lowercase())
        })
        .collect::<Option<Vec<_>>>()?;
    let (dataset, table) = match parts.as_slice() {
        [table] => (snapshot.default_dataset()?.to_string(), table.clone()),
        [dataset, table] => (dataset.clone(), table.clone()),
        _ => return None,
    };
    snapshot.dataset(&dataset)?;
    Some((dataset, table))
}

fn collect_required_constraints(expr: &Expr, output: &mut Vec<RoutingConstraint>) {
    match expr {
        Expr::Nested(inner) => collect_required_constraints(inner, output),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::And,
            right,
        } => {
            collect_required_constraints(left, output);
            collect_required_constraints(right, output);
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Eq,
            right,
        } => {
            if let Some((column, value)) =
                equality_constraint(left, right).or_else(|| equality_constraint(right, left))
            {
                output.push(RoutingConstraint {
                    column,
                    values: vec![value],
                });
            }
        }
        Expr::InList {
            expr,
            list,
            negated: false,
        } => {
            let Some(column) = column_name(expr) else {
                return;
            };
            let values = list.iter().map(string_value).collect::<Option<Vec<_>>>();
            if let Some(values) = values.filter(|values| !values.is_empty()) {
                output.push(RoutingConstraint { column, values });
            }
        }
        _ => {}
    }
}

fn equality_constraint(column: &Expr, value: &Expr) -> Option<(String, String)> {
    Some((column_name(column)?, string_value(value)?))
}

fn column_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Identifier(ident) => Some(ident.value.to_ascii_lowercase()),
        Expr::CompoundIdentifier(parts) => {
            parts.last().map(|ident| ident.value.to_ascii_lowercase())
        }
        Expr::Nested(inner) => column_name(inner),
        _ => None,
    }
}

fn string_value(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Value(value) => match &value.value {
            Value::SingleQuotedString(value) => Some(value.clone()),
            _ => None,
        },
        Expr::Nested(inner) => string_value(inner),
        _ => None,
    }
}

fn contains_file_predicate(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => contains_file_predicate(inner),
        Expr::BinaryOp { left, op, right } => {
            if matches!(
                op,
                BinaryOperator::Eq | BinaryOperator::And | BinaryOperator::Or
            ) {
                (column_name(left).as_deref() == Some("_file_")
                    || column_name(right).as_deref() == Some("_file_"))
                    || contains_file_predicate(left)
                    || contains_file_predicate(right)
            } else {
                false
            }
        }
        Expr::InList { expr, .. } | Expr::Like { expr, .. } | Expr::ILike { expr, .. } => {
            column_name(expr).as_deref() == Some("_file_")
        }
        _ => false,
    }
}

#[derive(Debug, Default)]
struct SourceRoutingIndex {
    datasets: BTreeMap<String, DatasetRoutingIndex>,
    fingerprint_state: RandomState,
    rows: usize,
}

impl SourceRoutingIndex {
    fn status(&self) -> RoutingIndexStatus {
        RoutingIndexStatus {
            rows: self.rows,
            sources: self
                .datasets
                .values()
                .map(|dataset| dataset.sources.len())
                .sum(),
            values: self
                .datasets
                .values()
                .flat_map(|dataset| dataset.columns.values())
                .map(HashMap::len)
                .sum(),
        }
    }

    fn source_count(&self, dataset: &str) -> usize {
        self.datasets
            .get(dataset)
            .map_or(0, |index| index.sources.len())
    }

    fn candidates(&self, dataset: &str, constraints: &[RoutingConstraint]) -> Option<Vec<String>> {
        let index = self.datasets.get(dataset)?;
        let mut candidates: Option<BTreeSet<u32>> = None;
        let mut used_constraint = false;
        for constraint in constraints {
            let Some(values) = index.columns.get(constraint.column.as_str()) else {
                continue;
            };
            used_constraint = true;
            let matching = constraint
                .values
                .iter()
                .filter_map(|value| values.get(&value_fingerprint(&self.fingerprint_state, value)))
                .fold(BTreeSet::new(), |mut matching, sources| {
                    sources.extend_into(&mut matching);
                    matching
                });
            candidates = Some(match candidates {
                None => matching,
                Some(current) => current.intersection(&matching).copied().collect(),
            });
        }
        if !used_constraint {
            return None;
        }
        let mut files = candidates
            .unwrap_or_default()
            .into_iter()
            .filter_map(|source| index.sources.get(source as usize).cloned())
            .collect::<Vec<_>>();
        files.sort();
        Some(files)
    }
}

#[derive(Debug, Default)]
struct DatasetRoutingIndex {
    sources: Vec<String>,
    columns: BTreeMap<&'static str, HashMap<u64, SourceIds>>,
}

#[derive(Debug)]
enum SourceIds {
    One(u32),
    Many(Box<[u32]>),
}

impl SourceIds {
    fn extend_into(&self, output: &mut BTreeSet<u32>) {
        match self {
            Self::One(source) => {
                output.insert(*source);
            }
            Self::Many(sources) => output.extend(sources.iter().copied()),
        }
    }
}

#[derive(Debug)]
struct SourceRoutingIndexBuilder {
    datasets: BTreeMap<String, DatasetRoutingIndexBuilder>,
    columns: &'static [&'static str],
    fingerprint_state: RandomState,
    rows: usize,
}

impl SourceRoutingIndexBuilder {
    fn new(columns: &'static [&'static str]) -> Self {
        Self {
            datasets: BTreeMap::new(),
            columns,
            fingerprint_state: RandomState::new(),
            rows: 0,
        }
    }

    fn add<'a, I>(&mut self, dataset: &str, file: &str, values: I)
    where
        I: IntoIterator<Item = (&'static str, Option<&'a str>)>,
    {
        self.rows += 1;
        self.datasets
            .entry(dataset.to_string())
            .or_insert_with(|| {
                DatasetRoutingIndexBuilder::new(self.columns, self.fingerprint_state.clone())
            })
            .add(file, values);
    }

    fn ensure_dataset(&mut self, dataset: &str) {
        self.datasets.entry(dataset.to_string()).or_insert_with(|| {
            DatasetRoutingIndexBuilder::new(self.columns, self.fingerprint_state.clone())
        });
    }

    fn finish(self) -> SourceRoutingIndex {
        SourceRoutingIndex {
            datasets: self
                .datasets
                .into_iter()
                .map(|(dataset, index)| (dataset, index.finish()))
                .collect(),
            fingerprint_state: self.fingerprint_state,
            rows: self.rows,
        }
    }

    fn ensure_within_limits(&self) -> Result<()> {
        self.ensure_limits(MAX_ROUTING_INDEX_ROWS, MAX_ROUTING_INDEX_VALUES)
    }

    fn ensure_limits(&self, max_rows: usize, max_values: usize) -> Result<()> {
        anyhow::ensure!(
            self.rows <= max_rows,
            "server routing index exceeds row limit of {max_rows}"
        );
        let values = self
            .datasets
            .values()
            .flat_map(|dataset| dataset.columns.values())
            .map(HashMap::len)
            .sum::<usize>();
        anyhow::ensure!(
            values <= max_values,
            "server routing index exceeds distinct-value limit of {max_values}"
        );
        Ok(())
    }
}

#[derive(Debug)]
struct DatasetRoutingIndexBuilder {
    sources: Vec<String>,
    source_ids: HashMap<String, u32>,
    columns: BTreeMap<&'static str, HashMap<u64, PendingSourceIds>>,
    fingerprint_state: RandomState,
}

#[derive(Debug)]
enum PendingSourceIds {
    One(u32),
    Many(Vec<u32>),
}

impl PendingSourceIds {
    fn push(&mut self, source: u32) {
        match self {
            Self::One(existing) if *existing == source => {}
            Self::One(existing) => {
                *self = Self::Many(vec![*existing, source]);
            }
            Self::Many(sources) if sources.last().copied() == Some(source) => {}
            Self::Many(sources) => sources.push(source),
        }
    }

    fn finish(self) -> SourceIds {
        match self {
            Self::One(source) => SourceIds::One(source),
            Self::Many(mut sources) => {
                sources.sort_unstable();
                sources.dedup();
                match sources.as_slice() {
                    [source] => SourceIds::One(*source),
                    _ => SourceIds::Many(sources.into_boxed_slice()),
                }
            }
        }
    }
}

impl DatasetRoutingIndexBuilder {
    fn new(columns: &'static [&'static str], fingerprint_state: RandomState) -> Self {
        Self {
            sources: Vec::new(),
            source_ids: HashMap::new(),
            columns: columns
                .iter()
                .copied()
                .map(|column| (column, HashMap::new()))
                .collect(),
            fingerprint_state,
        }
    }

    fn add<'a, I>(&mut self, file: &str, values: I)
    where
        I: IntoIterator<Item = (&'static str, Option<&'a str>)>,
    {
        let source = if let Some(source) = self.source_ids.get(file) {
            *source
        } else {
            let source = self.sources.len() as u32;
            self.sources.push(file.to_string());
            self.source_ids.insert(file.to_string(), source);
            source
        };
        for (column, value) in values {
            let Some(value) = value else {
                continue;
            };
            if let Some(index) = self.columns.get_mut(column) {
                index
                    .entry(value_fingerprint(&self.fingerprint_state, value))
                    .and_modify(|sources| sources.push(source))
                    .or_insert(PendingSourceIds::One(source));
            }
        }
    }

    fn finish(self) -> DatasetRoutingIndex {
        DatasetRoutingIndex {
            sources: self.sources,
            columns: self
                .columns
                .into_iter()
                .map(|(column, values)| {
                    (
                        column,
                        values
                            .into_iter()
                            .map(|(value, sources)| (value, sources.finish()))
                            .collect(),
                    )
                })
                .collect(),
        }
    }
}

fn value_fingerprint(state: &RandomState, value: &str) -> u64 {
    state.hash_one(value)
}

async fn build_run_summaries(
    snapshot: &DatasetCatalogSnapshot,
    engine: &ChronicleQueryEngine,
) -> Result<Arc<Vec<RunSummary>>> {
    let mut summaries = Vec::new();
    for dataset in snapshot.datasets() {
        let name = &dataset.mount.name;
        let event_stats = build_event_stats(engine, name).await?;
        let sql = format!(
            "SELECT r._file_, r.document_id, r.run_id, r.session_id, r.agent_id, r.agent_model_name, \
                    r.parent_json, r.final_metrics_json, r.extra_json, \
                    (SELECT COUNT(*) FROM {name}.steps s \
                      WHERE s._file_ = r._file_ AND s.document_id = r.document_id) AS row_count \
             FROM {name}.runs r"
        );
        let body = engine.query_jsonl(&sql).await?;
        for line in body.lines().filter(|line| !line.trim().is_empty()) {
            let row: JsonValue = serde_json::from_str(line).context("decode run index row")?;
            let file = required_json_string(&row, "_file_")?.to_string();
            let document_id = required_json_string(&row, "document_id")?.to_string();
            let run_id = row
                .get("run_id")
                .and_then(JsonValue::as_str)
                .map(str::to_owned);
            let session_id = required_json_string(&row, "session_id")?.to_string();
            let agent_id = required_json_string(&row, "agent_id")?.to_string();
            let model_name = row
                .get("agent_model_name")
                .and_then(JsonValue::as_str)
                .map(str::to_owned);
            let parent_session_id = row
                .get("parent_json")
                .and_then(JsonValue::as_str)
                .and_then(|parent| serde_json::from_str::<JsonValue>(parent).ok())
                .and_then(|parent| {
                    parent
                        .get("psid")
                        .or_else(|| parent.get("parent_session_id"))
                        .and_then(JsonValue::as_str)
                        .map(str::to_owned)
                });
            let root_session_id = parent_session_id
                .clone()
                .or_else(|| run_id.as_ref().filter(|id| *id != &session_id).cloned());
            let path = if file == "." {
                match root_session_id.as_deref() {
                    Some(root) if root != session_id => {
                        format!("{name}/{root}/subagents/{session_id}")
                    }
                    Some(root) => format!("{name}/{root}"),
                    None => format!("{name}/{session_id}"),
                }
            } else {
                match root_session_id.as_deref() {
                    Some(root) if root != session_id => {
                        format!("{name}/{file}/{root}/{session_id}")
                    }
                    Some(root) => format!("{name}/{file}/{root}"),
                    None => format!("{name}/{file}/{session_id}"),
                }
            };
            let status = event_stats
                .get(&(file.clone(), session_id.clone()))
                .map_or_else(
                    || run_status(&row),
                    |stats| stats.status.clone().unwrap_or_else(|| "active".into()),
                );
            let event_stats = event_stats.get(&(file.clone(), session_id.clone()));
            summaries.push(RunSummary {
                dataset: name.clone(),
                file,
                document_id,
                run_id,
                agent_id,
                model_name,
                session_id,
                root_session_id,
                path,
                row_count: event_stats.map_or_else(
                    || {
                        row.get("row_count")
                            .and_then(JsonValue::as_u64)
                            .unwrap_or(0) as usize
                    },
                    |stats| stats.row_count,
                ),
                duplicate_event_ids: event_stats.map_or(0, |stats| stats.duplicate_event_ids),
                status,
            });
        }
    }
    summaries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(Arc::new(summaries))
}

#[derive(Debug, Clone)]
struct EventStats {
    row_count: usize,
    duplicate_event_ids: usize,
    status: Option<String>,
}

async fn build_event_stats(
    engine: &ChronicleQueryEngine,
    dataset: &str,
) -> Result<HashMap<(String, String), EventStats>> {
    let sql = format!(
        "SELECT _file_, session_id, COUNT(*) AS row_count, \
                COUNT(event_id) - COUNT(DISTINCT event_id) AS duplicate_event_ids, \
                MAX(CASE \
                    WHEN kind = 'run.failed' OR kind = 'run.cancelled' THEN 3 \
                    WHEN kind = 'run.completed' THEN 2 \
                    WHEN kind = 'session.ended' THEN 1 \
                    ELSE 0 END) AS terminal_rank, \
                MAX(CASE WHEN kind = 'session.ended' THEN payload_json ELSE NULL END) \
                    AS session_ended_payload_json \
         FROM {dataset}.events GROUP BY _file_, session_id"
    );
    let body = engine.query_jsonl(&sql).await?;
    body.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let row: JsonValue = serde_json::from_str(line).context("decode event stats row")?;
            let file = required_json_string(&row, "_file_")?.to_string();
            let session_id = required_json_string(&row, "session_id")?.to_string();
            let row_count = required_json_u64(&row, "row_count")? as usize;
            let duplicate_event_ids = required_json_u64(&row, "duplicate_event_ids")? as usize;
            let terminal_rank = required_json_u64(&row, "terminal_rank")?;
            let status = terminal_event_status(
                terminal_rank,
                row.get("session_ended_payload_json")
                    .and_then(JsonValue::as_str),
            );
            Ok((
                (file, session_id),
                EventStats {
                    row_count,
                    duplicate_event_ids,
                    status,
                },
            ))
        })
        .collect()
}

fn terminal_event_status(rank: u64, session_ended_payload_json: Option<&str>) -> Option<String> {
    match rank {
        3.. => Some("failed".into()),
        2 => Some("completed".into()),
        1 => {
            let exit_code = session_ended_payload_json
                .and_then(|raw| serde_json::from_str::<JsonValue>(raw).ok())
                .and_then(|event| event.get("payload").cloned())
                .and_then(|payload| payload.get("exit_code").and_then(JsonValue::as_i64));
            Some(if exit_code.is_some_and(|code| code != 0) {
                "failed".into()
            } else {
                "completed".into()
            })
        }
        _ => None,
    }
}

fn run_status(row: &JsonValue) -> String {
    let values = ["final_metrics_json", "extra_json"]
        .into_iter()
        .filter_map(|field| parsed_json_field(row, field))
        .collect::<Vec<_>>();
    if let Some(status) = ["status", "state"].into_iter().find_map(|field| {
        values
            .iter()
            .find_map(|value| find_json_string(value, field))
    }) {
        return normalize_run_status(status);
    }
    if values.iter().any(|value| {
        find_json_bool(value, "is_session_completed") == Some(true)
            || find_json_bool(value, "is_terminal") == Some(true)
    }) {
        return "completed".into();
    }
    "active".into()
}

fn parsed_json_field(row: &JsonValue, field: &str) -> Option<JsonValue> {
    match row.get(field)? {
        JsonValue::String(value) => serde_json::from_str(value).ok(),
        value if !value.is_null() => Some(value.clone()),
        _ => None,
    }
}

fn find_json_string<'a>(value: &'a JsonValue, field: &str) -> Option<&'a str> {
    match value {
        JsonValue::Object(map) => map.get(field).and_then(JsonValue::as_str).or_else(|| {
            map.values()
                .find_map(|value| find_json_string(value, field))
        }),
        JsonValue::Array(values) => values
            .iter()
            .find_map(|value| find_json_string(value, field)),
        _ => None,
    }
}

fn find_json_bool(value: &JsonValue, field: &str) -> Option<bool> {
    match value {
        JsonValue::Object(map) => map
            .get(field)
            .and_then(JsonValue::as_bool)
            .or_else(|| map.values().find_map(|value| find_json_bool(value, field))),
        JsonValue::Array(values) => values.iter().find_map(|value| find_json_bool(value, field)),
        _ => None,
    }
}

fn normalize_run_status(status: &str) -> String {
    match status.trim().to_ascii_lowercase().as_str() {
        "complete" | "completed" | "ok" | "success" | "succeeded" => "completed".into(),
        "cancelled" | "canceled" | "error" | "failed" | "failure" => "failed".into(),
        _ => "active".into(),
    }
}

#[cfg(test)]
mod run_summary_tests {
    use super::*;

    #[test]
    fn run_status_uses_normalized_terminal_metadata() {
        let completed = serde_json::json!({
            "final_metrics_json": "{\"is_session_completed\":true}"
        });
        assert_eq!(run_status(&completed), "completed");

        let failed = serde_json::json!({
            "final_metrics_json": "{\"status\":\"failed\"}"
        });
        assert_eq!(run_status(&failed), "failed");

        assert_eq!(run_status(&serde_json::json!({})), "active");
    }

    #[test]
    fn session_ended_status_honors_nonzero_exit_codes() {
        let failed = serde_json::json!({"payload": {"exit_code": 7}}).to_string();
        let completed = serde_json::json!({"payload": {"exit_code": 0}}).to_string();

        assert_eq!(
            terminal_event_status(1, Some(&failed)).as_deref(),
            Some("failed")
        );
        assert_eq!(
            terminal_event_status(1, Some(&completed)).as_deref(),
            Some("completed")
        );
        assert_eq!(terminal_event_status(3, None).as_deref(), Some("failed"));
        assert_eq!(terminal_event_status(0, None), None);
    }
}

async fn build_run_index(
    snapshot: &DatasetCatalogSnapshot,
    engine: &ChronicleQueryEngine,
) -> Result<Arc<SourceRoutingIndex>> {
    let mut routes = SourceRoutingIndexBuilder::new(RUN_COLUMNS);
    for dataset in snapshot.datasets() {
        let name = &dataset.mount.name;
        routes.ensure_dataset(name);
        let mut batches = engine
            .dataframe(&format!(
                "SELECT _file_, run_id, session_id, agent_id, agent_model_name FROM {name}.runs"
            ))
            .await?
            .execute_stream()
            .await?;
        while let Some(batch) = batches.try_next().await? {
            add_run_batch(&mut routes, name, &batch)?;
            routes.ensure_within_limits()?;
        }
    }
    Ok(Arc::new(routes.finish()))
}

async fn build_event_identity_index(
    snapshot: &DatasetCatalogSnapshot,
    engine: &ChronicleQueryEngine,
) -> Result<Arc<SourceRoutingIndex>> {
    let mut routes = SourceRoutingIndexBuilder::new(EVENT_IDENTITY_COLUMNS);
    for dataset in snapshot.datasets() {
        let name = &dataset.mount.name;
        routes.ensure_dataset(name);
        let mut batches = engine
            .dataframe(&format!(
                "SELECT _file_, event_id, trace_id FROM {name}.events"
            ))
            .await?
            .execute_stream()
            .await?;
        while let Some(batch) = batches.try_next().await? {
            add_event_identity_batch(&mut routes, name, &batch)?;
            routes.ensure_within_limits()?;
        }
    }
    Ok(Arc::new(routes.finish()))
}

async fn build_event_partition_index(
    snapshot: &DatasetCatalogSnapshot,
    engine: &ChronicleQueryEngine,
) -> Result<Arc<SourceRoutingIndex>> {
    let mut routes = SourceRoutingIndexBuilder::new(EVENT_PARTITION_COLUMNS);
    for dataset in snapshot.datasets() {
        let name = &dataset.mount.name;
        routes.ensure_dataset(name);
        let mut batches = engine
            .dataframe(&format!(
                "SELECT _file_, session_id, agent_id FROM {name}.events"
            ))
            .await?
            .execute_stream()
            .await?;
        while let Some(batch) = batches.try_next().await? {
            add_event_partition_batch(&mut routes, name, &batch)?;
            routes.ensure_within_limits()?;
        }
    }
    Ok(Arc::new(routes.finish()))
}

fn add_event_identity_batch(
    routes: &mut SourceRoutingIndexBuilder,
    dataset: &str,
    batch: &RecordBatch,
) -> Result<()> {
    let file = string_column(batch, "_file_")?;
    let event_id = string_column(batch, "event_id")?;
    let trace_id = string_column(batch, "trace_id")?;
    for row in 0..batch.num_rows() {
        anyhow::ensure!(!file.is_null(row), "event routing row has null _file_");
        routes.add(
            dataset,
            file.value(row),
            [
                ("event_id", optional_string(event_id, row)),
                ("trace_id", optional_string(trace_id, row)),
            ],
        );
    }
    Ok(())
}

fn add_event_partition_batch(
    routes: &mut SourceRoutingIndexBuilder,
    dataset: &str,
    batch: &RecordBatch,
) -> Result<()> {
    let file = string_column(batch, "_file_")?;
    let session_id = string_column(batch, "session_id")?;
    let agent_id = string_column(batch, "agent_id")?;
    for row in 0..batch.num_rows() {
        anyhow::ensure!(!file.is_null(row), "event routing row has null _file_");
        routes.add(
            dataset,
            file.value(row),
            [
                ("session_id", optional_string(session_id, row)),
                ("agent_id", optional_string(agent_id, row)),
            ],
        );
    }
    Ok(())
}

fn add_run_batch(
    routes: &mut SourceRoutingIndexBuilder,
    dataset: &str,
    batch: &RecordBatch,
) -> Result<()> {
    let file = string_column(batch, "_file_")?;
    let run_id = string_column(batch, "run_id")?;
    let session_id = string_column(batch, "session_id")?;
    let agent_id = string_column(batch, "agent_id")?;
    let model_name = string_column(batch, "agent_model_name")?;
    for row in 0..batch.num_rows() {
        anyhow::ensure!(!file.is_null(row), "run routing row has null _file_");
        routes.add(
            dataset,
            file.value(row),
            [
                ("run_id", optional_string(run_id, row)),
                ("session_id", optional_string(session_id, row)),
                ("agent_id", optional_string(agent_id, row)),
                ("agent_model_name", optional_string(model_name, row)),
            ],
        );
    }
    Ok(())
}

fn string_column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    let index = batch
        .schema()
        .index_of(name)
        .with_context(|| format!("routing query is missing column {name}"))?;
    batch
        .column(index)
        .as_any()
        .downcast_ref::<StringArray>()
        .with_context(|| format!("routing query column {name} is not Utf8"))
}

fn optional_string(values: &StringArray, row: usize) -> Option<&str> {
    (!values.is_null(row)).then(|| values.value(row))
}

fn required_json_string<'a>(row: &'a JsonValue, field: &str) -> Result<&'a str> {
    row.get(field)
        .and_then(JsonValue::as_str)
        .with_context(|| format!("run index row is missing string field {field}"))
}

fn required_json_u64(row: &JsonValue, field: &str) -> Result<u64> {
    row.get(field)
        .and_then(JsonValue::as_u64)
        .with_context(|| format!("run index row is missing unsigned integer field {field}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn required_summary_failure_is_cached_and_preserves_sources_at_http_boundary() {
        use std::io;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use axum::response::IntoResponse as _;
        use futures::future::join_all;
        use http_body_util::BodyExt as _;

        let acceleration = ServerAcceleration::default();
        let attempts = AtomicUsize::new(0);
        let failures = join_all((0..8).map(|_| {
            acceleration.run_summaries_with(|| async {
                attempts.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
                Err(
                    anyhow::Error::new(io::Error::other("required-summary-source-diagnostic"))
                        .context("build required summaries"),
                )
            })
        }))
        .await;

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        let status = acceleration.status();
        assert!(!status.run_summaries_ready);
        assert!(status.run_summaries_failed);

        for failure in &failures {
            let chain = failure
                .as_ref()
                .unwrap_err()
                .chain()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            assert!(chain
                .iter()
                .any(|entry| entry == "build required summaries"));
            assert!(chain
                .iter()
                .any(|entry| entry == "required-summary-source-diagnostic"));
        }

        let response = super::super::problem::ApiError::internal(
            failures.into_iter().next().unwrap().unwrap_err(),
        )
        .into_response();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: JsonValue = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["code"], "internal");
        assert_eq!(body["message"], "internal server error");
        assert!(!body
            .to_string()
            .contains("required-summary-source-diagnostic"));
    }

    #[tokio::test]
    async fn optional_index_failure_is_cached_and_falls_back_without_diagnostics() -> Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use futures::future::join_all;
        use persisting_pchronicle::storage::{
            CatalogSnapshotOptions, DatasetMount, DEFAULT_DATASET_NAME,
        };

        let root = tempfile::tempdir()?;
        let snapshot = DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(root.path().to_string_lossy())?],
            Some(DEFAULT_DATASET_NAME.into()),
            CatalogSnapshotOptions::default(),
        )
        .await?;
        let acceleration = ServerAcceleration::default();
        let attempts = AtomicUsize::new(0);
        let sql = "SELECT session_id FROM runs WHERE session_id = 'session-a'";
        let routed = join_all((0..8).map(|_| {
            acceleration.route_sql_with(&snapshot, sql, |_| async {
                attempts.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
                anyhow::bail!("optional-index-source-diagnostic")
            })
        }))
        .await;

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(routed.iter().all(|query| {
            query.sql == sql
                && query.outcome == RoutingOutcome::IndexUnavailable
                && query.candidate_sources.is_none()
        }));
        let status = acceleration.status();
        assert!(status.run_index.is_none());
        assert!(status.run_index_unavailable);
        assert!(!status.event_identity_index_unavailable);
        assert!(!status.event_partition_index_unavailable);
        let status = serde_json::to_string(&status)?;
        assert!(!status.contains("optional-index-source-diagnostic"));
        assert!(!routed.iter().any(|query| {
            query.sql.contains("optional-index-source-diagnostic")
                || query
                    .outcome
                    .as_str()
                    .contains("optional-index-source-diagnostic")
        }));
        Ok(())
    }

    fn event(
        event_id: &str,
        trace_id: &str,
        agent_id: &str,
    ) -> persisting_pchronicle::model::EventRecord {
        persisting_pchronicle::model::EventRecord {
            identity: persisting_pchronicle::model::EventIdentity {
                event_id: Some(event_id.into()),
                ..Default::default()
            },
            seq: 1,
            source: "server-routing-test".into(),
            kind: "event".into(),
            timestamp: None,
            session_id: None,
            agent_id: Some(agent_id.into()),
            parent_uuid: None,
            trace_id: Some(trace_id.into()),
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({"event_id": event_id}),
        }
    }

    #[test]
    fn routing_index_intersects_values_without_duplicating_source_paths() {
        let mut builder =
            SourceRoutingIndexBuilder::new(&["event_id", "trace_id", "session_id", "agent_id"]);
        builder.add(
            "live",
            "project-a/run-1/events.lance",
            [
                ("event_id", Some("event-1")),
                ("trace_id", Some("trace-1")),
                ("session_id", Some("session-1")),
                ("agent_id", Some("project-a")),
            ],
        );
        builder.add(
            "live",
            "project-a/run-1/events.lance",
            [
                ("event_id", Some("event-2")),
                ("trace_id", Some("trace-1")),
                ("session_id", Some("session-1")),
                ("agent_id", Some("project-a")),
            ],
        );
        builder.add(
            "live",
            "project-b/run-2/events.lance",
            [
                ("event_id", Some("event-3")),
                ("trace_id", Some("trace-2")),
                ("session_id", Some("session-2")),
                ("agent_id", Some("project-b")),
            ],
        );
        builder.add(
            "live",
            "project-c/run-3/events.lance",
            [
                ("event_id", Some("")),
                ("trace_id", None),
                ("session_id", Some("session-3")),
                ("agent_id", Some("project-c")),
            ],
        );
        let index = builder.finish();
        assert_eq!(index.status().rows, 4);
        assert_eq!(index.status().sources, 3);
        assert_eq!(
            index.candidates(
                "live",
                &[
                    RoutingConstraint {
                        column: "agent_id".into(),
                        values: vec!["project-a".into()],
                    },
                    RoutingConstraint {
                        column: "event_id".into(),
                        values: vec!["event-2".into()],
                    },
                ]
            ),
            Some(vec!["project-a/run-1/events.lance".into()])
        );
        assert_eq!(
            index.candidates(
                "live",
                &[RoutingConstraint {
                    column: "event_id".into(),
                    values: vec!["missing".into()],
                }]
            ),
            Some(Vec::new())
        );
        assert_eq!(
            index.candidates(
                "live",
                &[RoutingConstraint {
                    column: "event_id".into(),
                    values: vec![String::new()],
                }]
            ),
            Some(vec!["project-c/run-3/events.lance".into()])
        );
    }

    #[test]
    fn required_constraints_ignore_disjunctions() {
        let statements = Parser::parse_sql(
            &GenericDialect,
            "SELECT * FROM events WHERE agent_id = 'project-a' AND (event_id = 'one' OR event_id = 'two')",
        )
        .unwrap();
        let Statement::Query(query) = &statements[0] else {
            panic!("expected query")
        };
        let selection = query.body.as_select().unwrap().selection.as_ref().unwrap();
        let mut constraints = Vec::new();
        collect_required_constraints(selection, &mut constraints);
        assert_eq!(
            constraints,
            vec![RoutingConstraint {
                column: "agent_id".into(),
                values: vec!["project-a".into()],
            }]
        );
    }

    #[test]
    fn routing_index_limits_are_explicit() {
        let mut builder = SourceRoutingIndexBuilder::new(EVENT_IDENTITY_COLUMNS);
        builder.add(
            "live",
            "one/events.lance",
            [("event_id", Some("one")), ("trace_id", Some("trace"))],
        );
        assert!(builder.ensure_limits(0, usize::MAX).is_err());
        assert!(builder.ensure_limits(usize::MAX, 1).is_err());
    }

    #[tokio::test]
    async fn event_index_routes_to_one_catalog_source_without_changing_results() -> Result<()> {
        use persisting_pchronicle::storage::{
            CatalogSnapshotOptions, DatasetMount, RawEventLanceAppender, StoryCoords,
            DEFAULT_DATASET_NAME,
        };

        let root = std::env::temp_dir().join(format!(
            "pchronicle-server-event-routing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir_all(&root)?;
        let project_a = StoryCoords::new(
            root.to_string_lossy(),
            "project-a",
            "session-a",
            Some("run-a".into()),
        );
        let project_b = StoryCoords::new(
            root.to_string_lossy(),
            "project-b",
            "session-b",
            Some("run-b".into()),
        );
        let mut appender = RawEventLanceAppender::default();
        appender
            .append_event_batch(&[
                (project_a, event("event-a", "trace-a", "project-a")),
                (project_b, event("event-b", "trace-b", "project-b")),
            ])
            .await?;
        appender.finish();

        let snapshot = Arc::new(
            DatasetCatalogSnapshot::discover(
                vec![DatasetMount::default(root.to_string_lossy())?],
                Some(DEFAULT_DATASET_NAME.into()),
                CatalogSnapshotOptions::default(),
            )
            .await?,
        );
        let engine = snapshot.clone().query_engine(Default::default()).await?;
        let acceleration = ServerAcceleration::default();
        let sql = "SELECT _file_, event_id FROM events WHERE agent_id = 'project-a' AND event_id = 'event-a'";
        let routed = acceleration.route_sql(&snapshot, &engine, sql).await;
        assert_eq!(routed.outcome, RoutingOutcome::Applied);
        assert_eq!(routed.candidate_sources, Some(1));
        assert!(routed.sql.contains("project-a/run-a/events.lance"));

        let original = engine.query_jsonl(sql).await?;
        let accelerated = engine.query_jsonl(&routed.sql).await?;
        assert_eq!(accelerated, original);
        assert_eq!(acceleration.status().event_identity_index.unwrap().rows, 2);
        assert!(acceleration.status().event_partition_index.is_none());

        let partition_acceleration = ServerAcceleration::default();
        let project_sql =
            "SELECT event_id FROM events WHERE agent_id = 'project-a' ORDER BY event_id";
        let project_routed = partition_acceleration
            .route_sql(&snapshot, &engine, project_sql)
            .await;
        assert_eq!(project_routed.outcome, RoutingOutcome::Applied);
        assert_eq!(project_routed.candidate_sources, Some(1));
        assert_eq!(
            engine.query_jsonl(&project_routed.sql).await?,
            engine.query_jsonl(project_sql).await?
        );
        assert!(partition_acceleration
            .status()
            .event_identity_index
            .is_none());
        assert_eq!(
            partition_acceleration
                .status()
                .event_partition_index
                .unwrap()
                .rows,
            2
        );

        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
