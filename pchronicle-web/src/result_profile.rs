use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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
