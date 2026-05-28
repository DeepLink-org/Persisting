//! Request body model rewrite for `forward` routes.

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use serde_json::Value;

/// Replace JSON `model` field before sending upstream.
pub fn rewrite_model_in_body(body: &Bytes, upstream_model: &str) -> Result<Bytes> {
    if body.is_empty() {
        return Ok(Bytes::new());
    }
    let mut v: Value = serde_json::from_slice(body).context("parse request JSON")?;
    let Some(obj) = v.as_object_mut() else {
        bail!("request body must be a JSON object to rewrite model");
    };
    obj.insert(
        "model".to_string(),
        Value::String(upstream_model.to_string()),
    );
    Ok(Bytes::from(
        serde_json::to_vec(&v).context("serialize request JSON")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn rewrite_model_in_request_body() {
        let body = Bytes::from_static(br#"{"model":"claude-3","messages":[]}"#);
        let out = rewrite_model_in_body(&body, "deepseek-chat").unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["model"], "deepseek-chat");
    }

    #[test]
    fn rewrite_model_empty_body_is_noop() {
        let out = rewrite_model_in_body(&Bytes::new(), "deepseek-chat").unwrap();
        assert!(out.is_empty());
    }
}
