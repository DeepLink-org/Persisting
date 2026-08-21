use std::path::Path;

use serde_json::{json, Value};

use crate::error::ReplayError;
use crate::io::atomic_write_json;

pub fn write_next_action(
    path: &Path,
    original: &Value,
    replayed: &Value,
) -> Result<(), ReplayError> {
    atomic_write_json(
        path,
        &json!({
            "schema_version": "sandbox-playback.next-action-comparison/v2",
            "original": original,
            "replayed": replayed,
            "metrics": metrics(original, replayed),
            "gating": false,
        }),
    )
}

fn metrics(original: &Value, replayed: &Value) -> Value {
    let original_text = text(original);
    let replayed_text = text(replayed);
    let original_reasoning = reasoning(original);
    let replayed_reasoning = reasoning(replayed);
    let original_tools = tools(original);
    let replayed_tools = tools(replayed);
    let text_metrics = content_metrics(original_text, replayed_text);
    let reasoning_metrics = content_metrics(original_reasoning, replayed_reasoning);
    json!({
        "original_text_present": text_metrics.original_present,
        "replayed_text_present": text_metrics.replayed_present,
        "text_comparison_status": text_metrics.status,
        "text_exact": text_metrics.exact,
        "text_similarity": text_metrics.similarity,
        "original_reasoning_present": reasoning_metrics.original_present,
        "replayed_reasoning_present": reasoning_metrics.replayed_present,
        "reasoning_comparison_status": reasoning_metrics.status,
        "reasoning_exact": reasoning_metrics.exact,
        "reasoning_similarity": reasoning_metrics.similarity,
        "tool_count_equal": original_tools.len() == replayed_tools.len(),
        "ordered_tool_names_equal": original_tools.iter().map(|tool| tool.get("name"))
            .eq(replayed_tools.iter().map(|tool| tool.get("name"))),
        "tool_arguments_equal": original_tools.iter().map(|tool| tool.get("arguments"))
            .eq(replayed_tools.iter().map(|tool| tool.get("arguments"))),
    })
}

#[derive(Debug)]
struct ContentMetrics {
    original_present: bool,
    replayed_present: bool,
    status: &'static str,
    exact: Option<bool>,
    similarity: Option<f64>,
}

fn content_metrics(original: &str, replayed: &str) -> ContentMetrics {
    let normalized_original = normalize(original);
    let normalized_replayed = normalize(replayed);
    let original_present = !normalized_original.is_empty();
    let replayed_present = !normalized_replayed.is_empty();
    match (original_present, replayed_present) {
        (false, false) => ContentMetrics {
            original_present,
            replayed_present,
            status: "not_applicable_both_empty",
            exact: None,
            similarity: None,
        },
        (false, true) => ContentMetrics {
            original_present,
            replayed_present,
            status: "original_empty",
            exact: Some(false),
            similarity: Some(0.0),
        },
        (true, false) => ContentMetrics {
            original_present,
            replayed_present,
            status: "replayed_empty",
            exact: Some(false),
            similarity: Some(0.0),
        },
        (true, true) => ContentMetrics {
            original_present,
            replayed_present,
            status: "comparable",
            exact: Some(original == replayed),
            similarity: Some(sequence_matcher_ratio(
                &normalized_original,
                &normalized_replayed,
            )),
        },
    }
}

fn text(action: &Value) -> &str {
    action
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn reasoning(action: &Value) -> &str {
    action
        .get("reasoning")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn tools(action: &Value) -> &[Value] {
    action
        .get("tools")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn normalize(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

// Port of difflib.SequenceMatcher's ratio for strings with isjunk=None. Keeping
// this local avoids a runtime Python dependency while preserving the metric
// emitted by the original SandboxReplay implementation.
fn sequence_matcher_ratio(left: &str, right: &str) -> f64 {
    let a: Vec<char> = left.chars().collect();
    let b: Vec<char> = right.chars().collect();
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    let matches: usize = matching_blocks(&a, &b).iter().map(|block| block.2).sum();
    2.0 * matches as f64 / total as f64
}

fn matching_blocks(a: &[char], b: &[char]) -> Vec<(usize, usize, usize)> {
    let mut queue = vec![(0, a.len(), 0, b.len())];
    let mut matches = Vec::new();
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, size) = longest_match(a, b, alo, ahi, blo, bhi);
        if size == 0 {
            continue;
        }
        matches.push((i, j, size));
        if alo < i && blo < j {
            queue.push((alo, i, blo, j));
        }
        if i + size < ahi && j + size < bhi {
            queue.push((i + size, ahi, j + size, bhi));
        }
    }
    matches.sort_unstable();
    let mut collapsed: Vec<(usize, usize, usize)> = Vec::new();
    for (i, j, size) in matches {
        if let Some(last) = collapsed.last_mut() {
            if last.0 + last.2 == i && last.1 + last.2 == j {
                last.2 += size;
                continue;
            }
        }
        collapsed.push((i, j, size));
    }
    collapsed
}

fn longest_match(
    a: &[char],
    b: &[char],
    alo: usize,
    ahi: usize,
    blo: usize,
    bhi: usize,
) -> (usize, usize, usize) {
    use std::collections::{HashMap, HashSet};

    let mut positions: HashMap<char, Vec<usize>> = HashMap::new();
    for (index, character) in b.iter().copied().enumerate() {
        positions.entry(character).or_default().push(index);
    }
    let popular: HashSet<char> = if b.len() >= 200 {
        let threshold = b.len() / 100 + 1;
        positions
            .iter()
            .filter_map(|(character, indexes)| (indexes.len() > threshold).then_some(*character))
            .collect()
    } else {
        HashSet::new()
    };

    let (mut best_i, mut best_j, mut best_size) = (alo, blo, 0);
    let mut previous: HashMap<usize, usize> = HashMap::new();
    for (i, character) in a.iter().copied().enumerate().take(ahi).skip(alo) {
        let mut current = HashMap::new();
        if !popular.contains(&character) {
            if let Some(indexes) = positions.get(&character) {
                for &j in indexes {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let size = previous
                        .get(&j.checked_sub(1).unwrap_or(usize::MAX))
                        .copied()
                        .unwrap_or(0)
                        + 1;
                    current.insert(j, size);
                    if size > best_size {
                        (best_i, best_j, best_size) = (i + 1 - size, j + 1 - size, size);
                    }
                }
            }
        }
        previous = current;
    }
    while best_i > alo && best_j > blo && a[best_i - 1] == b[best_j - 1] {
        best_i -= 1;
        best_j -= 1;
        best_size += 1;
    }
    while best_i + best_size < ahi
        && best_j + best_size < bhi
        && a[best_i + best_size] == b[best_j + best_size]
    {
        best_size += 1;
    }
    (best_i, best_j, best_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_matcher_matches_difflib_examples() {
        assert_eq!(sequence_matcher_ratio("", ""), 1.0);
        assert_eq!(sequence_matcher_ratio("abcd", "abxd"), 0.75);
        assert_eq!(sequence_matcher_ratio("abc", "abc"), 1.0);
    }

    #[test]
    fn next_action_metrics_preserve_nonempty_text_contract() {
        let original = json!({
            "text": "inspect\r\n file",
            "tools": [{"name": "read", "arguments": {"path": "a"}}],
        });
        let replayed = json!({
            "text": "inspect file",
            "tools": [{"name": "read", "arguments": {"path": "a"}}],
        });
        let value = metrics(&original, &replayed);
        assert_eq!(value["text_exact"], false);
        assert_eq!(value["text_similarity"], 1.0);
        assert_eq!(value["text_comparison_status"], "comparable");
        assert_eq!(value["original_text_present"], true);
        assert_eq!(value["replayed_text_present"], true);
        assert_eq!(value["reasoning_exact"], Value::Null);
        assert_eq!(value["reasoning_similarity"], Value::Null);
        assert_eq!(
            value["reasoning_comparison_status"],
            "not_applicable_both_empty"
        );
        assert_eq!(value["tool_count_equal"], true);
        assert_eq!(value["ordered_tool_names_equal"], true);
        assert_eq!(value["tool_arguments_equal"], true);
    }

    #[test]
    fn empty_text_is_not_reported_as_a_perfect_match() {
        let original = json!({"text": "\n\n", "tools": []});
        let replayed = json!({"text": " \t", "tools": []});
        let value = metrics(&original, &replayed);
        assert_eq!(value["original_text_present"], false);
        assert_eq!(value["replayed_text_present"], false);
        assert_eq!(value["text_comparison_status"], "not_applicable_both_empty");
        assert_eq!(value["text_exact"], Value::Null);
        assert_eq!(value["text_similarity"], Value::Null);
    }

    #[test]
    fn reasoning_is_compared_separately_from_visible_text() {
        let original = json!({"text": "", "reasoning": "inspect files", "tools": []});
        let replayed = json!({"text": "", "reasoning": "inspect code", "tools": []});
        let value = metrics(&original, &replayed);
        assert_eq!(value["text_similarity"], Value::Null);
        assert_eq!(value["reasoning_comparison_status"], "comparable");
        assert_eq!(value["reasoning_exact"], false);
        assert!(value["reasoning_similarity"].as_f64().unwrap() > 0.0);
    }
}
