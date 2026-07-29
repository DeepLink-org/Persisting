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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_id: String,
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
    use serde_json::json;

    #[test]
    fn flat_payload_becomes_args() {
        let t = TaskExpr::from_value(json!({"id": "t-0", "x": 1, "y": "a"})).unwrap();
        assert_eq!(t.id, "t-0");
        assert_eq!(t.op, "execute");
        assert_eq!(t.args.get("x"), Some(&json!(1)));
        assert_eq!(t.args.get("y"), Some(&json!("a")));
    }

    #[test]
    fn nested_args_and_task_id_alias() {
        let t = TaskExpr::from_value(json!({
            "task_id": 7,
            "type": "execute",
            "args": {"n": 3},
            "meta": {"prio": 1}
        }))
        .unwrap();
        assert_eq!(t.id, "7");
        assert_eq!(t.op, "execute");
        assert_eq!(t.args.get("n"), Some(&json!(3)));
        assert_eq!(t.meta.get("prio"), Some(&json!(1)));
    }

    #[test]
    fn rejects_non_object_meta() {
        assert!(TaskExpr::from_value(json!({"id": "t", "meta": 1})).is_err());
    }

    #[test]
    fn result_roundtrip_flags() {
        let ok = TaskResult::success("t", json!({"v": 1}), "w0", 1.0);
        let line = ok.to_ndjson().unwrap();
        let back: TaskResult = serde_json::from_str(&line).unwrap();
        assert!(back.ok && !back.cancelled);

        let c = TaskResult::cancelled("t");
        assert!(c.cancelled && !c.ok);
        let f = TaskResult::failure("t", "boom", Some("tb".into()), "w0", 0.0);
        assert!(!f.ok && f.traceback.as_deref() == Some("tb"));
    }

    #[test]
    fn success_extracts_metrics_and_artifacts_without_changing_value() {
        let value = json!({
            "metrics": {"reward": 0.75, "label": "ignored"},
            "artifacts": {"trajectory": "lance://runs/t-0"},
            "payload": {"x": 1}
        });
        let result = TaskResult::success("t", value.clone(), "w0", 0.0);
        assert_eq!(result.value, Some(value));
        assert_eq!(result.metrics.get("reward"), Some(&0.75));
        assert_eq!(result.artifacts["trajectory"], json!("lance://runs/t-0"));
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
