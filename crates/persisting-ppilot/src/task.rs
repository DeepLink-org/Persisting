//! Task expression / result wire format (one JSON object per line).
//!
//! **Primitive:** [`TaskExpr`] (plan item) · [`TaskResult`] (terminal outcome).
//! Product contract: `plan()` yield shape `{id, …fields}` ↔ `execute(item)`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// One unit of work in an execution plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskExpr {
    pub id: String,
    #[serde(default = "default_op")]
    pub op: String,
    #[serde(default)]
    pub args: HashMap<String, Value>,
    #[serde(default)]
    pub meta: HashMap<String, Value>,
}

fn default_op() -> String {
    "execute".into()
}

impl TaskExpr {
    pub fn from_value(mut v: Value) -> anyhow::Result<Self> {
        let obj = v
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("task must be a JSON object"))?;

        let id = obj
            .remove("id")
            .or_else(|| obj.remove("task_id"))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "task must define a stable non-empty 'id' (or 'task_id'); \
                     random ids are not generated because they break deduplication and --resume"
                )
            })
            .and_then(|x| match x {
                Value::String(s) if !s.trim().is_empty() => Ok(s),
                Value::Number(n) => Ok(n.to_string()),
                Value::String(_) => Err(anyhow::anyhow!("task id must not be empty")),
                _ => Err(anyhow::anyhow!("task id must be a string or number")),
            })?;

        let op = obj
            .remove("op")
            .or_else(|| obj.remove("type"))
            .and_then(|x| x.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "execute".into());

        let meta = match obj.remove("meta") {
            Some(Value::Object(m)) => m.into_iter().collect(),
            Some(_) => {
                return Err(anyhow::anyhow!("task.meta must be an object"));
            }
            None => HashMap::new(),
        };

        let args = match obj.remove("args") {
            Some(Value::Object(m)) => m.into_iter().collect(),
            Some(_) => {
                return Err(anyhow::anyhow!("task.args must be an object"));
            }
            None => {
                // Flat payload: remaining fields become args.
                std::mem::take(obj).into_iter().collect()
            }
        };

        Ok(Self { id, op, args, meta })
    }

    pub fn to_ndjson(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_id: String,
    /// Stable pVisor Run identity generated for this task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Concrete pVisor attempt that produced this result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    /// Fencing token held by the pPilot owner when the attempt was submitted.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub lease_epoch: u64,
    pub ok: bool,
    /// pPilot cancellation (not an execute failure).
    #[serde(default)]
    pub cancelled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// Numeric measurements returned by `execute()` as `{"metrics": {...}}`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metrics: HashMap<String, f64>,
    /// References to large outputs returned by `execute()` as `{"artifacts": {...}}`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub artifacts: HashMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traceback: Option<String>,
    /// Stable terminal failure category for filtering and retry policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<ErrorKind>,
    /// Whether retrying the task in a later job may be useful.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<f64>,
    /// How many pPilot infra retries occurred before this terminal result (0 = first try).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub infra_retries: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Execute,
    Infra,
    Cancelled,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

impl TaskResult {
    pub fn success(
        task_id: impl Into<String>,
        value: Value,
        worker: &str,
        started_at: f64,
    ) -> Self {
        let (metrics, artifacts) = result_metadata(&value);
        Self {
            task_id: task_id.into(),
            run_id: None,
            attempt_id: None,
            lease_epoch: 0,
            ok: true,
            cancelled: false,
            value: Some(value),
            metrics,
            artifacts,
            error: None,
            traceback: None,
            error_kind: None,
            retryable: false,
            worker: Some(worker.to_string()),
            started_at: Some(started_at),
            finished_at: Some(now_secs()),
            infra_retries: 0,
        }
    }

    pub fn failure(
        task_id: impl Into<String>,
        error: impl Into<String>,
        traceback: Option<String>,
        worker: &str,
        started_at: f64,
    ) -> Self {
        Self::failure_with_kind(
            task_id,
            error,
            traceback,
            worker,
            started_at,
            ErrorKind::Execute,
            false,
        )
    }

    pub fn failure_with_kind(
        task_id: impl Into<String>,
        error: impl Into<String>,
        traceback: Option<String>,
        worker: &str,
        started_at: f64,
        error_kind: ErrorKind,
        retryable: bool,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            run_id: None,
            attempt_id: None,
            lease_epoch: 0,
            ok: false,
            cancelled: false,
            value: None,
            metrics: HashMap::new(),
            artifacts: HashMap::new(),
            error: Some(error.into()),
            traceback,
            error_kind: Some(error_kind),
            retryable,
            worker: Some(worker.to_string()),
            started_at: Some(started_at),
            finished_at: Some(now_secs()),
            infra_retries: 0,
        }
    }

    pub fn cancelled(task_id: impl Into<String>) -> Self {
        let started = now_secs();
        Self {
            task_id: task_id.into(),
            run_id: None,
            attempt_id: None,
            lease_epoch: 0,
            ok: false,
            cancelled: true,
            value: None,
            metrics: HashMap::new(),
            artifacts: HashMap::new(),
            error: Some("cancelled".into()),
            traceback: None,
            error_kind: Some(ErrorKind::Cancelled),
            retryable: false,
            worker: None,
            started_at: Some(started),
            finished_at: Some(started),
            infra_retries: 0,
        }
    }

    pub fn to_ndjson(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

fn result_metadata(value: &Value) -> (HashMap<String, f64>, HashMap<String, Value>) {
    let Some(obj) = value.as_object() else {
        return (HashMap::new(), HashMap::new());
    };
    let metrics = obj
        .get("metrics")
        .and_then(Value::as_object)
        .map(|items| {
            items
                .iter()
                .filter_map(|(name, value)| value.as_f64().map(|number| (name.clone(), number)))
                .collect()
        })
        .unwrap_or_default();
    let artifacts = obj
        .get("artifacts")
        .and_then(Value::as_object)
        .map(|items| {
            items
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();
    (metrics, artifacts)
}

pub fn unix_now() -> f64 {
    now_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::{Map, Value, json};

    fn scalar_value() -> impl Strategy<Value = Value> {
        prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(|value| json!(value)),
            proptest::string::string_regex("[a-zA-Z0-9 _-]{0,12}")
                .unwrap()
                .prop_map(Value::String),
        ]
    }

    fn non_null_scalar_value() -> impl Strategy<Value = Value> {
        scalar_value().prop_filter("value must not be JSON null", |value| !value.is_null())
    }

    fn indexed_object(prefix: &'static str, values: Vec<Value>) -> Map<String, Value> {
        values
            .into_iter()
            .enumerate()
            .map(|(index, value)| (format!("{prefix}_{index}"), value))
            .collect()
    }

    fn without_timestamps(mut value: Value) -> Value {
        if let Some(object) = value.as_object_mut() {
            object.remove("started_at");
            object.remove("finished_at");
        }
        value
    }

    fn assert_timestamp_roundtrip(actual: Option<f64>, expected: Option<f64>) {
        match (actual, expected) {
            (Some(actual), Some(expected)) => {
                assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
            }
            (None, None) => {}
            (actual, expected) => panic!("timestamp presence changed: {actual:?} != {expected:?}"),
        }
    }

    proptest! {
        #[test]
        fn flat_payload_becomes_args(
            fields in prop::collection::vec(scalar_value(), 0..8),
        ) {
            let mut input = Map::new();
            input.insert("id".into(), json!("t-0"));
            let expected = indexed_object("field", fields);
            input.extend(expected.clone());

            let task = TaskExpr::from_value(Value::Object(input)).unwrap();
            prop_assert_eq!(task.id, "t-0");
            prop_assert_eq!(task.op, "execute");
            prop_assert_eq!(task.args, expected.into_iter().collect());
            prop_assert!(task.meta.is_empty());
        }

        #[test]
        fn nested_args_and_task_id_alias(
            task_id in prop_oneof![
                proptest::string::string_regex("[a-z][a-z0-9-]{0,8}")
                    .unwrap()
                    .prop_map(Value::String),
                any::<u64>().prop_map(|value| json!(value)),
            ],
            args in prop::collection::vec(scalar_value(), 0..6),
            meta in prop::collection::vec(scalar_value(), 0..6),
        ) {
            let expected_id = match &task_id {
                Value::String(value) => value.clone(),
                Value::Number(value) => value.to_string(),
                _ => unreachable!(),
            };
            let expected_args = indexed_object("arg", args);
            let expected_meta = indexed_object("meta", meta);
            let input = json!({
                "task_id": task_id,
                "type": "execute",
                "args": expected_args,
                "meta": expected_meta,
            });

            let task = TaskExpr::from_value(input).unwrap();
            prop_assert_eq!(task.id, expected_id);
            prop_assert_eq!(task.op, "execute");
            prop_assert_eq!(task.args, expected_args.into_iter().collect());
            prop_assert_eq!(task.meta, expected_meta.into_iter().collect());
        }

        #[test]
        fn rejects_non_object_meta(meta in scalar_value()) {
            let input = json!({"id": "t", "meta": meta});
            prop_assert!(TaskExpr::from_value(input).is_err());
        }

        #[test]
        fn result_roundtrip_preserves_terminal_flags(
            task_id in proptest::string::string_regex("[a-z][a-z0-9-]{0,8}").unwrap(),
            value in non_null_scalar_value(),
            worker in proptest::string::string_regex("w[0-9]{1,2}").unwrap(),
            started_at in 0u64..1_000_000u64,
        ) {
            let started_at = started_at as f64;
            let ok = TaskResult::success(&task_id, value, &worker, started_at);
            let back: TaskResult = serde_json::from_str(&ok.to_ndjson().unwrap()).unwrap();
            let back_wire = serde_json::to_value(&back).unwrap();
            let ok_wire = serde_json::to_value(&ok).unwrap();
            prop_assert_eq!(without_timestamps(back_wire), without_timestamps(ok_wire));
            assert_timestamp_roundtrip(back.started_at, ok.started_at);
            assert_timestamp_roundtrip(back.finished_at, ok.finished_at);
            prop_assert!(back.ok);
            prop_assert!(!back.cancelled);

            let cancelled = TaskResult::cancelled(&task_id);
            let cancelled_back: TaskResult =
                serde_json::from_str(&cancelled.to_ndjson().unwrap()).unwrap();
            let cancelled_back_wire = serde_json::to_value(&cancelled_back).unwrap();
            let cancelled_wire = serde_json::to_value(&cancelled).unwrap();
            prop_assert_eq!(cancelled_back_wire, cancelled_wire);
            prop_assert!(cancelled_back.cancelled);
            prop_assert!(!cancelled_back.ok);

            let failed = TaskResult::failure(
                &task_id,
                "boom",
                Some("traceback".into()),
                &worker,
                started_at,
            );
            let failed_back: TaskResult =
                serde_json::from_str(&failed.to_ndjson().unwrap()).unwrap();
            let failed_back_wire = serde_json::to_value(&failed_back).unwrap();
            let failed_wire = serde_json::to_value(&failed).unwrap();
            prop_assert_eq!(
                without_timestamps(failed_back_wire),
                without_timestamps(failed_wire)
            );
            assert_timestamp_roundtrip(failed_back.started_at, failed.started_at);
            assert_timestamp_roundtrip(failed_back.finished_at, failed.finished_at);
            prop_assert!(!failed_back.ok);
            prop_assert_eq!(failed_back.traceback.as_deref(), Some("traceback"));
        }

        #[test]
        fn success_extracts_metadata_without_changing_value(
            metrics in prop::collection::vec(-1_000.0f64..1_000.0, 0..6),
            artifacts in prop::collection::vec(scalar_value(), 0..6),
            payload in scalar_value(),
        ) {
            let metric_values = indexed_object(
                "metric",
                metrics.into_iter().map(|value| json!(value)).collect(),
            );
            let artifact_values = indexed_object("artifact", artifacts);
            let expected_artifacts: HashMap<_, _> = artifact_values.clone().into_iter().collect();
            let value = json!({
                "metrics": metric_values,
                "artifacts": artifact_values,
                "payload": payload,
            });
            let result = TaskResult::success("t", value.clone(), "w0", 0.0);

            prop_assert_eq!(result.value, Some(value));
            prop_assert_eq!(result.metrics.len(), metric_values.len());
            for (name, metric) in metric_values {
                let expected_metric = metric.as_f64();
                prop_assert_eq!(result.metrics.get(&name), expected_metric.as_ref());
            }
            prop_assert_eq!(result.artifacts, expected_artifacts);
        }
    }

    #[test]
    fn missing_id_is_rejected_for_stable_resume_identity() {
        let err = TaskExpr::from_value(json!({"x": 1})).unwrap_err();
        assert!(err.to_string().contains("stable non-empty 'id'"));
    }

    #[test]
    fn invalid_ids_are_rejected() {
        assert!(TaskExpr::from_value(json!({"id": "", "x": 1})).is_err());
        assert!(TaskExpr::from_value(json!({"id": null, "x": 1})).is_err());
        assert!(TaskExpr::from_value(json!({"id": {"nested": 1}, "x": 1})).is_err());
    }

    #[test]
    fn flat_yield_shape_roundtrips_to_result_ndjson() {
        // plan() yield {"id", ...fields} → TaskExpr → TaskResult ndjson for sink.
        let wire = TaskExpr::from_value(json!({"id": "t-0", "x": 2, "tag": "a"})).unwrap();
        assert_eq!(wire.op, "execute");
        assert_eq!(wire.args.get("x"), Some(&json!(2)));
        let ok = TaskResult::success(&wire.id, json!({"x2": 4}), "w0", 0.0);
        let line = ok.to_ndjson().unwrap();
        assert!(line.contains("\"task_id\":\"t-0\"") && line.contains("\"ok\":true"));
    }
}
