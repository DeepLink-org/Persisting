use std::collections::{BTreeMap, BTreeSet};

use persisting_pchronicle::EventRecord;
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
    pub(crate) sort: Option<String>,
    pub(crate) direction: Option<String>,
    pub(crate) offset: Option<usize>,
    pub(crate) limit: Option<usize>,
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
    pub(crate) turn: persisting_pchronicle::StorylineTurn,
    pub(crate) wire_tool_calls: Vec<WireToolCall>,
    pub(crate) events: Vec<EventRecord>,
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
        if let Some(timestamp) = item
            .turn
            .timestamp
            .as_ref()
            .filter(|value| !value.is_empty())
        {
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

    timestamps.sort();
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
        start_timestamp: timestamps.first().cloned(),
        end_timestamp: timestamps.last().cloned(),
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
    let text = match &item.turn.message {
        Value::String(value) => value.clone(),
        value => serde_json::to_string(value).unwrap_or_default(),
    };
    let preview = compact(&text, 220);
    TurnSummary {
        id: item.turn.id,
        source: item.turn.source.clone(),
        kind: item.turn.kind.clone(),
        timestamp: item.turn.timestamp.clone(),
        call_id: item.call_id.clone(),
        preview,
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
        tool_names: display_tool_calls(item)
            .into_iter()
            .map(|(name, _)| name)
            .collect(),
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

fn display_tool_calls(item: &TrajectoryTurnView) -> Vec<(String, Option<f64>)> {
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
}
