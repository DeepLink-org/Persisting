//! Cross-crate compatibility corpus sourced from `persisting-gateway/tests/fixtures`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use persisting_pchronicle::{
    agenticmd_blocks_to_event_records, decode_event_lines, encode_event_lines,
    event_record_to_event_row, event_row_to_event_record, event_rows_from_batch,
    event_rows_to_batch, from_storyline, index_agenticmd_path, into_storyline,
    markdown_document_to_event_records, materialize_lance_to_markdown,
    parse_agenticmd_document_validated, read_agenticmd_blocks_from_file,
    session_markdown_write_path_for_key, trajectory_arrow_schema, write_agenticmd_document,
    ChronicleFormat, EventRecord, LanceEventStore, StoryCoords, StructuredStore,
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
fn capture_agenticmd_fixture_exercises_full_document_stack() -> Result<()> {
    let source = capture_fixture_root().join("tlv/demo-run-001.md");
    let document =
        std::fs::read_to_string(&source).with_context(|| format!("read {}", source.display()))?;

    let strict_blocks = parse_agenticmd_document_validated(&document)?;
    assert_eq!(strict_blocks.len(), 3);
    assert_eq!(
        strict_blocks
            .iter()
            .map(|block| block.role())
            .collect::<Vec<_>>(),
        vec![Some("user"), Some("assistant"), Some("note")]
    );

    let strict_events = agenticmd_blocks_to_event_records(&strict_blocks)?;
    let document_events = markdown_document_to_event_records(&document)?;
    assert_eq!(document_events, strict_events);
    assert_eq!(strict_events[0].payload["_tlv"]["role"], "user");
    assert_eq!(strict_events[1].call_id.as_deref(), Some("call-demo-1"));
    assert_eq!(
        strict_events[1].payload["_tlv"]["block_fields"]["trace_id"],
        "trace-demo-1"
    );

    let wire = encode_event_lines(&strict_events)?;
    assert_eq!(decode_event_lines(&wire)?, strict_events);

    let dir = tempfile::tempdir()?;
    let copied = dir.path().join("demo-run-001.md");
    let body_offset = persisting_pchronicle::agenticmd_body_byte_offset(&document)?;
    write_agenticmd_document(&copied, &document[..body_offset], &strict_blocks)?;
    assert_eq!(read_agenticmd_blocks_from_file(&copied)?, strict_blocks);
    let index = index_agenticmd_path(&copied)?;
    assert_eq!(index.block_count, 3);
    assert_eq!(
        index.call_ids.into_iter().collect::<Vec<_>>(),
        vec!["call-demo-1"]
    );
    assert!(index.structural_issues.is_empty());

    let storyline = into_storyline(ChronicleFormat::Agenticmd, &document)?;
    let regenerated = from_storyline(ChronicleFormat::Agenticmd, &storyline)?;
    let regenerated_blocks = parse_agenticmd_document_validated(&regenerated)?;
    assert_eq!(
        regenerated_blocks
            .iter()
            .map(|block| (block.role(), block.body.as_str()))
            .collect::<Vec<_>>(),
        strict_blocks
            .iter()
            .map(|block| (block.role(), block.body.as_str()))
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
fn capture_payload_corpus_roundtrips_through_wire_and_arrow() -> Result<()> {
    let (records, json_documents, raw_streams) = capture_payload_records()?;
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

    let wire = encode_event_lines(&records)?;
    assert_eq!(decode_event_lines(&wire)?, records);

    let rows = records
        .iter()
        .enumerate()
        .map(|(index, record)| event_record_to_event_row(record, index as i64))
        .collect::<Result<Vec<_>>>()?;
    for (row, expected) in rows.iter().zip(&records) {
        assert_eq!(event_row_to_event_record(row)?, *expected);
    }

    let batch = event_rows_to_batch(trajectory_arrow_schema(), &rows)?;
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
    let outcome = LanceEventStore.append_events(&session, &records).await?;
    assert_eq!(outcome.accepted_records, records.len());
    assert_eq!(outcome.persisted_units, records.len());

    let restored = LanceEventStore.read_events(&session, 0, None).await?;
    assert_eq!(restored, records);
    Ok(())
}

#[tokio::test]
async fn capture_agenticmd_fixture_materializes_from_canonical_lance() -> Result<()> {
    let source = capture_fixture_root().join("tlv/demo-run-001.md");
    let document = std::fs::read_to_string(&source)?;
    let source_blocks = parse_agenticmd_document_validated(&document)?;
    let mut events = agenticmd_blocks_to_event_records(&source_blocks)?;
    for (seq, event) in events.iter_mut().enumerate() {
        event.seq = seq as u64;
    }

    let dir = tempfile::tempdir()?;
    let session = StoryCoords::new(
        dir.path().to_string_lossy(),
        "fixture-agent",
        "demo-run-001",
        None,
    );
    LanceEventStore.append_events(&session, &events).await?;
    let outcome = materialize_lance_to_markdown(&session).await?;
    assert_eq!(outcome.stats.source_events, 3);
    assert_eq!(outcome.stats.markdown_blocks, 3);
    assert_eq!(outcome.stats.skipped_events, 0);

    let run_dir = session.run_dir()?;
    let path = session_markdown_write_path_for_key(&run_dir, "demo-run-001");
    let materialized = read_agenticmd_blocks_from_file(&path)?;
    assert_eq!(
        materialized
            .iter()
            .map(|block| (block.role(), block.body.as_str()))
            .collect::<Vec<_>>(),
        source_blocks
            .iter()
            .map(|block| (block.role(), block.body.as_str()))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        LanceEventStore.read_events(&session, 0, None).await?,
        events
    );
    Ok(())
}
