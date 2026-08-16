//! Cross-crate capture corpus sourced from `persisting-gateway/tests/fixtures`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use persisting_pchronicle::{
    event_record_to_event_row, event_row_to_event_record, event_rows_from_batch,
    event_rows_to_batch, raw_event_arrow_schema, EventRecord, RawEventLanceStore, StoryCoords,
};
use serde_json::{json, Value};

fn capture_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../persisting-gateway/tests/fixtures")
}

fn capture_tests_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../persisting-gateway/tests")
}

fn collect_capture_payload_paths() -> Result<Vec<PathBuf>> {
    fn visit(dir: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
        for entry in
            std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))?
        {
            let path = entry?.path();
            if path.is_dir() {
                visit(&path, paths)?;
            } else if matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("json" | "snap" | "sse" | "txt") | None
            ) {
                paths.push(path);
            }
        }
        Ok(())
    }

    let mut paths = Vec::new();
    visit(&capture_fixture_root(), &mut paths)?;
    visit(
        &capture_tests_root().join("capture/apps/claude/fixtures"),
        &mut paths,
    )?;
    paths.sort();
    Ok(paths)
}

fn capture_payload_records() -> Result<(Vec<EventRecord>, usize, usize)> {
    let root = capture_tests_root();
    let paths = collect_capture_payload_paths()?;
    let mut json_documents = 0;
    let mut raw_streams = 0;
    let records = paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let raw = std::fs::read_to_string(path)
                .with_context(|| format!("read capture fixture {}", path.display()))?;
            let (encoding, document) = match serde_json::from_str::<Value>(&raw) {
                Ok(value) => {
                    json_documents += 1;
                    ("json", value)
                }
                Err(_) => {
                    raw_streams += 1;
                    ("raw-stream", Value::String(raw))
                }
            };
            let relative = path
                .strip_prefix(&root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            Ok(EventRecord {
                identity: persisting_pchronicle::EventIdentity::default(),
                seq: index as u64,
                source: "persisting-gateway-fixture".into(),
                kind: "fixture.capture_payload".into(),
                timestamp: Some(format!("2026-01-01T00:00:{:02}Z", index % 60)),
                session_id: Some("capture-fixture-corpus".into()),
                agent_id: Some("fixture-agent".into()),
                parent_uuid: None,
                trace_id: Some(format!("fixture-trace-{index}")),
                call_id: Some(format!("fixture-call-{index}")),
                subagent_id: None,
                parent_agent_id: None,
                branch: Some("fixture".into()),
                parent_call_id: (index > 0).then(|| format!("fixture-call-{}", index - 1)),
                payload: json!({
                    "fixture_path": relative,
                    "encoding": encoding,
                    "document": document,
                }),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((records, json_documents, raw_streams))
}

#[test]
fn capture_payload_corpus_roundtrips_through_wire_and_arrow() -> Result<()> {
    let (mut records, json_documents, raw_streams) = capture_payload_records()?;
    for (index, record) in records.iter_mut().enumerate() {
        record.identity.event_id = Some(format!("fixture-event-{index}"));
    }
    assert!(
        records.len() >= 170,
        "expected the full Capture request/response/snapshot/SSE corpus"
    );
    assert!(
        json_documents >= 46,
        "expected structured cross-provider fixtures"
    );
    assert!(
        raw_streams >= 124,
        "expected snapshots and SSE fixtures to be preserved as raw payloads"
    );

    let rows = records
        .iter()
        .map(event_record_to_event_row)
        .collect::<Result<Vec<_>>>()?;
    for (row, expected) in rows.iter().zip(&records) {
        assert_eq!(event_row_to_event_record(row)?, *expected);
    }

    let batch = event_rows_to_batch(raw_event_arrow_schema(), &rows)?;
    let restored_rows = event_rows_from_batch(&batch)?;
    assert_eq!(restored_rows, rows);
    let restored_records = restored_rows
        .iter()
        .map(event_row_to_event_record)
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(restored_records, records);
    Ok(())
}

#[tokio::test]
async fn capture_payload_corpus_roundtrips_through_lance() -> Result<()> {
    let (records, _, _) = capture_payload_records()?;
    let dir = tempfile::tempdir()?;
    let session = StoryCoords::new(
        dir.path().to_string_lossy(),
        "fixture-agent",
        "capture-fixture-corpus",
        None,
    );
    let outcome = RawEventLanceStore.append_events(&session, &records).await?;
    assert_eq!(outcome.accepted_records, records.len());
    assert_eq!(outcome.persisted_units, records.len());

    let mut restored = RawEventLanceStore.read_events(&session, 0, None).await?;
    assert!(restored.iter().all(|record| {
        record.identity.run_id.as_deref() == Some("capture-fixture-corpus")
            && record.identity.storyline_id.as_deref() == Some("capture-fixture-corpus")
            && record.identity.timestamp_unix_ms.is_some()
            && record.identity.producer.as_deref() == Some("persisting-gateway-fixture")
    }));
    for record in &mut restored {
        record.identity = persisting_pchronicle::EventIdentity::default();
    }
    assert_eq!(restored, records);
    Ok(())
}
