//! Safe, bounded physical partitioning below one logical Gateway Dataset.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Timelike, Utc};

const MAX_TEMPLATE_BYTES: usize = 256;
const MAX_TEMPLATE_SEGMENTS: usize = 16;
const MAX_USER_SEGMENT_BYTES: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    User,
    Date,
    Hour,
}

/// A deliberately small partition-template language.
///
/// Templates are relative paths made from safe literal segments and the exact
/// placeholders `{user}`, `{date}`, and `{hour}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GatewaySplitTemplate {
    source: String,
    segments: Vec<Segment>,
}

impl GatewaySplitTemplate {
    pub(crate) fn parse(source: &str) -> Result<Self> {
        let source = source.trim();
        anyhow::ensure!(
            !source.is_empty(),
            "Gateway split template must not be empty"
        );
        anyhow::ensure!(
            source.len() <= MAX_TEMPLATE_BYTES,
            "Gateway split template exceeds {MAX_TEMPLATE_BYTES} bytes"
        );
        anyhow::ensure!(
            !source.starts_with('/') && !source.ends_with('/'),
            "Gateway split template must be relative and must not start or end with '/'"
        );
        anyhow::ensure!(
            !source.contains('\\'),
            "Gateway split template must use '/' separators"
        );

        let raw_segments = source.split('/').collect::<Vec<_>>();
        anyhow::ensure!(
            raw_segments.len() <= MAX_TEMPLATE_SEGMENTS,
            "Gateway split template exceeds {MAX_TEMPLATE_SEGMENTS} segments"
        );
        let mut segments = Vec::with_capacity(raw_segments.len());
        for raw in raw_segments {
            anyhow::ensure!(
                !raw.is_empty(),
                "Gateway split template contains an empty segment"
            );
            let segment = match raw {
                "{user}" => Segment::User,
                "{date}" => Segment::Date,
                "{hour}" => Segment::Hour,
                "." | ".." => {
                    anyhow::bail!("Gateway split template must not contain '.' or '..' segments")
                }
                literal => {
                    anyhow::ensure!(
                        literal
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)),
                        "Gateway split literal '{literal}' contains an unsupported character"
                    );
                    anyhow::ensure!(
                        !literal.contains('{') && !literal.contains('}'),
                        "unknown Gateway split placeholder '{literal}'"
                    );
                    Segment::Literal(literal.to_string())
                }
            };
            segments.push(segment);
        }
        Ok(Self {
            source: source.to_string(),
            segments,
        })
    }

    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    fn render(&self, user: Option<&str>, started_at: DateTime<Utc>) -> String {
        let user = safe_user_segment(user);
        self.segments
            .iter()
            .map(|segment| match segment {
                Segment::Literal(value) => value.clone(),
                Segment::User => user.clone(),
                Segment::Date => format!(
                    "{:04}-{:02}-{:02}",
                    started_at.year(),
                    started_at.month(),
                    started_at.day()
                ),
                Segment::Hour => format!("{:02}", started_at.hour()),
            })
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// Pins each logical run/session to the first partition selected for it.
/// Request and response records therefore cannot land in different hourly
/// prefixes, and a long-lived session remains one canonical event source.
#[derive(Debug)]
pub(crate) struct GatewayPartitionRouter {
    dataset_uri: String,
    split: Option<GatewaySplitTemplate>,
    routes: Mutex<HashMap<String, String>>,
}

impl GatewayPartitionRouter {
    pub(crate) fn new(
        dataset_uri: impl Into<String>,
        split: Option<GatewaySplitTemplate>,
    ) -> Result<Self> {
        let dataset_uri = dataset_uri.into();
        persisting_pchronicle::storage::DatasetLocation::parse(&dataset_uri)
            .context("validate Gateway Dataset URI")?;
        Ok(Self {
            dataset_uri: dataset_uri.trim_end_matches('/').to_string(),
            split,
            routes: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn dataset_uri(&self) -> &str {
        &self.dataset_uri
    }

    pub(crate) fn split_source(&self) -> Option<&str> {
        self.split.as_ref().map(GatewaySplitTemplate::source)
    }

    pub(crate) fn route(&self, route_key: &str, user: Option<&str>) -> String {
        let Some(split) = &self.split else {
            return self.dataset_uri.clone();
        };
        let mut routes = self.routes.lock().unwrap();
        routes
            .entry(route_key.to_string())
            .or_insert_with(|| {
                let prefix = split.render(user, Utc::now());
                format!("{}/{prefix}", self.dataset_uri)
            })
            .clone()
    }
}

fn safe_user_segment(user: Option<&str>) -> String {
    let Some(user) = user.map(str::trim).filter(|value| !value.is_empty()) else {
        return "_unknown".to_string();
    };
    let mut safe = String::with_capacity(user.len().min(MAX_USER_SEGMENT_BYTES));
    let mut changed = false;
    for byte in user.bytes() {
        if safe.len() >= MAX_USER_SEGMENT_BYTES {
            changed = true;
            break;
        }
        if byte.is_ascii_alphanumeric() || b"-_".contains(&byte) {
            safe.push(char::from(byte));
        } else {
            safe.push('_');
            changed = true;
        }
    }
    if safe.is_empty() || safe == "." || safe == ".." {
        changed = true;
        safe = "user".to_string();
    }
    if changed {
        let digest = blake3::hash(user.as_bytes()).to_hex();
        let keep = MAX_USER_SEGMENT_BYTES.saturating_sub(13).min(safe.len());
        safe.truncate(keep);
        safe.push('-');
        safe.push_str(&digest[..12]);
    }
    safe
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn template_renders_safe_relative_prefix() {
        let template = GatewaySplitTemplate::parse("archive/{user}/{date}/{hour}").unwrap();
        let at = Utc.with_ymd_and_hms(2026, 8, 25, 7, 30, 0).unwrap();
        assert_eq!(
            template.render(Some("alice"), at),
            "archive/alice/2026-08-25/07"
        );
    }

    #[test]
    fn template_rejects_escape_and_unknown_placeholders() {
        for invalid in ["/date", "date/", "date//hour", "../hour", "{month}"] {
            assert!(GatewaySplitTemplate::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn user_values_are_bounded_and_collision_resistant() {
        let first = safe_user_segment(Some("alice@example.com"));
        let second = safe_user_segment(Some("alice+example.com"));
        assert!(first.starts_with("alice_example_com-"));
        assert_ne!(first, second);
        assert_eq!(safe_user_segment(None), "_unknown");
    }

    #[test]
    fn router_pins_one_session_to_its_first_partition() {
        let router = GatewayPartitionRouter::new(
            "/tmp/captures",
            Some(GatewaySplitTemplate::parse("{user}/{date}/{hour}").unwrap()),
        )
        .unwrap();
        let first = router.route("agent|session", Some("alice"));
        let second = router.route("agent|session", Some("bob"));
        assert_eq!(first, second);
        assert!(first.contains("/alice/"));
    }
}
