use std::collections::{BTreeMap, BTreeSet};

use persisting_pchronicle::model::EventRecord;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{RunSummary, TrajectoryTurnView, WireToolCall};

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct ExplorerRunsQuery {
    pub(crate) q: Option<String>,
    pub(crate) dataset: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) agent: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) file: Option<String>,
    pub(crate) sort: Option<String>,
    pub(crate) direction: Option<String>,
    pub(crate) offset: Option<usize>,
    pub(crate) limit: Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct ExplorerTreeQuery {
    pub(crate) dataset: Option<String>,
    pub(crate) prefix: Option<String>,
}

pub(crate) const MAX_TREE_CHILDREN: usize = 16;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogTree {
    pub(crate) dataset: Option<String>,
    #[serde(default)]
    pub(crate) prefix: String,
    pub(crate) run_count: usize,
    pub(crate) failed_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ready_sources: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error_sources: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) total_tokens: Option<u64>,
    pub(crate) children: Vec<CatalogTreeChild>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogTreeChild {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) path: String,
    pub(crate) run_count: usize,
    pub(crate) failed_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) entries: Vec<CatalogTreeChild>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct WarehouseSources {
    pub(crate) run_count: usize,
    pub(crate) failed_count: usize,
    pub(crate) source_count: usize,
    pub(crate) error_sources: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) total_tokens: Option<u64>,
    pub(crate) sources: Vec<WarehouseSource>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct WarehouseSource {
    pub(crate) dataset: String,
    pub(crate) file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) format: Option<String>,
    pub(crate) kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) snapshot_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) projection_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) projection_generation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_modified: Option<String>,
    pub(crate) status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    pub(crate) run_count: usize,
    pub(crate) failed_count: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SourceSeed {
    pub(crate) dataset: String,
    pub(crate) file: String,
    pub(crate) format: Option<String>,
    pub(crate) kind: String,
    pub(crate) snapshot_ref: Option<String>,
    pub(crate) projection_status: Option<String>,
    pub(crate) projection_generation: Option<String>,
    pub(crate) size_bytes: Option<u64>,
    pub(crate) last_modified: Option<String>,
    pub(crate) status: String,
    pub(crate) error: Option<String>,
}

pub(crate) fn warehouse_sources(
    seeds: &[SourceSeed],
    summaries: &[RunSummary],
) -> WarehouseSources {
    let run_count = summaries.len();
    let failed_count = summaries
        .iter()
        .filter(|run| is_failed_status(&run.status))
        .count();
    let error_sources = seeds.iter().filter(|seed| seed.status == "error").count();
    let mut sources: Vec<WarehouseSource> = seeds
        .iter()
        .map(|seed| {
            let (run_count, failed_count) = summaries.iter().fold((0, 0), |acc, run| {
                if run.dataset == seed.dataset && run.file == seed.file {
                    (
                        acc.0 + 1,
                        acc.1 + usize::from(is_failed_status(&run.status)),
                    )
                } else {
                    acc
                }
            });
            WarehouseSource {
                dataset: seed.dataset.clone(),
                file: seed.file.clone(),
                format: seed.format.clone(),
                kind: seed.kind.clone(),
                snapshot_ref: seed.snapshot_ref.clone(),
                projection_status: seed.projection_status.clone(),
                projection_generation: seed.projection_generation.clone(),
                size_bytes: seed.size_bytes,
                last_modified: seed.last_modified.clone(),
                status: seed.status.clone(),
                error: seed.error.clone(),
                run_count,
                failed_count,
            }
        })
        .collect();
    sources.sort_by(|left, right| {
        right
            .run_count
            .cmp(&left.run_count)
            .then(left.dataset.cmp(&right.dataset))
            .then(left.file.cmp(&right.file))
    });
    WarehouseSources {
        run_count,
        failed_count,
        source_count: sources.len(),
        error_sources,
        sources,
        ..WarehouseSources::default()
    }
}

pub(crate) fn catalog_tree(
    summaries: &[RunSummary],
    dataset: Option<&str>,
    prefix: &str,
    max_children: usize,
) -> CatalogTree {
    let prefix = prefix.trim().trim_matches('/');
    let scoped = summaries
        .iter()
        .filter(|run| {
            dataset.is_none_or(|name| run.dataset == name)
                && (dataset.is_none() || file_matches_prefix(&run.file, prefix))
        })
        .collect::<Vec<_>>();
    let run_count = scoped.len();
    let failed_count = scoped
        .iter()
        .filter(|run| is_failed_status(&run.status))
        .count();
    let children = if dataset.is_none() {
        fold_tree_children(dataset_children(&scoped), max_children, prefix)
    } else {
        fold_tree_children(file_children(&scoped, prefix), max_children, prefix)
    };
    CatalogTree {
        dataset: dataset.map(str::to_string),
        prefix: prefix.to_string(),
        run_count,
        failed_count,
        children,
        ..CatalogTree::default()
    }
}

fn is_failed_status(status: &str) -> bool {
    matches!(status, "failed" | "error")
}

fn file_matches_prefix(file: &str, prefix: &str) -> bool {
    prefix.is_empty() || file == prefix || file.starts_with(&format!("{prefix}/"))
}

struct ChildAcc {
    run_count: usize,
    failed_count: usize,
    has_deeper: bool,
}

fn dataset_children(runs: &[&RunSummary]) -> Vec<CatalogTreeChild> {
    let mut groups = BTreeMap::<String, ChildAcc>::new();
    for run in runs {
        let entry = groups.entry(run.dataset.clone()).or_insert(ChildAcc {
            run_count: 0,
            failed_count: 0,
            has_deeper: false,
        });
        entry.run_count += 1;
        if is_failed_status(&run.status) {
            entry.failed_count += 1;
        }
    }
    groups
        .into_iter()
        .map(|(name, acc)| CatalogTreeChild {
            name: name.clone(),
            kind: "dataset".into(),
            path: name,
            run_count: acc.run_count,
            failed_count: acc.failed_count,
            entries: Vec::new(),
        })
        .collect()
}

fn file_children(runs: &[&RunSummary], prefix: &str) -> Vec<CatalogTreeChild> {
    let mut groups = BTreeMap::<String, ChildAcc>::new();
    for run in runs {
        if !prefix.is_empty() && run.file == prefix {
            continue;
        }
        let rest = if prefix.is_empty() {
            run.file.as_str()
        } else {
            match run.file.strip_prefix(&format!("{prefix}/")) {
                Some(rest) => rest,
                None => continue,
            }
        };
        if rest.is_empty() {
            continue;
        }
        let (name, has_deeper) = match rest.split_once('/') {
            Some((name, _)) => (name, true),
            None => (rest, false),
        };
        if name.is_empty() {
            continue;
        }
        let entry = groups.entry(name.to_string()).or_insert(ChildAcc {
            run_count: 0,
            failed_count: 0,
            has_deeper: false,
        });
        entry.run_count += 1;
        if is_failed_status(&run.status) {
            entry.failed_count += 1;
        }
        entry.has_deeper |= has_deeper;
    }
    groups
        .into_iter()
        .map(|(name, acc)| CatalogTreeChild {
            path: if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            },
            kind: if acc.has_deeper {
                "dir".into()
            } else {
                "file".into()
            },
            name,
            run_count: acc.run_count,
            failed_count: acc.failed_count,
            entries: Vec::new(),
        })
        .collect()
}

fn fold_tree_children(
    mut children: Vec<CatalogTreeChild>,
    max_children: usize,
    prefix: &str,
) -> Vec<CatalogTreeChild> {
    children.sort_by(|left, right| {
        right
            .run_count
            .cmp(&left.run_count)
            .then(left.name.cmp(&right.name))
    });
    if max_children == 0 || children.len() <= max_children {
        return children;
    }
    let keep = max_children.saturating_sub(1);
    let rest = children.split_off(keep);
    children.push(CatalogTreeChild {
        name: "other".into(),
        kind: "other".into(),
        path: prefix.to_string(),
        run_count: rest.iter().map(|child| child.run_count).sum(),
        failed_count: rest.iter().map(|child| child.failed_count).sum(),
        entries: rest,
    });
    children
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ExplorerPage<T> {
    pub(crate) snapshot: PageSnapshot,
    pub(crate) records: Vec<T>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RunExplorerPage {
    pub(crate) snapshot: PageSnapshot,
    pub(crate) records: Vec<RunExplorerItem>,
    pub(crate) path_index: Vec<RunSummary>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PageSnapshot {
    pub(crate) offset: usize,
    pub(crate) next_offset: usize,
    pub(crate) total: usize,
    pub(crate) has_more: bool,
    pub(crate) limit: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RunExplorerItem {
    #[serde(flatten)]
    pub(crate) run: RunSummary,
    pub(crate) model: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct MetricStats {
    pub(crate) sample_count: usize,
    pub(crate) total_count: usize,
    pub(crate) p50: Option<f64>,
    pub(crate) p95: Option<f64>,
    pub(crate) max: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ToolAggregate {
    pub(crate) name: String,
    pub(crate) count: usize,
    pub(crate) duration_sample_count: usize,
    pub(crate) total_duration_ms: Option<f64>,
    pub(crate) average_duration_ms: Option<f64>,
    pub(crate) max_duration_ms: Option<f64>,
    pub(crate) error_associated_count: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DimensionAggregate {
    pub(crate) name: String,
    pub(crate) turn_count: usize,
    pub(crate) error_count: usize,
    pub(crate) latency_sample_count: usize,
    pub(crate) average_latency_ms: Option<f64>,
    pub(crate) total_tokens: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct HistogramBucket {
    pub(crate) label: &'static str,
    pub(crate) lower_bound_ms: f64,
    pub(crate) upper_bound_ms: Option<f64>,
    pub(crate) count: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RunAnalysis {
    pub(crate) run: RunSummary,
    pub(crate) event_count: usize,
    pub(crate) turn_count: usize,
    pub(crate) tool_call_count: usize,
    pub(crate) error_count: usize,
    pub(crate) start_timestamp: Option<String>,
    pub(crate) end_timestamp: Option<String>,
    pub(crate) models: Vec<String>,
    pub(crate) prompt_tokens: Option<u64>,
    pub(crate) completion_tokens: Option<u64>,
    pub(crate) total_tokens: Option<u64>,
    pub(crate) latency_ms: MetricStats,
    pub(crate) ttft_ms: MetricStats,
    pub(crate) latency_histogram: Vec<HistogramBucket>,
    pub(crate) source_breakdown: Vec<DimensionAggregate>,
    pub(crate) kind_breakdown: Vec<DimensionAggregate>,
    pub(crate) model_breakdown: Vec<DimensionAggregate>,
    pub(crate) tools: Vec<ToolAggregate>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TurnSummary {
    pub(crate) id: i64,
    pub(crate) source: String,
    pub(crate) kind: Option<String>,
    pub(crate) timestamp: Option<String>,
    pub(crate) call_id: Option<String>,
    pub(crate) preview: String,
    pub(crate) char_count: u64,
    pub(crate) modalities: Vec<String>,
    pub(crate) model_name: Option<String>,
    pub(crate) latency_ms: Option<f64>,
    pub(crate) ttft_ms: Option<f64>,
    pub(crate) prompt_tokens: Option<u64>,
    pub(crate) completion_tokens: Option<u64>,
    pub(crate) total_tokens: Option<u64>,
    pub(crate) tool_names: Vec<String>,
    pub(crate) event_seqs: Vec<u64>,
    pub(crate) has_error: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TurnDetail {
    pub(crate) summary: TurnSummary,
    pub(crate) turn: persisting_pchronicle::model::StorylineTurn,
    pub(crate) wire_tool_calls: Vec<WireToolCall>,
    pub(crate) events: Vec<EventRecord>,
}

pub(crate) fn explorer_run_path(
    dataset: &str,
    file: &str,
    document_id: &str,
    session_id: &str,
    run_id: Option<&str>,
    parent_session_id: Option<&str>,
) -> String {
    if file == "." {
        return match parent_session_id {
            Some(parent) if parent != session_id => {
                format!("{dataset}/{parent}/subagents/{document_id}")
            }
            _ => format!("{dataset}/{document_id}"),
        };
    }
    let root_session_id = parent_session_id.or_else(|| run_id.filter(|id| *id != session_id));
    match root_session_id {
        Some(root) if root != session_id => {
            format!("{dataset}/{file}/{root}/{session_id}")
        }
        Some(root) => format!("{dataset}/{file}/{root}"),
        None => format!("{dataset}/{file}/{session_id}"),
    }
}

pub(crate) fn run_page(summaries: Vec<RunSummary>, query: &ExplorerRunsQuery) -> RunExplorerPage {
    let needle = query
        .q
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let mut records = summaries
        .into_iter()
        .map(|run| RunExplorerItem {
            model: run.model_name.clone(),
            run,
        })
        .filter(|item| {
            (needle.is_empty()
                || format!(
                    "{} {} {} {} {}",
                    item.run.agent_id,
                    item.run.session_id,
                    item.run.root_session_id.as_deref().unwrap_or_default(),
                    item.run.status,
                    item.run.path,
                )
                .to_ascii_lowercase()
                .contains(&needle))
                && matches_filter(&item.run.dataset, query.dataset.as_deref())
                && matches_filter(&item.run.status, query.status.as_deref())
                && matches_filter(&item.run.agent_id, query.agent.as_deref())
                && matches_filter(
                    item.model.as_deref().unwrap_or_default(),
                    query.model.as_deref(),
                )
                && file_matches_prefix(
                    &item.run.file,
                    query
                        .file
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .unwrap_or(""),
                )
        })
        .collect::<Vec<_>>();
    let path_index = records.iter().map(|item| item.run.clone()).collect();
    if let Some(path) = query
        .path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        let prefix = format!("{path}/");
        records.retain(|item| item.run.path == path || item.run.path.starts_with(&prefix));
    }

    records.sort_by(
        |left, right| match query.sort.as_deref().unwrap_or("session") {
            "events" => left.run.row_count.cmp(&right.run.row_count),
            "status" => left.run.status.cmp(&right.run.status),
            "agent" => left.run.agent_id.cmp(&right.run.agent_id),
            _ => left.run.session_id.cmp(&right.run.session_id),
        },
    );
    if query.direction.as_deref() == Some("desc") {
        records.reverse();
    }

    let page = paginate(
        records,
        query.offset.unwrap_or(0),
        query.limit.unwrap_or(50).clamp(1, 200),
    );
    RunExplorerPage {
        snapshot: page.snapshot,
        records: page.records,
        path_index,
    }
}

pub(crate) fn analyze(
    run: RunSummary,
    turns: &[TrajectoryTurnView],
    events: &[EventRecord],
) -> RunAnalysis {
    let mut latencies = Vec::new();
    let mut ttfts = Vec::new();
    let mut prompt_tokens = 0u64;
    let mut completion_tokens = 0u64;
    let mut total_tokens = 0u64;
    let mut prompt_seen = false;
    let mut completion_seen = false;
    let mut total_seen = false;
    let mut models = BTreeSet::new();
    let mut timestamps = Vec::new();
    let mut tools = BTreeMap::<String, ToolAccumulator>::new();
    let mut sources = BTreeMap::<String, DimensionAccumulator>::new();
    let mut kinds = BTreeMap::<String, DimensionAccumulator>::new();
    let mut model_groups = BTreeMap::<String, DimensionAccumulator>::new();
    let mut error_count = 0usize;

    for item in turns {
        if let Some(timestamp) = item.turn.timestamp.as_ref() {
            timestamps.push(timestamp.clone());
        }
        let linked = events
            .iter()
            .filter(|event| item.event_seqs.contains(&event.seq))
            .collect::<Vec<_>>();
        let mut values = Vec::new();
        if let Some(value) = &item.turn.metrics {
            values.push(value);
        }
        if let Some(value) = &item.turn.extra {
            values.push(value);
        }
        values.extend(linked.iter().map(|event| &event.payload));

        let (turn_prompt_tokens, turn_completion_tokens, turn_total_tokens) = token_counts(&values);
        if let Some(value) = turn_prompt_tokens {
            prompt_tokens = prompt_tokens.saturating_add(value);
            prompt_seen = true;
        }
        if let Some(value) = turn_completion_tokens {
            completion_tokens = completion_tokens.saturating_add(value);
            completion_seen = true;
        }
        if let Some(value) = turn_total_tokens {
            total_tokens = total_tokens.saturating_add(value);
            total_seen = true;
        }
        let latency = item.turn.latency_ms.map(|value| value as f64).or_else(|| {
            first_number(
                &values,
                &[
                    "latency_ms",
                    "total_latency_ms",
                    "upstream_latency_ms",
                    "llm_infer_ms",
                ],
            )
        });
        if let Some(value) = latency.filter(|value| value.is_finite() && *value >= 0.0) {
            latencies.push(value);
        }
        let ttft = item
            .turn
            .ttft_ms
            .map(|value| value as f64)
            .or_else(|| first_number(&values, &["ttft_ms"]));
        if let Some(value) = ttft.filter(|value| value.is_finite() && *value >= 0.0) {
            ttfts.push(value);
        }
        let model = item
            .turn
            .model_name
            .clone()
            .or_else(|| first_string(&values, &["model", "model_name", "agent_model"]));
        if let Some(model) = &model {
            if !model.trim().is_empty() {
                models.insert(model.clone());
            }
        }
        let has_error = turn_has_error(item, &linked);
        if has_error {
            error_count += 1;
        }
        let turn_tokens = turn_total_tokens.or_else(|| {
            (turn_prompt_tokens.is_some() || turn_completion_tokens.is_some()).then_some(
                turn_prompt_tokens
                    .unwrap_or_default()
                    .saturating_add(turn_completion_tokens.unwrap_or_default()),
            )
        });
        record_dimension(
            &mut sources,
            &item.turn.source,
            has_error,
            latency,
            turn_tokens,
        );
        record_dimension(
            &mut kinds,
            item.turn.effective_kind(),
            has_error,
            latency,
            turn_tokens,
        );
        record_dimension(
            &mut model_groups,
            model.as_deref().unwrap_or("unavailable"),
            has_error,
            latency,
            turn_tokens,
        );
        for call in display_tool_calls(item) {
            let entry = tools.entry(call.0).or_default();
            entry.count += 1;
            if has_error {
                entry.error_associated_count += 1;
            }
            if let Some(duration) = call.1 {
                entry.total_duration_ms += duration;
                entry.duration_sample_count += 1;
                entry.max_duration_ms = entry.max_duration_ms.max(duration);
            }
        }
    }

    timestamps.sort_by_key(|timestamp| timestamp.timestamp_nanos());
    let tools = tools
        .into_iter()
        .map(|(name, value)| ToolAggregate {
            name,
            count: value.count,
            duration_sample_count: value.duration_sample_count,
            total_duration_ms: (value.duration_sample_count > 0).then_some(value.total_duration_ms),
            average_duration_ms: (value.duration_sample_count > 0)
                .then_some(value.total_duration_ms / value.duration_sample_count as f64),
            max_duration_ms: (value.duration_sample_count > 0).then_some(value.max_duration_ms),
            error_associated_count: value.error_associated_count,
        })
        .collect();
    let latency_histogram = latency_histogram(&latencies);

    RunAnalysis {
        event_count: events.len(),
        turn_count: turns.len(),
        tool_call_count: turns
            .iter()
            .map(display_tool_calls)
            .map(|calls| calls.len())
            .sum(),
        error_count,
        start_timestamp: timestamps
            .first()
            .map(|timestamp| timestamp.canonical_rfc3339()),
        end_timestamp: timestamps
            .last()
            .map(|timestamp| timestamp.canonical_rfc3339()),
        models: models.into_iter().collect(),
        prompt_tokens: prompt_seen.then_some(prompt_tokens),
        completion_tokens: completion_seen.then_some(completion_tokens),
        total_tokens: if total_seen {
            Some(total_tokens)
        } else if prompt_seen || completion_seen {
            Some(prompt_tokens.saturating_add(completion_tokens))
        } else {
            None
        },
        latency_ms: metric_stats(latencies, turns.len()),
        ttft_ms: metric_stats(ttfts, turns.len()),
        latency_histogram,
        source_breakdown: dimension_aggregates(sources),
        kind_breakdown: dimension_aggregates(kinds),
        model_breakdown: dimension_aggregates(model_groups),
        tools,
        run,
    }
}

pub(crate) fn turn_page(
    turns: &[TrajectoryTurnView],
    events: &[EventRecord],
    q: Option<&str>,
    source: Option<&str>,
    offset: usize,
    limit: usize,
) -> ExplorerPage<TurnSummary> {
    let needle = q.unwrap_or_default().trim().to_ascii_lowercase();
    let records = turns
        .iter()
        .filter(|item| source.is_none_or(|source| source == "all" || item.turn.source == source))
        .filter(|item| needle.is_empty() || searchable_turn(item).contains(&needle))
        .map(|item| turn_summary(item, events))
        .collect();
    paginate(records, offset, limit.clamp(1, 500))
}

pub(crate) fn turn_detail(item: &TrajectoryTurnView, events: &[EventRecord]) -> TurnDetail {
    let linked = events
        .iter()
        .filter(|event| item.event_seqs.contains(&event.seq))
        .cloned()
        .collect::<Vec<_>>();
    TurnDetail {
        summary: turn_summary(item, events),
        turn: item.turn.clone(),
        wire_tool_calls: item.wire_tool_calls.clone(),
        events: linked,
    }
}

fn turn_summary(item: &TrajectoryTurnView, events: &[EventRecord]) -> TurnSummary {
    let linked = events
        .iter()
        .filter(|event| item.event_seqs.contains(&event.seq))
        .collect::<Vec<_>>();
    let mut values = Vec::new();
    if let Some(value) = &item.turn.metrics {
        values.push(value);
    }
    if let Some(value) = &item.turn.extra {
        values.push(value);
    }
    values.extend(linked.iter().map(|event| &event.payload));
    let (prompt_tokens, completion_tokens, total_tokens) = token_counts(&values);
    let tool_names = display_tool_calls(item)
        .into_iter()
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
    let extracted = extract_message_content(&item.turn.message, !tool_names.is_empty());
    TurnSummary {
        id: item.turn.id,
        source: item.turn.source.clone(),
        kind: item.turn.kind.clone(),
        timestamp: item
            .turn
            .timestamp
            .as_ref()
            .map(|timestamp| timestamp.canonical_rfc3339()),
        call_id: item.call_id.clone(),
        preview: compact(&extracted.text, 180),
        char_count: extracted.char_count,
        modalities: extracted.modalities,
        model_name: item
            .turn
            .model_name
            .clone()
            .or_else(|| first_string(&values, &["model", "model_name", "agent_model"])),
        latency_ms: item.turn.latency_ms.map(|value| value as f64).or_else(|| {
            first_number(
                &values,
                &[
                    "latency_ms",
                    "total_latency_ms",
                    "upstream_latency_ms",
                    "llm_infer_ms",
                ],
            )
        }),
        ttft_ms: item
            .turn
            .ttft_ms
            .map(|value| value as f64)
            .or_else(|| first_number(&values, &["ttft_ms"])),
        prompt_tokens,
        completion_tokens,
        total_tokens,
        tool_names,
        event_seqs: item.event_seqs.clone(),
        has_error: turn_has_error(item, &linked),
    }
}

fn paginate<T>(records: Vec<T>, offset: usize, limit: usize) -> ExplorerPage<T> {
    let total = records.len();
    let offset = offset.min(total);
    let records = records
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let next_offset = offset + records.len();
    ExplorerPage {
        snapshot: PageSnapshot {
            offset,
            next_offset,
            total,
            has_more: next_offset < total,
            limit,
        },
        records,
    }
}

fn matches_filter(value: &str, filter: Option<&str>) -> bool {
    filter
        .map(str::trim)
        .filter(|filter| !filter.is_empty() && *filter != "all")
        .is_none_or(|filter| value.eq_ignore_ascii_case(filter))
}

#[derive(Default)]
struct ToolAccumulator {
    count: usize,
    duration_sample_count: usize,
    total_duration_ms: f64,
    max_duration_ms: f64,
    error_associated_count: usize,
}

#[derive(Default)]
struct DimensionAccumulator {
    turn_count: usize,
    error_count: usize,
    latency_sample_count: usize,
    total_latency_ms: f64,
    total_tokens: u64,
    token_sample_count: usize,
}

fn record_dimension(
    dimensions: &mut BTreeMap<String, DimensionAccumulator>,
    name: &str,
    has_error: bool,
    latency_ms: Option<f64>,
    total_tokens: Option<u64>,
) {
    let entry = dimensions.entry(name.to_string()).or_default();
    entry.turn_count += 1;
    entry.error_count += usize::from(has_error);
    if let Some(value) = latency_ms {
        entry.latency_sample_count += 1;
        entry.total_latency_ms += value;
    }
    if let Some(value) = total_tokens {
        entry.token_sample_count += 1;
        entry.total_tokens = entry.total_tokens.saturating_add(value);
    }
}

fn dimension_aggregates(values: BTreeMap<String, DimensionAccumulator>) -> Vec<DimensionAggregate> {
    let mut values = values
        .into_iter()
        .map(|(name, value)| DimensionAggregate {
            name,
            turn_count: value.turn_count,
            error_count: value.error_count,
            latency_sample_count: value.latency_sample_count,
            average_latency_ms: (value.latency_sample_count > 0)
                .then_some(value.total_latency_ms / value.latency_sample_count as f64),
            total_tokens: (value.token_sample_count > 0).then_some(value.total_tokens),
        })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| {
        right
            .turn_count
            .cmp(&left.turn_count)
            .then_with(|| left.name.cmp(&right.name))
    });
    values
}

fn latency_histogram(values: &[f64]) -> Vec<HistogramBucket> {
    const BUCKETS: [(&str, f64, Option<f64>); 6] = [
        ("<100ms", 0.0, Some(100.0)),
        ("100–500ms", 100.0, Some(500.0)),
        ("0.5–1s", 500.0, Some(1_000.0)),
        ("1–3s", 1_000.0, Some(3_000.0)),
        ("3–10s", 3_000.0, Some(10_000.0)),
        ("≥10s", 10_000.0, None),
    ];
    BUCKETS
        .into_iter()
        .map(|(label, lower_bound_ms, upper_bound_ms)| HistogramBucket {
            label,
            lower_bound_ms,
            upper_bound_ms,
            count: values
                .iter()
                .filter(|value| {
                    **value >= lower_bound_ms && upper_bound_ms.is_none_or(|upper| **value < upper)
                })
                .count(),
        })
        .collect()
}

fn token_counts(values: &[&Value]) -> (Option<u64>, Option<u64>, Option<u64>) {
    let non_negative = |value: f64| value.is_finite().then_some(value.max(0.0) as u64);
    let prompt = first_number(
        values,
        &["prompt_tokens", "input_tokens", "prompt_tokens_len"],
    )
    .and_then(non_negative);
    let completion = first_number(
        values,
        &[
            "completion_tokens",
            "output_tokens",
            "completion_tokens_len",
        ],
    )
    .and_then(non_negative);
    let total = first_number(values, &["total_tokens"])
        .and_then(non_negative)
        .or_else(|| {
            (prompt.is_some() || completion.is_some()).then_some(
                prompt
                    .unwrap_or_default()
                    .saturating_add(completion.unwrap_or_default()),
            )
        });
    (prompt, completion, total)
}

fn metric_stats(mut values: Vec<f64>, total_count: usize) -> MetricStats {
    values.sort_by(f64::total_cmp);
    MetricStats {
        sample_count: values.len(),
        total_count,
        p50: percentile(&values, 0.50),
        p95: percentile(&values, 0.95),
        max: values.last().copied(),
    }
}

fn percentile(values: &[f64], percentile: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let index = ((values.len() - 1) as f64 * percentile).ceil() as usize;
    values.get(index).copied()
}

pub(super) fn display_tool_calls(item: &TrajectoryTurnView) -> Vec<(String, Option<f64>)> {
    if let Some(calls) = &item.turn.tool_calls {
        return calls
            .iter()
            .map(|call| {
                (
                    call.function_name.clone(),
                    call.duration_ms.map(|value| value as f64),
                )
            })
            .collect();
    }
    if !matches!(
        item.turn.source.trim().to_ascii_lowercase().as_str(),
        "agent" | "assistant" | "model"
    ) {
        return Vec::new();
    }
    item.wire_tool_calls
        .iter()
        .map(|call| (call.name.clone(), None))
        .collect()
}

fn searchable_turn(item: &TrajectoryTurnView) -> String {
    format!(
        "{} {} {} {}",
        item.turn.source,
        item.turn.kind.as_deref().unwrap_or_default(),
        item.call_id.as_deref().unwrap_or_default(),
        match &item.turn.message {
            Value::String(value) => value.clone(),
            value => value.to_string(),
        }
    )
    .to_ascii_lowercase()
}

#[derive(Debug, PartialEq)]
struct ExtractedMessage {
    text: String,
    char_count: u64,
    modalities: Vec<String>,
}

fn extract_message_content(message: &Value, has_tools: bool) -> ExtractedMessage {
    let mut texts = Vec::new();
    let mut flags = BTreeSet::new();
    match message {
        Value::String(value) => {
            if !value.is_empty() {
                texts.push(value.clone());
            }
        }
        other => collect_message_parts(other, &mut texts, &mut flags),
    }
    let text = texts.join(" ");
    if !text.is_empty() {
        flags.insert("text");
    }
    if has_tools || text.contains("<tool_call>") {
        flags.insert("tool_call");
    }
    let modalities = ["text", "image", "audio", "tool_call"]
        .into_iter()
        .filter(|name| flags.contains(name))
        .map(str::to_string)
        .collect();
    ExtractedMessage {
        char_count: text.chars().count() as u64,
        text,
        modalities,
    }
}

fn collect_message_parts(
    value: &Value,
    texts: &mut Vec<String>,
    flags: &mut BTreeSet<&'static str>,
) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_message_parts(item, texts, flags);
            }
        }
        Value::Object(map) => {
            if let Some(Value::String(text)) = map.get("text") {
                if !text.is_empty() {
                    texts.push(text.clone());
                }
            }
            if let Some(Value::String(content)) = map.get("content") {
                if !content.is_empty() {
                    texts.push(content.clone());
                }
            }
            if value_present(map.get("image"))
                || value_present(map.get("image_url"))
                || value_present(map.get("image_bytes"))
            {
                flags.insert("image");
            }
            if value_present(map.get("audio")) || value_present(map.get("input_audio")) {
                flags.insert("audio");
            }
            if let Some(kind) = map.get("type").and_then(Value::as_str) {
                match kind {
                    "image" | "image_url" => {
                        flags.insert("image");
                    }
                    "audio" | "input_audio" => {
                        flags.insert("audio");
                    }
                    _ => {}
                }
            }
            for (key, child) in map {
                if key == "text" || (key == "content" && child.is_string()) {
                    continue;
                }
                collect_message_parts(child, texts, flags);
            }
        }
        _ => {}
    }
}

fn value_present(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::String(value)) => !value.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
        Some(Value::Bool(_) | Value::Number(_)) => true,
    }
}

fn compact(value: &str, limit: usize) -> String {
    let single = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if single.chars().count() <= limit {
        single
    } else {
        format!(
            "{}…",
            single
                .chars()
                .take(limit.saturating_sub(1))
                .collect::<String>()
        )
    }
}

fn turn_has_error(item: &TrajectoryTurnView, events: &[&EventRecord]) -> bool {
    item.turn.kind.as_deref().is_some_and(explicit_error_text)
        || events.iter().any(|event| {
            explicit_error_text(&event.kind) || value_has_explicit_error(&event.payload)
        })
}

fn explicit_error_text(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "error" | "failed" | "failure" | "tool.error" | "llm.error"
    )
}

fn value_has_explicit_error(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.get("status_code")
                .and_then(Value::as_u64)
                .is_some_and(|status| status >= 400)
                || map.get("error_type").is_some_and(|value| {
                    !value.is_null() && value.as_str().is_none_or(|value| !value.is_empty())
                })
                || map
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some_and(explicit_error_text)
                || map.values().any(value_has_explicit_error)
        }
        Value::Array(values) => values.iter().any(value_has_explicit_error),
        _ => false,
    }
}

fn first_number(values: &[&Value], keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|key| values.iter().find_map(|value| find_number(value, key)))
}

fn find_number(value: &Value, key: &str) -> Option<f64> {
    match value {
        Value::Object(map) => map
            .get(key)
            .and_then(number_value)
            .or_else(|| map.values().find_map(|value| find_number(value, key))),
        Value::Array(values) => values.iter().find_map(|value| find_number(value, key)),
        _ => None,
    }
}

fn number_value(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn first_string(values: &[&Value], keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| values.iter().find_map(|value| find_string(value, key)))
}

fn find_string(value: &Value, key: &str) -> Option<String> {
    match value {
        Value::Object(map) => map
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| map.values().find_map(|value| find_string(value, key))),
        Value::Array(values) => values.iter().find_map(|value| find_string(value, key)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squashed_store_does_not_use_run_id_as_a_folder() {
        assert_eq!(
            explorer_run_path(
                "default",
                ".",
                "13f9aec9-0e2a-4bdf-baf6-48b58f5715fc",
                "13f9aec9-0e2a-4bdf-baf6-48b58f5715fc",
                Some("cybergym_0729001"),
                None,
            ),
            "default/13f9aec9-0e2a-4bdf-baf6-48b58f5715fc"
        );
    }

    #[test]
    fn squashed_store_nests_only_real_parent_sessions() {
        assert_eq!(
            explorer_run_path(
                "default",
                ".",
                "Energy_001#1",
                "Energy_001#1",
                Some("Energy_001"),
                Some("Energy_001"),
            ),
            "default/Energy_001/subagents/Energy_001#1"
        );
    }

    #[test]
    fn preserved_file_sources_keep_the_source_path() {
        assert_eq!(
            explorer_run_path(
                "dataset",
                "gateway.json",
                "json-session",
                "json-session",
                Some("json-job"),
                None,
            ),
            "dataset/gateway.json/json-job/json-session"
        );
    }

    #[test]
    fn percentiles_report_coverage_without_inventing_missing_samples() {
        let stats = metric_stats(vec![10.0, 20.0, 30.0, 40.0], 8);
        assert_eq!(stats.sample_count, 4);
        assert_eq!(stats.total_count, 8);
        assert_eq!(stats.p50, Some(30.0));
        assert_eq!(stats.p95, Some(40.0));
    }

    #[test]
    fn explicit_errors_do_not_match_arbitrary_message_text() {
        assert!(!value_has_explicit_error(
            &serde_json::json!({"content":"the word error appears"})
        ));
        assert!(value_has_explicit_error(
            &serde_json::json!({"status_code":500})
        ));
    }

    #[test]
    fn latency_histogram_uses_stable_non_overlapping_buckets() {
        let buckets = latency_histogram(&[50.0, 100.0, 499.0, 500.0, 1_000.0, 12_000.0]);
        assert_eq!(
            buckets
                .iter()
                .map(|bucket| bucket.count)
                .collect::<Vec<_>>(),
            vec![1, 2, 1, 1, 0, 1]
        );
        assert_eq!(buckets.iter().map(|bucket| bucket.count).sum::<usize>(), 6);
    }

    #[test]
    fn token_counts_derive_total_without_hiding_partial_coverage() {
        let value = serde_json::json!({"usage":{"input_tokens":12,"output_tokens":5}});
        assert_eq!(token_counts(&[&value]), (Some(12), Some(5), Some(17)));
        let empty = serde_json::json!({"usage":{}});
        assert_eq!(token_counts(&[&empty]), (None, None, None));
    }

    #[test]
    fn multimodal_null_media_fields_do_not_hide_text() {
        let message = serde_json::json!([{
            "image_bytes": null,
            "image_url": null,
            "input_audio": null,
            "text": "Please continue on whatever approach you think is suitable"
        }]);
        let extracted = extract_message_content(&message, false);
        assert_eq!(
            extracted.text,
            "Please continue on whatever approach you think is suitable"
        );
        assert_eq!(extracted.char_count, extracted.text.chars().count() as u64);
        assert_eq!(extracted.modalities, vec!["text"]);
    }

    #[test]
    fn string_message_is_plain_text() {
        let extracted = extract_message_content(&serde_json::json!("hello world"), false);
        assert_eq!(extracted.text, "hello world");
        assert_eq!(extracted.char_count, 11);
        assert_eq!(extracted.modalities, vec!["text"]);
    }

    #[test]
    fn nonempty_image_without_text_is_image_only() {
        let extracted = extract_message_content(
            &serde_json::json!([{"type":"image_url","image_url":"https://ex/a.png"}]),
            false,
        );
        assert_eq!(extracted.text, "");
        assert_eq!(extracted.char_count, 0);
        assert_eq!(extracted.modalities, vec!["image"]);
    }

    #[test]
    fn tool_calls_and_markup_mark_tool_modality() {
        let from_names = extract_message_content(&serde_json::json!("ok"), true);
        assert_eq!(from_names.modalities, vec!["text", "tool_call"]);
        let from_markup = extract_message_content(
            &serde_json::json!("<tool_call>execute_bash\n<parameter=command>ls</parameter>"),
            false,
        );
        assert!(from_markup.modalities.contains(&"tool_call".to_string()));
        assert!(from_markup.modalities.contains(&"text".to_string()));
    }

    fn sample_run(dataset: &str, file: &str, status: &str, session: &str) -> RunSummary {
        RunSummary {
            dataset: dataset.into(),
            file: file.into(),
            document_id: session.into(),
            run_id: None,
            agent_id: "agent".into(),
            model_name: None,
            session_id: session.into(),
            root_session_id: None,
            path: format!("{dataset}/{file}/{session}"),
            row_count: 1,
            duplicate_event_ids: 0,
            status: status.into(),
        }
    }

    #[test]
    fn warehouse_tree_sizes_datasets_by_run_count() {
        let tree = catalog_tree(
            &[
                sample_run("evals", "a.json", "completed", "s1"),
                sample_run("evals", "b.json", "failed", "s2"),
                sample_run("archive", "c.json", "completed", "s3"),
            ],
            None,
            "",
            16,
        );
        assert_eq!(tree.dataset, None);
        assert_eq!(tree.run_count, 3);
        assert_eq!(tree.failed_count, 1);
        let names: Vec<_> = tree
            .children
            .iter()
            .map(|child| (child.name.as_str(), child.kind.as_str(), child.run_count))
            .collect();
        assert_eq!(
            names,
            vec![("evals", "dataset", 2), ("archive", "dataset", 1)]
        );
    }

    #[test]
    fn dataset_tree_groups_the_next_file_segment() {
        let tree = catalog_tree(
            &[
                sample_run("evals", "gsm8k/train/events.lance", "completed", "s1"),
                sample_run("evals", "gsm8k/test/events.lance", "failed", "s2"),
                sample_run("evals", "mmlu.json", "completed", "s3"),
                sample_run("archive", "skip.json", "completed", "s4"),
            ],
            Some("evals"),
            "",
            16,
        );
        assert_eq!(tree.dataset.as_deref(), Some("evals"));
        assert_eq!(tree.run_count, 3);
        assert_eq!(tree.failed_count, 1);
        assert_eq!(
            tree.children
                .iter()
                .map(|child| (
                    child.name.as_str(),
                    child.kind.as_str(),
                    child.path.as_str(),
                    child.run_count
                ))
                .collect::<Vec<_>>(),
            vec![
                ("gsm8k", "dir", "gsm8k", 2),
                ("mmlu.json", "file", "mmlu.json", 1),
            ]
        );

        let nested = catalog_tree(
            &[
                sample_run("evals", "gsm8k/train/events.lance", "completed", "s1"),
                sample_run("evals", "gsm8k/test/events.lance", "failed", "s2"),
            ],
            Some("evals"),
            "gsm8k",
            16,
        );
        assert_eq!(nested.prefix, "gsm8k");
        assert_eq!(
            nested
                .children
                .iter()
                .map(|child| child.name.as_str())
                .collect::<Vec<_>>(),
            vec!["test", "train"]
        );
    }

    #[test]
    fn exact_file_prefix_has_no_children() {
        let tree = catalog_tree(
            &[sample_run("evals", "mmlu.json", "completed", "s1")],
            Some("evals"),
            "mmlu.json",
            16,
        );
        assert_eq!(tree.run_count, 1);
        assert!(tree.children.is_empty());
    }

    #[test]
    fn tree_folds_the_tail_into_other() {
        let runs: Vec<_> = (0..5)
            .map(|index| {
                sample_run(
                    "evals",
                    &format!("f{index}.json"),
                    "completed",
                    &format!("s{index}"),
                )
            })
            .collect();
        let tree = catalog_tree(&runs, Some("evals"), "", 3);
        assert_eq!(tree.children.len(), 3);
        let other = tree.children.last().unwrap();
        assert_eq!(other.kind, "other");
        assert_eq!(other.run_count, 3);
        assert_eq!(
            other
                .entries
                .iter()
                .map(|child| child.name.as_str())
                .collect::<Vec<_>>(),
            vec!["f2.json", "f3.json", "f4.json"]
        );
    }

    #[test]
    fn run_page_file_prefix_is_not_run_path() {
        let summaries = vec![
            sample_run("evals", "gsm8k/train/events.lance", "completed", "s1"),
            sample_run("evals", "gsm8k/test/events.lance", "completed", "s2"),
            sample_run("evals", "mmlu.json", "completed", "s3"),
        ];
        let page = run_page(
            summaries,
            &ExplorerRunsQuery {
                dataset: Some("evals".into()),
                file: Some("gsm8k".into()),
                limit: Some(50),
                ..ExplorerRunsQuery::default()
            },
        );
        assert_eq!(page.snapshot.total, 2);
        assert!(page
            .records
            .iter()
            .all(|item| item.run.file.starts_with("gsm8k")));
        assert_eq!(page.path_index.len(), 2);
    }

    fn source_seed(dataset: &str, file: &str, status: &str) -> SourceSeed {
        SourceSeed {
            dataset: dataset.into(),
            file: file.into(),
            format: Some("canonical-event".into()),
            kind: "store".into(),
            snapshot_ref: Some("manifest-revision:184".into()),
            projection_status: Some("fresh".into()),
            projection_generation: Some("42".into()),
            size_bytes: Some(2_400_000_000),
            last_modified: Some("2026-08-23T14:32:00Z".into()),
            status: status.into(),
            error: None,
        }
    }

    #[test]
    fn warehouse_sources_join_run_counts_across_datasets_and_keep_every_source() {
        let page = warehouse_sources(
            &[
                source_seed("evals", "gateway/capture", "ready"),
                source_seed("evals", "experiments/v4", "ready"),
                source_seed("archive", "old.json", "error"),
            ],
            &[
                sample_run("evals", "gateway/capture", "completed", "s1"),
                sample_run("evals", "gateway/capture", "completed", "s2"),
                sample_run("evals", "experiments/v4", "failed", "s3"),
                sample_run("archive", "unrelated.json", "completed", "s4"),
            ],
        );
        assert_eq!(page.run_count, 4);
        assert_eq!(page.failed_count, 1);
        assert_eq!(page.source_count, 3);
        assert_eq!(page.error_sources, 1);
        assert_eq!(page.sources.len(), 3);
        assert_eq!(page.sources[0].dataset, "evals");
        assert_eq!(page.sources[0].file, "gateway/capture");
        assert_eq!(page.sources[0].run_count, 2);
        assert_eq!(page.sources[0].failed_count, 0);
        let old = page
            .sources
            .iter()
            .find(|source| source.file == "old.json")
            .expect("keep catalog sources with zero runs");
        assert_eq!(old.dataset, "archive");
        assert_eq!(old.run_count, 0);
        assert_eq!(old.status, "error");
    }
}
