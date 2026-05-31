//! Per-turn one-line summary for `trajectory stats --detail` (tree-aware subagents).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use persisting_capture::record::CaptureRecord;
use persisting_proto::TrajectoryStatsResponse;
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct TurnDetail {
    pub index: usize,
    pub user_chars: usize,
    pub assistant_chars: usize,
    pub completion_tokens: Option<u64>,
    pub ttft_ms: Option<u64>,
    pub tpot_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct SpawnLinkInfo {
    pub subagent_id: String,
    pub subagent_trajectory: String,
    pub subagent_type: Option<String>,
    pub description: Option<String>,
}

impl SpawnLinkInfo {
    pub fn storage_session_id(&self) -> String {
        let leaf = self
            .subagent_trajectory
            .rsplit('/')
            .next()
            .unwrap_or(self.subagent_trajectory.as_str());
        leaf.strip_suffix(".md").unwrap_or(leaf).to_string()
    }

    pub fn label(&self) -> String {
        let base = match &self.subagent_type {
            Some(t) => format!("{} ({t})", self.subagent_id),
            None => self.subagent_id.clone(),
        };
        match &self.description {
            Some(d) if !d.is_empty() => {
                let short = if d.chars().count() > 48 {
                    format!("{}…", d.chars().take(45).collect::<String>())
                } else {
                    d.clone()
                };
                format!("{base}: {short}")
            }
            _ => base,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TurnNode {
    pub turn: TurnDetail,
    pub spawn_links: Vec<SpawnLinkInfo>,
    pub subagents: Vec<DetailNode>,
}

#[derive(Debug, Clone)]
pub struct DetailNode {
    pub label: String,
    /// Storage-relative markdown path (`agent-….md`); set for subagent nodes.
    pub trajectory: Option<String>,
    pub summary: SessionDetailSummary,
    pub turns: Vec<TurnNode>,
}

#[derive(Debug, Clone, Default)]
pub struct SessionDetailSummary {
    pub turn_count: usize,
    pub block_count: usize,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub models: Vec<String>,
}

#[derive(Debug, Clone)]
struct ParsedRecord {
    kind: String,
    content: String,
    model: Option<String>,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    timestamp: Option<String>,
    ttft_ms: Option<u64>,
    stream_partial: bool,
    call_id: Option<String>,
    spawn_links: Vec<SpawnLinkInfo>,
}

#[derive(Debug, Clone)]
struct TurnAccumulator {
    call_id: Option<String>,
    items: Vec<ParsedRecord>,
    spawn_links: Vec<SpawnLinkInfo>,
}

pub fn analyze_session_tree(records: &[String]) -> Result<(SessionDetailSummary, Vec<TurnNode>)> {
    let parsed: Vec<ParsedRecord> = records
        .iter()
        .enumerate()
        .map(|(i, line)| parse_record_line(line).with_context(|| format!("records[{i}]")))
        .collect::<Result<Vec<_>>>()?;

    let mut accumulators: Vec<TurnAccumulator> = Vec::new();
    let mut summary = SessionDetailSummary {
        block_count: records.len(),
        ..Default::default()
    };
    let mut models = std::collections::BTreeSet::new();
    let mut pending_request: Option<ParsedRecord> = None;
    let mut detail_index = 0usize;
    let mut seen_spawns = HashSet::new();
    let mut pending_spawns: std::collections::HashMap<String, Vec<SpawnLinkInfo>> =
        std::collections::HashMap::new();

    for rec in parsed {
        match rec.kind.as_str() {
            "llm.spawn_link" => {
                let fresh = take_new_spawns(&mut seen_spawns, &rec.spawn_links);
                if fresh.is_empty() {
                    continue;
                }
                match attach_spawns_to_call(
                    &mut accumulators,
                    &mut pending_spawns,
                    rec.call_id.clone(),
                    fresh,
                ) {
                    AttachOutcome::Done => {}
                    AttachOutcome::Fallback(spawns) => {
                        if let Some(last) = accumulators.last_mut() {
                            last.spawn_links.extend(spawns);
                        }
                    }
                }
            }
            "llm.request" => {
                if let Some(prev) = pending_request.take() {
                    flush_accumulator(
                        &mut accumulators,
                        TurnAccumulator {
                            call_id: prev.call_id.clone(),
                            items: vec![prev],
                            spawn_links: Vec::new(),
                        },
                        &mut pending_spawns,
                    );
                }
                pending_request = Some(rec);
            }
            "llm.response" | "llm.response.stream" => {
                if rec.stream_partial {
                    continue;
                }
                if let Some(req) = pending_request.take() {
                    let call_id = req.call_id.clone().or_else(|| rec.call_id.clone());
                    let spawns = take_new_spawns(&mut seen_spawns, &rec.spawn_links);
                    flush_accumulator(
                        &mut accumulators,
                        TurnAccumulator {
                            call_id,
                            items: vec![req, rec.clone()],
                            spawn_links: spawns,
                        },
                        &mut pending_spawns,
                    );
                } else if let Some(call_id) = rec.call_id.clone() {
                    let spawns = take_new_spawns(&mut seen_spawns, &rec.spawn_links);
                    if !merge_response_into_call(
                        &mut accumulators,
                        &call_id,
                        rec.clone(),
                        spawns.clone(),
                    ) {
                        flush_accumulator(
                            &mut accumulators,
                            TurnAccumulator {
                                call_id: rec.call_id.clone(),
                                items: vec![rec.clone()],
                                spawn_links: spawns,
                            },
                            &mut pending_spawns,
                        );
                    }
                } else {
                    let spawns = take_new_spawns(&mut seen_spawns, &rec.spawn_links);
                    flush_accumulator(
                        &mut accumulators,
                        TurnAccumulator {
                            call_id: rec.call_id.clone(),
                            items: vec![rec.clone()],
                            spawn_links: spawns,
                        },
                        &mut pending_spawns,
                    );
                }
            }
            _ => {}
        }
    }
    if let Some(req) = pending_request.take() {
        flush_accumulator(
            &mut accumulators,
            TurnAccumulator {
                call_id: req.call_id.clone(),
                items: vec![req],
                spawn_links: Vec::new(),
            },
            &mut pending_spawns,
        );
    }

    let mut turns = Vec::new();
    for acc in accumulators {
        detail_index += 1;
        let turn = build_turn_node(detail_index, &acc.items);
        if let Some(c) = turn.completion_tokens {
            summary.total_completion_tokens += c;
        }
        for item in &acc.items {
            if let Some(p) = item.prompt_tokens {
                summary.total_prompt_tokens += p;
            }
            if let Some(ref m) = item.model {
                models.insert(m.clone());
            }
        }
        turns.push(TurnNode {
            turn,
            spawn_links: acc.spawn_links,
            subagents: Vec::new(),
        });
    }

    summary.turn_count = turns.len();
    summary.models = models.into_iter().collect();
    Ok((summary, turns))
}

enum AttachOutcome {
    Done,
    Fallback(Vec<SpawnLinkInfo>),
}

fn attach_spawns_to_call(
    accumulators: &mut [TurnAccumulator],
    pending_spawns: &mut std::collections::HashMap<String, Vec<SpawnLinkInfo>>,
    parent: Option<String>,
    fresh: Vec<SpawnLinkInfo>,
) -> AttachOutcome {
    let Some(parent) = parent else {
        return AttachOutcome::Fallback(fresh);
    };
    if let Some(acc) = accumulators
        .iter_mut()
        .find(|a| a.call_id.as_deref() == Some(parent.as_str()))
    {
        acc.spawn_links.extend(fresh);
    } else {
        pending_spawns.entry(parent).or_default().extend(fresh);
    }
    AttachOutcome::Done
}

fn merge_response_into_call(
    accumulators: &mut [TurnAccumulator],
    call_id: &str,
    rec: ParsedRecord,
    spawns: Vec<SpawnLinkInfo>,
) -> bool {
    let Some(acc) = accumulators
        .iter_mut()
        .find(|a| a.call_id.as_deref() == Some(call_id))
    else {
        return false;
    };
    acc.items.push(rec);
    acc.spawn_links.extend(spawns);
    true
}

fn flush_accumulator(
    accumulators: &mut Vec<TurnAccumulator>,
    mut acc: TurnAccumulator,
    pending_spawns: &mut std::collections::HashMap<String, Vec<SpawnLinkInfo>>,
) {
    if let Some(call_id) = acc.call_id.clone() {
        if let Some(queued) = pending_spawns.remove(&call_id) {
            acc.spawn_links.extend(queued);
        }
    }
    accumulators.push(acc);
}

fn take_new_spawns(seen: &mut HashSet<String>, links: &[SpawnLinkInfo]) -> Vec<SpawnLinkInfo> {
    let mut fresh = Vec::new();
    for link in links {
        if seen.insert(link.subagent_trajectory.clone()) {
            fresh.push(link.clone());
        }
    }
    fresh
}

fn build_turn_node(index: usize, items: &[ParsedRecord]) -> TurnDetail {
    let mut user_chars = 0usize;
    let mut assistant_chars = 0usize;
    let mut completion_tokens = None;
    let mut ttft_ms = None;
    let mut request_ts = None;
    let mut response_ts = None;

    for rec in items {
        match rec.kind.as_str() {
            "llm.request" => {
                user_chars = rec.content.chars().count();
                request_ts = rec.timestamp.clone();
            }
            "llm.response" | "llm.response.stream" => {
                assistant_chars += rec.content.chars().count();
                completion_tokens = rec.completion_tokens.or(completion_tokens);
                if ttft_ms.is_none() {
                    ttft_ms = rec.ttft_ms;
                }
                response_ts = rec.timestamp.clone();
            }
            _ => {
                if user_chars == 0 {
                    user_chars = rec.content.chars().count();
                    request_ts = rec.timestamp.clone();
                } else if assistant_chars == 0 {
                    assistant_chars += rec.content.chars().count();
                    response_ts = rec.timestamp.clone();
                }
            }
        }
    }

    let total_ms = wall_ms(request_ts.as_deref(), response_ts.as_deref());
    let ttft = ttft_ms.or(total_ms);
    let tpot = compute_tpot_ms(total_ms, ttft, completion_tokens);

    TurnDetail {
        index,
        user_chars,
        assistant_chars,
        completion_tokens,
        ttft_ms: ttft,
        tpot_ms: tpot,
    }
}

/// Attach subagent subtrees; `load_subagent` returns replay records for one spawn link.
pub fn attach_subagent_trees(
    turns: &mut [TurnNode],
    load_subagent: &mut impl FnMut(&SpawnLinkInfo) -> Result<Option<Vec<String>>>,
    cache: &mut std::collections::HashMap<String, DetailNode>,
) -> Result<()> {
    for turn in turns.iter_mut() {
        let mut seen = HashSet::new();
        for link in &turn.spawn_links {
            if !seen.insert(link.subagent_trajectory.clone()) {
                continue;
            }
            let node = if let Some(cached) = cache.get(&link.subagent_trajectory) {
                cached.clone()
            } else if let Some(records) = load_subagent(link)? {
                let (summary, mut sub_turns) = analyze_session_tree(&records)?;
                attach_subagent_trees(&mut sub_turns, load_subagent, cache)?;
                let node = DetailNode {
                    label: link.label(),
                    trajectory: Some(link.subagent_trajectory.clone()),
                    summary,
                    turns: sub_turns,
                };
                cache.insert(link.subagent_trajectory.clone(), node.clone());
                node
            } else {
                DetailNode {
                    label: format!("{} (missing)", link.label()),
                    trajectory: Some(link.subagent_trajectory.clone()),
                    summary: SessionDetailSummary::default(),
                    turns: Vec::new(),
                }
            };
            turn.subagents.push(node);
        }
    }
    Ok(())
}

pub fn build_detail_node(
    label: impl Into<String>,
    records: &[String],
    load_subagent: &mut impl FnMut(&SpawnLinkInfo) -> Result<Option<Vec<String>>>,
) -> Result<DetailNode> {
    let (summary, mut turns) = analyze_session_tree(records)?;
    let mut cache = std::collections::HashMap::new();
    attach_subagent_trees(&mut turns, load_subagent, &mut cache)?;
    Ok(DetailNode {
        label: label.into(),
        trajectory: None,
        summary,
        turns,
    })
}

fn wall_ms(start: Option<&str>, end: Option<&str>) -> Option<u64> {
    let t0 = parse_ts(start)?;
    let t1 = parse_ts(end)?;
    let ms = (t1 - t0).num_milliseconds();
    if ms >= 0 {
        Some(ms as u64)
    } else {
        None
    }
}

fn parse_ts(s: Option<&str>) -> Option<DateTime<Utc>> {
    s.and_then(|raw| {
        DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    })
}

fn compute_tpot_ms(
    total_ms: Option<u64>,
    ttft_ms: Option<u64>,
    out_tokens: Option<u64>,
) -> Option<u64> {
    let total = total_ms?;
    let out = out_tokens.filter(|&n| n > 0)?;
    match ttft_ms {
        Some(ttft) if out > 1 && total > ttft => Some((total - ttft) / (out - 1)),
        Some(ttft) if out == 1 => Some(total.saturating_sub(ttft)),
        _ => Some(total / out),
    }
}

fn parse_record_line(line: &str) -> Result<ParsedRecord> {
    let line = line.trim();
    let v: serde_json::Value = serde_json::from_str(line).context("parse replay JSON")?;
    if let Ok(rec) = serde_json::from_value::<CaptureRecord>(v.clone()) {
        return parsed_from_capture_record(&rec);
    }
    let obj = v.as_object().context("replay record must be JSON object")?;
    let kind = json_str(obj, "kind").unwrap_or_else(|| "markdown".into());
    let content = json_str(obj, "content").unwrap_or_default();
    let mut call_id = json_str(obj, "call_id");
    let mut spawn_links = extract_spawn_links(obj);

    if kind == "llm.spawn_link" {
        if let Ok(body) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(body_obj) = body.as_object() {
                call_id = call_id.or_else(|| json_str(body_obj, "parent_call_id"));
                if spawn_links.is_empty() {
                    spawn_links = extract_spawn_links(body_obj);
                }
            }
        }
    }

    Ok(ParsedRecord {
        kind,
        content,
        model: json_str(obj, "model"),
        prompt_tokens: json_u64(obj, "prompt_tokens").or_else(|| json_u64(obj, "input_tokens")),
        completion_tokens: json_u64(obj, "completion_tokens")
            .or_else(|| json_u64(obj, "output_tokens")),
        timestamp: json_str(obj, "timestamp"),
        ttft_ms: json_u64(obj, "ttft_ms"),
        stream_partial: obj
            .get("stream_partial")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        call_id,
        spawn_links,
    })
}

fn parsed_from_capture_record(rec: &CaptureRecord) -> Result<ParsedRecord> {
    let content = match rec.kind.as_str() {
        "llm.request" => rec.visible_user_text().unwrap_or_default(),
        "llm.response" | "llm.response.stream" => rec.visible_assistant_text().unwrap_or_default(),
        _ => rec
            .payload
            .get("content")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| rec.payload.to_string()),
    };
    let usage = rec
        .payload
        .get("usage")
        .or_else(|| rec.payload.get("body").and_then(|b| b.get("usage")));
    let prompt_tokens = usage
        .and_then(|u| token_field(u, "prompt_tokens").or_else(|| token_field(u, "input_tokens")));
    let completion_tokens = usage.and_then(|u| {
        token_field(u, "completion_tokens").or_else(|| token_field(u, "output_tokens"))
    });
    let model = rec
        .payload
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let spawn_links = rec
        .payload
        .get("spawn_links")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(parse_spawn_link).collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(ParsedRecord {
        kind: rec.kind.clone(),
        content,
        model,
        prompt_tokens,
        completion_tokens,
        timestamp: rec.timestamp.clone(),
        ttft_ms: rec.payload.get("ttft_ms").and_then(json_u64_value),
        stream_partial: rec
            .payload
            .get("stream_partial")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        call_id: rec.call_id.clone(),
        spawn_links,
    })
}

fn token_field(usage: &serde_json::Value, key: &str) -> Option<u64> {
    usage.get(key).and_then(json_u64_value)
}

fn json_u64_value(v: &serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn extract_spawn_links(obj: &serde_json::Map<String, serde_json::Value>) -> Vec<SpawnLinkInfo> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    if let Some(arr) = obj.get("spawn_links").and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(link) = parse_spawn_link(item) {
                if seen.insert(link.subagent_trajectory.clone()) {
                    out.push(link);
                }
            }
        }
    }

    out
}

fn parse_spawn_link(v: &serde_json::Value) -> Option<SpawnLinkInfo> {
    let o = v.as_object()?;
    Some(SpawnLinkInfo {
        subagent_id: json_str(o, "subagent_id")?,
        subagent_trajectory: json_str(o, "subagent_trajectory")?,
        subagent_type: json_str(o, "subagent_type"),
        description: json_str(o, "description"),
    })
}

fn json_str(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    obj.get(key).and_then(|v| match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

fn json_u64(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<u64> {
    obj.get(key).and_then(|v| match v {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    })
}

fn fmt_ms(ms: Option<u64>) -> String {
    match ms {
        None => "-".into(),
        Some(v) if v >= 10_000 => format!("{:.1}s", v as f64 / 1000.0),
        Some(v) if v >= 1_000 => format!("{:.2}s", v as f64 / 1000.0),
        Some(v) => format!("{v}ms"),
    }
}

fn fmt_tpot(ms: Option<u64>) -> String {
    match ms {
        None => "-".into(),
        Some(v) => format!("{v}ms/tok"),
    }
}

fn format_turn_line(t: &TurnDetail) -> String {
    let out_tok = t
        .completion_tokens
        .map(|n| n.to_string())
        .unwrap_or_else(|| "-".into());
    format!(
        "turn {:>2}  user {:>5}c  model {:>5}c  TTFT {:>7}  TPOT {:>10}  out {} tok",
        t.index,
        t.user_chars,
        t.assistant_chars,
        fmt_ms(t.ttft_ms),
        fmt_tpot(t.tpot_ms),
        out_tok,
    )
}

fn print_detail_node(node: &DetailNode, depth: usize) {
    let indent = "  ".repeat(depth);
    if depth > 0 {
        println!("{indent}└─ {}", node.label);
    }
    for turn in &node.turns {
        println!("{indent}  {}", format_turn_line(&turn.turn));
        for sub in &turn.subagents {
            print_detail_node(sub, depth + 1);
        }
    }
}

fn aggregate_summary(node: &DetailNode) -> SessionDetailSummary {
    let mut total = node.summary.clone();
    let mut seen = HashSet::new();
    merge_subagent_summaries(node, &mut total, &mut seen);
    total
}

fn merge_subagent_summaries(
    node: &DetailNode,
    total: &mut SessionDetailSummary,
    seen: &mut HashSet<String>,
) {
    for turn in &node.turns {
        for sub in &turn.subagents {
            let key = sub.trajectory.clone().unwrap_or_else(|| sub.label.clone());
            if !seen.insert(key) {
                continue;
            }
            total.turn_count += sub.summary.turn_count;
            total.block_count += sub.summary.block_count;
            total.total_prompt_tokens += sub.summary.total_prompt_tokens;
            total.total_completion_tokens += sub.summary.total_completion_tokens;
            for m in &sub.summary.models {
                if !total.models.contains(m) {
                    total.models.push(m.clone());
                }
            }
            merge_subagent_summaries(sub, total, seen);
        }
    }
}

fn count_unique_subagents(node: &DetailNode) -> usize {
    let mut seen = HashSet::new();
    collect_subagent_trajectories(node, &mut seen);
    seen.len()
}

fn collect_subagent_trajectories(node: &DetailNode, seen: &mut HashSet<String>) {
    for turn in &node.turns {
        for sub in &turn.subagents {
            if let Some(ref path) = sub.trajectory {
                seen.insert(path.clone());
            }
            collect_subagent_trajectories(sub, seen);
        }
    }
}

pub fn print_trajectory_stats_detail(
    stats: &TrajectoryStatsResponse,
    root: &DetailNode,
) -> Result<()> {
    if stats.status != "ok" {
        println!(
            "status={} row_count={} note={}",
            stats.status, stats.row_count, stats.note
        );
        return Ok(());
    }

    let agg = aggregate_summary(root);
    let subagents = count_unique_subagents(root);
    let models = if agg.models.is_empty() {
        "-".to_string()
    } else {
        agg.models.join(", ")
    };
    let turn_line = if subagents > 0 {
        format!("{} turns ({} subagents)", agg.turn_count, subagents)
    } else {
        format!("{} turns", agg.turn_count)
    };
    println!(
        "session {} | {} | {} blocks | prompt {} tok | completion {} tok | models: {}",
        stats.session_id,
        turn_line,
        agg.block_count,
        agg.total_prompt_tokens,
        agg.total_completion_tokens,
        models,
    );
    println!("# tree: turn  user  model  TTFT  TPOT  out_tok  (+ subagent branches)");
    print_detail_node(root, 0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_tpot_from_ttft_and_total() {
        let tpot = compute_tpot_ms(Some(5000), Some(1000), Some(5)).unwrap();
        assert_eq!(tpot, 1000);
    }

    #[test]
    fn extracts_spawn_links() {
        let records = vec![
            r#"{"kind":"llm.request","role":"user","content":"go"}"#.into(),
            r#"{"kind":"llm.response.stream","role":"assistant","content":"spawned","spawn_links":[{"subagent_type":"Explore","subagent_id":"abc","subagent_trajectory":"agent-abc.md"}]}"#.into(),
        ];
        let (_, turns) = analyze_session_tree(&records).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].spawn_links.len(), 1);
        assert_eq!(turns[0].spawn_links[0].storage_session_id(), "agent-abc");
    }

    #[test]
    fn pairs_request_response() {
        let records = vec![
            r#"{"kind":"llm.request","role":"user","content":"你好","timestamp":"2026-01-01T00:00:00.000Z"}"#.into(),
            r#"{"kind":"llm.response.stream","role":"assistant","completion_tokens":18,"content":"你好！","timestamp":"2026-01-01T00:00:02.000Z","ttft_ms":500}"#.into(),
        ];
        let (_, turns) = analyze_session_tree(&records).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].turn.user_chars, 2);
        assert_eq!(turns[0].turn.ttft_ms, Some(500));
    }

    #[test]
    fn pairs_by_replay_order_not_capture_turn_metadata() {
        // capture writes turn=seq/2+1 so request/response often land on different turn numbers
        let records = vec![
            r#"{"kind":"llm.request","content":"same","turn":7}"#.into(),
            r#"{"kind":"llm.response.stream","content":"reply","completion_tokens":10,"turn":8}"#
                .into(),
        ];
        let (_, turns) = analyze_session_tree(&records).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].turn.user_chars, 4);
        assert_eq!(turns[0].turn.assistant_chars, 5);
        assert_eq!(turns[0].turn.completion_tokens, Some(10));
    }

    #[test]
    fn spawn_site_ignores_backfilled_refs_on_later_turns() {
        let link = r#"{"subagent_type":"Explore","subagent_id":"aeb","subagent_trajectory":"agent-aeb.md","description":"task"}"#;
        let records = vec![
            r#"{"kind":"llm.request","content":"hi","call_id":"call-a"}"#.into(),
            r#"{"kind":"llm.response.stream","content":"go","call_id":"call-a"}"#.into(),
            format!(r#"{{"kind":"llm.spawn_link","call_id":"call-a","spawn_links":[{link}]}}"#),
            format!(
                r#"{{"kind":"llm.response.stream","content":"done","call_id":"call-a","spawn_links":[{link}]}}"#
            ),
            r#"{"kind":"llm.request","content":"next"}"#.into(),
        ];
        let (_, turns) = analyze_session_tree(&records).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].spawn_links.len(), 1);
        assert_eq!(turns[0].turn.assistant_chars, 6);
        assert!(turns[1].spawn_links.is_empty());
    }

    #[test]
    fn parses_vortex_capture_record_replay_json() {
        let records = vec![
            serde_json::json!({
                "seq": 0,
                "source": "capture",
                "kind": "llm.request",
                "call_id": "c1",
                "payload": {
                    "model": "gpt-test",
                    "body": {"messages": [{"role": "user", "content": "hi"}]}
                }
            })
            .to_string(),
            serde_json::json!({
                "seq": 1,
                "source": "capture",
                "kind": "llm.response",
                "call_id": "c1",
                "payload": {
                    "body": {
                        "choices": [{"message": {"role": "assistant", "content": "hello"}}],
                        "usage": {"prompt_tokens": 3, "completion_tokens": 5}
                    },
                    "ttft_ms": 120
                }
            })
            .to_string(),
        ];
        let (_, turns) = analyze_session_tree(&records).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].turn.user_chars, 2);
        assert_eq!(turns[0].turn.assistant_chars, 5);
        assert_eq!(turns[0].turn.completion_tokens, Some(5));
        assert_eq!(turns[0].turn.ttft_ms, Some(120));
    }

    #[test]
    fn merges_same_call_and_groups_multiple_spawns() {
        let link_a =
            r#"{"subagent_type":"Explore","subagent_id":"a","subagent_trajectory":"agent-a.md"}"#;
        let link_b =
            r#"{"subagent_type":"Explore","subagent_id":"b","subagent_trajectory":"agent-b.md"}"#;
        let records = vec![
            r#"{"kind":"llm.request","content":"task","call_id":"call-main"}"#.into(),
            r#"{"kind":"llm.response.stream","content":"part1","call_id":"call-main"}"#.into(),
            format!(
                r#"{{"kind":"llm.spawn_link","call_id":"call-main","spawn_links":[{link_a}]}}"#
            ),
            r#"{"kind":"llm.response.stream","content":"part2","call_id":"call-main"}"#.into(),
            format!(
                r#"{{"kind":"llm.spawn_link","call_id":"call-main","spawn_links":[{link_b}]}}"#
            ),
            r#"{"kind":"llm.request","content":"next"}"#.into(),
        ];
        let (_, turns) = analyze_session_tree(&records).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].spawn_links.len(), 2);
        assert_eq!(turns[0].turn.assistant_chars, 10);
    }
}
