use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnKind {
    Empty,
    Number,
    Boolean,
    Categorical,
    Text,
    DateTime,
    Object,
    Array,
    Identifier,
    Mixed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistogramBin {
    pub lower: f64,
    pub upper: f64,
    pub count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueCount {
    pub label: String,
    pub count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColumnProfile {
    pub name: String,
    pub kind: ColumnKind,
    pub row_count: usize,
    pub non_null_count: usize,
    pub missing_count: usize,
    pub unique_count: usize,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub mean: Option<f64>,
    pub histogram: Vec<HistogramBin>,
    pub top_values: Vec<ValueCount>,
    pub other_count: usize,
    pub type_counts: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RefinementIntent {
    pub source_revision_id: u64,
    pub column: String,
    pub label: String,
    pub predicate: RefinementPredicate,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RefinementPredicate {
    Equals {
        value: Value,
    },
    NumericRange {
        lower: f64,
        upper: f64,
        include_upper: bool,
    },
    Missing,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnalysisRefinement {
    Filter {
        intent: RefinementIntent,
    },
    FullProfile {
        source_revision_id: u64,
        column: String,
        column_kind: ColumnKind,
    },
}

const MAX_BINS: usize = 10;
const MAX_TOP_VALUES: usize = 10;

pub fn profile_rows(rows: &[Value]) -> Vec<ColumnProfile> {
    let mut columns = BTreeSet::new();
    for row in rows {
        if let Some(object) = row.as_object() {
            columns.extend(object.keys().cloned());
        }
    }

    columns
        .into_iter()
        .map(|name| profile_column(rows, name))
        .collect()
}

fn profile_column(rows: &[Value], name: String) -> ColumnProfile {
    let values = rows
        .iter()
        .filter_map(|row| row.as_object().and_then(|object| object.get(&name)))
        .filter(|value| !value.is_null())
        .collect::<Vec<_>>();
    let row_count = rows.len();
    let non_null_count = values.len();
    let missing_count = row_count - non_null_count;
    let value_counts = count_values(&values);
    let unique_count = value_counts.len();

    let kind = infer_kind(&name, &values, unique_count);
    let mut profile = ColumnProfile {
        name,
        kind: kind.clone(),
        row_count,
        non_null_count,
        missing_count,
        unique_count,
        min: None,
        max: None,
        mean: None,
        histogram: Vec::new(),
        top_values: Vec::new(),
        other_count: 0,
        type_counts: BTreeMap::new(),
    };

    match kind {
        ColumnKind::Number => add_numeric_summary(&mut profile, &values),
        ColumnKind::DateTime => add_datetime_summary(&mut profile, &values),
        ColumnKind::Text => add_text_summary(&mut profile, &values),
        ColumnKind::Categorical | ColumnKind::Boolean => add_top_values(&mut profile, value_counts),
        ColumnKind::Mixed => profile.type_counts = count_types(&values),
        ColumnKind::Empty | ColumnKind::Object | ColumnKind::Array | ColumnKind::Identifier => {}
    }

    profile
}

fn infer_kind(name: &str, values: &[&Value], unique_count: usize) -> ColumnKind {
    if values.is_empty() {
        return ColumnKind::Empty;
    }
    if is_identity_column(name) {
        return ColumnKind::Identifier;
    }
    if values.iter().all(|value| value.is_number()) {
        return ColumnKind::Number;
    }
    if values.iter().all(|value| value.is_boolean()) {
        return ColumnKind::Boolean;
    }
    if values.iter().all(|value| value.is_object()) {
        return ColumnKind::Object;
    }
    if values.iter().all(|value| value.is_array()) {
        return ColumnKind::Array;
    }
    if values.iter().all(|value| value.is_string()) {
        let strings = values
            .iter()
            .map(|value| value.as_str().expect("all values were checked as strings"));
        if strings
            .clone()
            .all(|value| OffsetDateTime::parse(value, &Rfc3339).is_ok())
        {
            return ColumnKind::DateTime;
        }
        let non_null_count = values.len();
        if unique_count <= 20 && (unique_count * 2 <= non_null_count || unique_count <= 2) {
            return ColumnKind::Categorical;
        }
        return ColumnKind::Text;
    }
    ColumnKind::Mixed
}

fn is_identity_column(name: &str) -> bool {
    matches!(
        name,
        "_file_"
            | "dataset"
            | "id"
            | "uuid"
            | "run_id"
            | "agent_id"
            | "session_id"
            | "root_session_id"
            | "turn_id"
    )
}

fn count_values(values: &[&Value]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for value in values {
        *counts.entry(canonical_label(value)).or_default() += 1;
    }
    counts
}

fn canonical_label(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        _ => serde_json::to_string(value).expect("serde_json values always serialize"),
    }
}

fn count_types(values: &[&Value]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for value in values {
        let label = match value {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };
        *counts.entry(label.to_string()).or_default() += 1;
    }
    counts
}

fn add_numeric_summary(profile: &mut ColumnProfile, values: &[&Value]) {
    let Some(numbers) = values
        .iter()
        .map(|value| value.as_f64().filter(|number| number.is_finite()))
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    add_distribution_summary(profile, &numbers);
}

fn add_datetime_summary(profile: &mut ColumnProfile, values: &[&Value]) {
    let Some(timestamps) = values
        .iter()
        .map(|value| {
            OffsetDateTime::parse(
                value
                    .as_str()
                    .expect("datetime inference verified string values"),
                &Rfc3339,
            )
            .ok()
            .map(|value| value.unix_timestamp_nanos() as f64 / 1_000_000_000.0)
        })
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    add_distribution_summary(profile, &timestamps);
}

fn add_text_summary(profile: &mut ColumnProfile, values: &[&Value]) {
    let lengths = values
        .iter()
        .map(|value| {
            value
                .as_str()
                .expect("text inference verified string values")
                .chars()
                .count() as f64
        })
        .collect::<Vec<_>>();
    add_distribution_summary(profile, &lengths);
}

fn add_distribution_summary(profile: &mut ColumnProfile, values: &[f64]) {
    let Some(min) = values.iter().copied().reduce(f64::min) else {
        return;
    };
    let max = values.iter().copied().reduce(f64::max).expect("min exists");
    profile.min = Some(min);
    profile.max = Some(max);
    profile.mean = Some(values.iter().sum::<f64>() / values.len() as f64);
    profile.histogram = equal_width_histogram(values, min, max);
}

fn equal_width_histogram(values: &[f64], min: f64, max: f64) -> Vec<HistogramBin> {
    if values.is_empty() {
        return Vec::new();
    }
    if min == max {
        return vec![HistogramBin {
            lower: min,
            upper: max,
            count: values.len(),
        }];
    }

    let bin_count = values.len().min(MAX_BINS);
    let width = (max - min) / bin_count as f64;
    let mut bins = (0..bin_count)
        .map(|index| HistogramBin {
            lower: min + width * index as f64,
            upper: if index + 1 == bin_count {
                max
            } else {
                min + width * (index + 1) as f64
            },
            count: 0,
        })
        .collect::<Vec<_>>();

    for value in values {
        let index = (((value - min) / width).floor() as usize).min(bin_count - 1);
        bins[index].count += 1;
    }
    bins
}

fn add_top_values(profile: &mut ColumnProfile, counts: BTreeMap<String, usize>) {
    let mut values = counts.into_iter().collect::<Vec<_>>();
    values.sort_by(|(left_label, left_count), (right_label, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_label.cmp(right_label))
    });
    profile.other_count = values
        .iter()
        .skip(MAX_TOP_VALUES)
        .map(|(_, count)| *count)
        .sum();
    profile.top_values = values
        .into_iter()
        .take(MAX_TOP_VALUES)
        .map(|(label, count)| ValueCount { label, count })
        .collect();
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{profile_rows, ColumnKind, ColumnProfile};

    fn profile<'a>(profiles: &'a [ColumnProfile], name: &str) -> &'a ColumnProfile {
        profiles
            .iter()
            .find(|profile| profile.name == name)
            .unwrap()
    }

    #[test]
    fn profiles_numeric_categorical_text_and_missing_values() {
        let rows = vec![
            json!({"latency_ms": 10, "status": "ok", "message": "short"}),
            json!({"latency_ms": 20, "status": "failed", "message": "a longer message"}),
            json!({"latency_ms": null, "status": "ok", "message": "free text three"}),
        ];
        let profiles = profile_rows(&rows);
        assert_eq!(profile(&profiles, "latency_ms").kind, ColumnKind::Number);
        assert_eq!(profile(&profiles, "latency_ms").missing_count, 1);
        assert_eq!(profile(&profiles, "status").kind, ColumnKind::Categorical);
        assert_eq!(profile(&profiles, "status").top_values[0].label, "ok");
        assert_eq!(profile(&profiles, "message").kind, ColumnKind::Text);
    }

    #[test]
    fn numeric_histogram_handles_single_value_without_fake_range() {
        let profiles = profile_rows(&[json!({"value": 7}), json!({"value": 7})]);
        let bins = &profile(&profiles, "value").histogram;
        assert_eq!(bins.len(), 1);
        assert_eq!((bins[0].lower, bins[0].upper, bins[0].count), (7.0, 7.0, 2));
    }

    #[test]
    fn top_values_use_label_as_stable_tie_breaker() {
        let rows = vec![json!({"kind":"b"}), json!({"kind":"a"})];
        let profiles = profile_rows(&rows);
        let values = &profile(&profiles, "kind").top_values;
        assert_eq!(
            values.iter().map(|v| v.label.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn profiles_only_strict_rfc3339_strings_as_datetimes() {
        let profiles = profile_rows(&[
            json!({"occurred_at": "2026-08-22T01:02:03Z"}),
            json!({"occurred_at": "2026-08-23T02:03:04+08:00"}),
            json!({"locale_date": "08/22/2026"}),
            json!({"locale_date": "08/23/2026"}),
            json!({"locale_date": "08/24/2026"}),
        ]);

        let datetime = profile(&profiles, "occurred_at");
        assert_eq!(datetime.kind, ColumnKind::DateTime);
        assert_eq!(
            datetime
                .histogram
                .iter()
                .map(|bin| bin.count)
                .sum::<usize>(),
            2
        );
        assert_eq!(profile(&profiles, "locale_date").kind, ColumnKind::Text);
    }

    #[test]
    fn known_identity_names_override_string_cardinality_inference() {
        let profiles = profile_rows(&[
            json!({"run_id": "run-a", "status": "ok"}),
            json!({"run_id": "run-b", "status": "ok"}),
        ]);

        assert_eq!(profile(&profiles, "run_id").kind, ColumnKind::Identifier);
        assert_eq!(profile(&profiles, "status").kind, ColumnKind::Categorical);
    }

    #[test]
    fn profiles_uniform_boolean_object_and_array_values() {
        let profiles = profile_rows(&[
            json!({"enabled": true, "metadata": {"a": 1}, "tags": ["a"]}),
            json!({"enabled": false, "metadata": {"b": 2}, "tags": ["b", "c"]}),
        ]);

        assert_eq!(profile(&profiles, "enabled").kind, ColumnKind::Boolean);
        assert_eq!(profile(&profiles, "metadata").kind, ColumnKind::Object);
        assert_eq!(profile(&profiles, "tags").kind, ColumnKind::Array);
    }

    #[test]
    fn mixed_scalars_only_report_type_counts() {
        let profiles = profile_rows(&[
            json!({"value": 1}),
            json!({"value": "one"}),
            json!({"value": true}),
            json!({"value": null}),
        ]);
        let mixed = profile(&profiles, "value");

        assert_eq!(mixed.kind, ColumnKind::Mixed);
        assert_eq!(mixed.missing_count, 1);
        assert_eq!(mixed.type_counts.get("number"), Some(&1));
        assert_eq!(mixed.type_counts.get("string"), Some(&1));
        assert_eq!(mixed.type_counts.get("boolean"), Some(&1));
        assert!(mixed.histogram.is_empty());
        assert!(mixed.top_values.is_empty());
    }

    #[test]
    fn empty_and_all_null_columns_are_empty_with_separate_missing_counts() {
        assert!(profile_rows(&[]).is_empty());

        let profiles = profile_rows(&[json!({"only_null": null}), json!({})]);
        let empty = profile(&profiles, "only_null");
        assert_eq!(empty.kind, ColumnKind::Empty);
        assert_eq!(empty.row_count, 2);
        assert_eq!(empty.non_null_count, 0);
        assert_eq!(empty.missing_count, 2);
        assert_eq!(empty.unique_count, 0);
    }

    #[test]
    fn numeric_ranges_cover_negative_values_without_losing_the_upper_bound() {
        let profiles = profile_rows(&[
            json!({"delta": -10}),
            json!({"delta": -5}),
            json!({"delta": 0}),
        ]);
        let numeric = profile(&profiles, "delta");

        assert_eq!(
            (numeric.min, numeric.max, numeric.mean),
            (Some(-10.0), Some(0.0), Some(-5.0))
        );
        assert!(numeric.histogram.len() <= 10);
        assert_eq!(numeric.histogram.first().unwrap().lower, -10.0);
        assert_eq!(numeric.histogram.last().unwrap().upper, 0.0);
        assert_eq!(
            numeric.histogram.iter().map(|bin| bin.count).sum::<usize>(),
            3
        );
    }

    #[test]
    fn text_profiles_bin_character_lengths() {
        let profiles = profile_rows(&[
            json!({"message": "a"}),
            json!({"message": "abc"}),
            json!({"message": "abcde"}),
        ]);
        let text = profile(&profiles, "message");

        assert_eq!(text.kind, ColumnKind::Text);
        assert_eq!(
            (text.min, text.max, text.mean),
            (Some(1.0), Some(5.0), Some(3.0))
        );
        assert!(text.histogram.len() <= 10);
        assert_eq!(text.histogram.iter().map(|bin| bin.count).sum::<usize>(), 3);
    }

    #[test]
    fn categorical_top_values_are_limited_to_ten_and_track_other_values() {
        let rows = (0..11)
            .flat_map(|index| {
                std::iter::repeat(json!({"kind": format!("kind-{index:02}")})).take(2)
            })
            .collect::<Vec<_>>();
        let profiles = profile_rows(&rows);
        let categorical = profile(&profiles, "kind");

        assert_eq!(categorical.kind, ColumnKind::Categorical);
        assert_eq!(categorical.top_values.len(), 10);
        assert_eq!(categorical.top_values[0].label, "kind-00");
        assert_eq!(categorical.other_count, 2);
    }

    #[test]
    fn serde_json_rejects_non_finite_numbers_before_profiling() {
        assert!(serde_json::Number::from_f64(f64::NAN).is_none());
        assert!(serde_json::Number::from_f64(f64::INFINITY).is_none());
        assert!(serde_json::Number::from_f64(f64::NEG_INFINITY).is_none());
    }

    #[test]
    fn refinement_data_serializes_without_embedded_sql() {
        let refinement = super::AnalysisRefinement::Filter {
            intent: super::RefinementIntent {
                source_revision_id: 42,
                column: "latency_ms".into(),
                label: "10 through 20".into(),
                predicate: super::RefinementPredicate::NumericRange {
                    lower: 10.0,
                    upper: 20.0,
                    include_upper: false,
                },
            },
        };

        assert_eq!(
            serde_json::to_value(refinement).unwrap(),
            json!({
                "kind": "filter",
                "intent": {
                    "source_revision_id": 42,
                    "column": "latency_ms",
                    "label": "10 through 20",
                    "predicate": {
                        "kind": "numeric_range",
                        "lower": 10.0,
                        "upper": 20.0,
                        "include_upper": false,
                    },
                },
            })
        );
    }
}
