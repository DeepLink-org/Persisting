//! events ⇄ storyline.

use std::collections::BTreeMap;

use serde_json::json;

use crate::convert::message_text;
use crate::formats::events::{EventIdentity, EventRecord, EventsDocument};
use crate::formats::storyline::{StoryLink, StorylineAgent, StorylineDocument, StorylineTurn};
use crate::{Error, Result};

/// Resolve and project exactly one canonical Storyline from append-ordered events.
///
/// Identity-free input is invalid, but conflicting claims are resolved in
/// append order: the last non-empty value wins. Physical event replay overlays
/// its routing identity first, so canonical storage remains deterministically
/// session-scoped without rejecting producer metadata at ingest.
pub fn project_event_records(records: &[EventRecord]) -> Result<StorylineDocument> {
    records
        .iter()
        .try_for_each(EventRecord::validate)
        .map_err(|error| crate::Error::Other(error.to_string()))?;
    let session_id = records
        .iter()
        .rev()
        .find_map(event_storyline_key)
        .ok_or_else(|| {
            Error::Other("canonical event requires session_id, storyline_id, or run_id".into())
        })?
        .to_string();
    let run_id = records.iter().rev().find_map(|record| {
        record
            .identity
            .run_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(str::to_string)
    });
    let mut story = events_to_storyline_unchecked(records)?;
    story.session_id = session_id.clone();
    story.run_id = run_id.clone().filter(|run_id| run_id != &session_id);
    if let Some(parent_session_id) = run_id.filter(|run_id| run_id != &session_id) {
        story.parent = Some(StoryLink {
            parent_session_id,
            spawn_call_id: None,
            spawn_id: None,
            relation: "spawn".into(),
        });
    }
    Ok(story)
}

pub(crate) fn event_storyline_key(record: &EventRecord) -> Option<&str> {
    record
        .session_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .or_else(|| {
            record
                .identity
                .storyline_id
                .as_deref()
                .filter(|id| !id.is_empty())
        })
        .or_else(|| {
            record
                .identity
                .run_id
                .as_deref()
                .filter(|id| !id.is_empty())
        })
}

pub fn events_to_storyline(doc: &EventsDocument) -> Result<StorylineDocument> {
    project_event_records(&doc.events)
}

fn events_to_storyline_unchecked(events: &[EventRecord]) -> Result<StorylineDocument> {
    let session_id = events
        .iter()
        .rev()
        .find_map(|event| event.session_id.clone())
        .unwrap_or_else(|| "unknown".into());
    let agent_id = events
        .iter()
        .rev()
        .find_map(|event| event.agent_id.clone())
        .unwrap_or_else(|| "unknown".into());

    let mut by_call: BTreeMap<String, Vec<&EventRecord>> = BTreeMap::new();
    let mut first_call_position = BTreeMap::<String, usize>::new();
    let mut orphans: Vec<&EventRecord> = Vec::new();
    for (position, ev) in events.iter().enumerate() {
        match &ev.call_id {
            Some(cid) if !cid.is_empty() => {
                first_call_position.entry(cid.clone()).or_insert(position);
                by_call.entry(cid.clone()).or_default().push(ev);
            }
            _ => orphans.push(ev),
        }
    }

    let mut turns = Vec::new();
    let mut next_id = 1i64;

    let mut call_order: Vec<(usize, String)> = first_call_position
        .into_iter()
        .map(|(cid, position)| (position, cid))
        .collect();
    call_order.sort_by_key(|(position, _)| *position);

    for (_, cid) in call_order {
        let evs = &by_call[&cid];
        let mut model = None;
        let mut user_text = None;
        let mut asst_text = None;
        let mut metrics = None;
        let mut ttft_ms = None;
        let mut latency_from_payload = None;
        let mut req_ts = None;
        let mut resp_ts = None;
        let mut request_messages = None;
        for ev in evs {
            match ev.kind.as_str() {
                "llm.request" | "http.request" => {
                    req_ts = ev.timestamp.clone();
                    model = ev
                        .payload
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    user_text = extract_user(&ev.payload);
                    request_messages = ev.payload.get("messages").cloned();
                }
                "llm.response"
                | "llm.response.stream"
                | "http.response"
                | "http.response.stream" => {
                    resp_ts = ev.timestamp.clone();
                    asst_text = extract_assistant(&ev.payload);
                    if let Some(u) = ev.payload.get("usage") {
                        metrics = Some(u.clone());
                    }
                    ttft_ms = ev
                        .payload
                        .get("ttft_ms")
                        .and_then(|v| v.as_i64())
                        .or_else(|| {
                            metrics
                                .as_ref()
                                .and_then(|m| m.get("ttft_ms"))
                                .and_then(|v| v.as_i64())
                        });
                    if latency_from_payload.is_none() {
                        latency_from_payload =
                            ev.payload.get("latency_ms").and_then(|v| v.as_i64());
                    }
                }
                _ => {}
            }
        }

        let latency_ms =
            latency_from_payload.or_else(|| latency_between(req_ts.as_deref(), resp_ts.as_deref()));

        let req_seq = evs
            .iter()
            .find(|e| matches!(e.kind.as_str(), "llm.request" | "http.request"))
            .map(|e| e.seq);
        let resp_seq = evs
            .iter()
            .find(|e| {
                matches!(
                    e.kind.as_str(),
                    "llm.response"
                        | "llm.response.stream"
                        | "http.response"
                        | "http.response.stream"
                )
            })
            .map(|e| e.seq);

        if let Some(ut) = user_text.clone() {
            if asst_text.is_some() {
                let mut user_extra = json!({"call_id": cid});
                if let Some(seq) = req_seq {
                    user_extra["seq"] = json!(seq);
                }
                turns.push(StorylineTurn {
                    id: next_id,
                    kind: Some("llm.request".into()),
                    timestamp: req_ts
                        .clone()
                        .or_else(|| evs.first().and_then(|e| e.timestamp.clone())),
                    source: "user".into(),
                    message: serde_json::Value::String(ut),
                    reasoning_content: None,
                    reasoning_effort: None,
                    tool_calls: None,
                    observation: None,
                    metrics: None,
                    model_name: None,
                    llm_call_count: None,
                    is_copied_context: None,
                    latency_ms: None,
                    ttft_ms: None,
                    extra: Some(user_extra),
                });
                next_id += 1;
            }
        }

        let (source, message, turn_kind, turn_seq) = if let Some(a) = asst_text {
            (
                "agent",
                serde_json::Value::String(a),
                "llm.response",
                resp_seq,
            )
        } else if let Some(u) = user_text {
            ("user", serde_json::Value::String(u), "llm.request", req_seq)
        } else {
            (
                "agent",
                serde_json::Value::String(String::new()),
                "llm.response",
                resp_seq.or(req_seq),
            )
        };

        let mut agent_extra = json!({
            "call_id": cid,
            "request_messages": request_messages,
        });
        if let Some(seq) = turn_seq {
            agent_extra["seq"] = json!(seq);
        }

        turns.push(StorylineTurn {
            id: next_id,
            kind: Some(turn_kind.into()),
            timestamp: resp_ts
                .or(req_ts)
                .or_else(|| evs.first().and_then(|e| e.timestamp.clone())),
            source: source.into(),
            message,
            reasoning_content: None,
            reasoning_effort: None,
            tool_calls: None,
            observation: None,
            metrics,
            model_name: model,
            llm_call_count: if source == "agent" { Some(1) } else { None },
            is_copied_context: None,
            latency_ms: if source == "agent" { latency_ms } else { None },
            ttft_ms: if source == "agent" { ttft_ms } else { None },
            extra: Some(agent_extra),
        });
        next_id += 1;
    }

    for ev in orphans {
        if ev.kind.starts_with("session.") || ev.kind == "note" {
            turns.push(StorylineTurn {
                id: next_id,
                kind: Some("internal".into()),
                timestamp: ev.timestamp.clone(),
                source: "system".into(),
                message: json!({"kind": ev.kind, "payload": ev.payload}),
                reasoning_content: None,
                reasoning_effort: None,
                tool_calls: None,
                observation: None,
                metrics: None,
                model_name: None,
                llm_call_count: None,
                is_copied_context: None,
                latency_ms: None,
                ttft_ms: None,
                extra: Some(json!({"seq": ev.seq, "event_seq": ev.seq})),
            });
            next_id += 1;
        }
    }

    Ok(StorylineDocument {
        run_id: None,
        session_id,
        agent: StorylineAgent {
            id: agent_id.clone(),
            name: Some(agent_id),
            version: None,
            model_name: None,
            tool_definitions: None,
            extra: None,
        },
        parent: None,
        child_session_ids: None,
        notes: None,
        final_metrics: None,
        continued_trajectory_ref: None,
        extra: None,
        turns,
    })
}

pub fn storyline_to_events(story: &StorylineDocument) -> Result<EventsDocument> {
    story.validate()?;
    let mut events = Vec::new();
    let mut next_seq = 0u64;
    let mut i = 0;
    while i < story.turns.len() {
        let turn = &story.turns[i];
        if turn.effective_kind() == "internal" {
            let seq = take_seq(turn.extra.as_ref(), &mut next_seq);
            events.push(EventRecord {
                identity: EventIdentity::default(),
                seq,
                source: "pchronicle".into(),
                kind: "note".into(),
                timestamp: turn.timestamp.clone(),
                session_id: Some(story.session_id.clone()),
                agent_id: Some(story.agent.id.clone()),
                parent_uuid: None,
                trace_id: None,
                call_id: None,
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id: None,
                payload: turn.message.clone(),
            });
            i += 1;
            continue;
        }

        let explicit_call_id = turn
            .extra
            .as_ref()
            .and_then(|e| e.get("call_id").and_then(|c| c.as_str()))
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        // Pair consecutive user → agent turns under one call_id so events→storyline
        // can rebuild dialogue (orphans without call_id are otherwise dropped).
        let paired_agent = if turn.source == "user" {
            story
                .turns
                .get(i + 1)
                .filter(|t| t.source == "agent" && t.effective_kind() != "internal")
        } else {
            None
        };
        let call_id = explicit_call_id.or_else(|| {
            paired_agent
                .and_then(|a| {
                    a.extra
                        .as_ref()
                        .and_then(|e| e.get("call_id").and_then(|c| c.as_str()))
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                })
                .or_else(|| Some(format!("turn-{}", turn.id)))
        });

        let text = message_text(&turn.message).unwrap_or_default();

        match turn.source.as_str() {
            "user" => {
                let seq = take_seq(turn.extra.as_ref(), &mut next_seq);
                let req_kind = turn
                    .kind
                    .as_deref()
                    .filter(|k| k.starts_with("llm.") || k.starts_with("http."))
                    .unwrap_or("llm.request");
                events.push(EventRecord {
                    identity: EventIdentity::default(),
                    seq,
                    source: "pchronicle".into(),
                    kind: req_kind.into(),
                    timestamp: turn.timestamp.clone(),
                    session_id: Some(story.session_id.clone()),
                    agent_id: Some(story.agent.id.clone()),
                    parent_uuid: None,
                    trace_id: None,
                    call_id: call_id.clone(),
                    subagent_id: None,
                    parent_agent_id: None,
                    branch: None,
                    parent_call_id: None,
                    payload: json!({
                        "model": turn.model_name,
                        "messages": [{"role":"user","content": text}],
                    }),
                });

                if let Some(agent) = paired_agent {
                    let atext = message_text(&agent.message).unwrap_or_default();
                    let seq = take_seq(agent.extra.as_ref(), &mut next_seq);
                    let resp_kind = agent
                        .kind
                        .as_deref()
                        .filter(|k| k.starts_with("llm.") || k.starts_with("http."))
                        .unwrap_or("llm.response");
                    events.push(EventRecord {
                        identity: EventIdentity::default(),
                        seq,
                        source: "pchronicle".into(),
                        kind: resp_kind.into(),
                        timestamp: agent.timestamp.clone(),
                        session_id: Some(story.session_id.clone()),
                        agent_id: Some(story.agent.id.clone()),
                        parent_uuid: None,
                        trace_id: None,
                        call_id,
                        subagent_id: None,
                        parent_agent_id: None,
                        branch: None,
                        parent_call_id: None,
                        payload: json!({
                            "content": atext,
                            "choices":[{"message":{"role":"assistant","content": atext}}],
                            "usage": agent.metrics,
                            "latency_ms": agent.latency_ms,
                            "ttft_ms": agent.ttft_ms,
                        }),
                    });
                    i += 2;
                    continue;
                }
            }
            _ => {
                if let Some(msgs) = turn
                    .extra
                    .as_ref()
                    .and_then(|e| e.get("request_messages").cloned())
                {
                    let seq = take_seq(None, &mut next_seq);
                    events.push(EventRecord {
                        identity: EventIdentity::default(),
                        seq,
                        source: "pchronicle".into(),
                        kind: "llm.request".into(),
                        timestamp: turn.timestamp.clone(),
                        session_id: Some(story.session_id.clone()),
                        agent_id: Some(story.agent.id.clone()),
                        parent_uuid: None,
                        trace_id: None,
                        call_id: call_id.clone(),
                        subagent_id: None,
                        parent_agent_id: None,
                        branch: None,
                        parent_call_id: None,
                        payload: json!({
                            "model": turn.model_name,
                            "messages": msgs,
                        }),
                    });
                }
                let seq = take_seq(turn.extra.as_ref(), &mut next_seq);
                let resp_kind = turn
                    .kind
                    .as_deref()
                    .filter(|k| k.starts_with("llm.") || k.starts_with("http."))
                    .unwrap_or("llm.response");
                events.push(EventRecord {
                    identity: EventIdentity::default(),
                    seq,
                    source: "pchronicle".into(),
                    kind: resp_kind.into(),
                    timestamp: turn.timestamp.clone(),
                    session_id: Some(story.session_id.clone()),
                    agent_id: Some(story.agent.id.clone()),
                    parent_uuid: None,
                    trace_id: None,
                    call_id,
                    subagent_id: None,
                    parent_agent_id: None,
                    branch: None,
                    parent_call_id: None,
                    payload: json!({
                        "content": text,
                        "choices":[{"message":{"role":"assistant","content": text}}],
                        "usage": turn.metrics,
                        "latency_ms": turn.latency_ms,
                        "ttft_ms": turn.ttft_ms,
                    }),
                });
            }
        }
        i += 1;
    }

    Ok(EventsDocument {
        format: EventsDocument::FORMAT_NAME.into(),
        session_id: Some(story.session_id.clone()),
        agent_id: Some(story.agent.id.clone()),
        events,
    })
}

/// Prefer explicit `extra.seq`; otherwise allocate the next monotonic seq.
fn take_seq(extra: Option<&serde_json::Value>, next_seq: &mut u64) -> u64 {
    if let Some(seq) = extra.and_then(|e| e.get("seq")).and_then(|v| v.as_u64()) {
        if seq >= *next_seq {
            *next_seq = seq + 1;
        }
        return seq;
    }
    let seq = *next_seq;
    *next_seq += 1;
    seq
}

fn latency_between(req: Option<&str>, resp: Option<&str>) -> Option<i64> {
    let req_ms = parse_rfc3339_millis(req?)?;
    let resp_ms = parse_rfc3339_millis(resp?)?;
    Some(resp_ms - req_ms)
}

/// Best-effort RFC3339 → unix millis (UTC `Z` or `±HH:MM`). No chrono dependency.
fn parse_rfc3339_millis(s: &str) -> Option<i64> {
    // Accept `YYYY-MM-DDTHH:MM:SS[.fff]Z` or with offset.
    let s = s.trim();
    if s.len() < 20 {
        return None;
    }
    let date = &s[..10];
    let time = &s[11..];
    let (y, mo, d) = {
        let parts: Vec<_> = date.split('-').collect();
        if parts.len() != 3 {
            return None;
        }
        (
            parts[0].parse::<i64>().ok()?,
            parts[1].parse::<i64>().ok()?,
            parts[2].parse::<i64>().ok()?,
        )
    };
    let (hms, frac_and_tz) = time.split_once('.').unwrap_or((time, "0Z"));
    let hms_parts: Vec<_> = hms.split(':').collect();
    if hms_parts.len() != 3 {
        return None;
    }
    let (hh, mm, ss) = (
        hms_parts[0].parse::<i64>().ok()?,
        hms_parts[1].parse::<i64>().ok()?,
        hms_parts[2]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse::<i64>()
            .ok()?,
    );
    let (frac_str, tz) = if let Some(rest) = frac_and_tz.strip_suffix('Z') {
        (rest, 0i64)
    } else if let Some(pos) = frac_and_tz.rfind('+').or_else(|| {
        // find last '-' after digits start
        frac_and_tz
            .char_indices()
            .skip_while(|(_, c)| c.is_ascii_digit())
            .find(|(_, c)| *c == '-')
            .map(|(i, _)| i)
    }) {
        let (frac, off) = frac_and_tz.split_at(pos);
        let sign = if off.starts_with('+') { 1i64 } else { -1i64 };
        let off = off.trim_start_matches(['+', '-']);
        let op: Vec<_> = off.split(':').collect();
        let oh = op.first()?.parse::<i64>().ok()?;
        let om = op.get(1).and_then(|x| x.parse().ok()).unwrap_or(0);
        (frac, sign * (oh * 60 + om) * 60_000)
    } else if hms_parts[2].ends_with('Z') {
        ("0", 0)
    } else {
        return None;
    };
    let mut millis = 0i64;
    let digits: String = frac_str
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if !digits.is_empty() {
        let padded = format!("{:0<3}", digits.chars().take(3).collect::<String>());
        millis = padded.parse().ok()?;
    }
    // Days from civil date (Howard Hinnant algorithm)
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468; // days since 1970-01-01
    Some(days * 86_400_000 + hh * 3_600_000 + mm * 60_000 + ss * 1_000 + millis - tz)
}

fn extract_user(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("user_content")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            payload
                .get("messages")
                .and_then(|m| m.as_array())
                .and_then(|arr| {
                    arr.iter().rev().find_map(|m| {
                        if m.get("role").and_then(|r| r.as_str()) == Some("user") {
                            m.get("content").and_then(|c| match c {
                                serde_json::Value::String(s) => Some(s.clone()),
                                _ => None,
                            })
                        } else {
                            None
                        }
                    })
                })
        })
}

fn extract_assistant(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("assistant_content")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            payload
                .pointer("/choices/0/message/content")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            payload
                .get("content")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
}

#[cfg(test)]
mod projection_tests {
    use super::*;

    fn response(session_id: Option<&str>, call_id: &str, seq: u64, content: &str) -> EventRecord {
        EventRecord {
            identity: EventIdentity::default(),
            seq,
            source: "test".into(),
            kind: "llm.response".into(),
            timestamp: None,
            session_id: session_id.map(str::to_string),
            agent_id: Some("agent".into()),
            parent_uuid: None,
            trace_id: None,
            call_id: Some(call_id.into()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: json!({"content": content}),
        }
    }

    #[test]
    fn canonical_projection_rejects_missing_storyline_identity() {
        let error = project_event_records(&[response(None, "call", 0, "text")]).unwrap_err();
        assert!(error.to_string().contains("requires session_id"));
    }

    #[test]
    fn canonical_projection_uses_append_order_before_producer_seq() {
        let records = vec![
            response(Some("session"), "later-seq", 100, "first"),
            response(Some("session"), "earlier-seq", 1, "second"),
        ];
        let story = project_event_records(&records).unwrap();
        assert_eq!(story.turns[0].message, json!("first"));
        assert_eq!(story.turns[1].message, json!("second"));
    }

    #[test]
    fn public_projection_resolves_conflicting_identity_in_append_order() {
        let mut first = response(Some("first"), "call-1", 0, "one");
        first.identity.run_id = Some("run-first".into());
        first.agent_id = Some("agent-first".into());
        let mut second = response(Some("second"), "call-2", 1, "two");
        second.identity.run_id = Some("run-second".into());
        second.agent_id = Some("agent-second".into());
        let story = events_to_storyline(&EventsDocument::new(vec![first, second])).unwrap();
        assert_eq!(story.session_id, "second");
        assert_eq!(story.run_id.as_deref(), Some("run-second"));
        assert_eq!(story.agent.id, "agent-second");

        let mut conflicting = response(Some("session"), "call", 0, "text");
        conflicting.identity.storyline_id = Some("other".into());
        let story = project_event_records(&[conflicting]).unwrap();
        assert_eq!(story.session_id, "session");
    }
}
