//! Capture ↔ pChronicle bridge: mapping, materialize, compact (not pure codec).

use persisting_capture::dialogue::{
    agenticmd_block_to_capture_record, capture_record_to_agenticmd_block,
};
use persisting_capture::markdown_trajectory::{
    encode_agenticmd_block_validated, format_document_preamble, parse_document,
};
use persisting_capture::sink::{llm_request_record, llm_response_record};
use persisting_capture::trajectory_convert::{
    markdown_document_to_capture_records, markdown_document_to_engine_lines,
    materialize_records_to_markdown,
};
use persisting_capture::Call;
use persisting_pchronicle::parse_agenticmd_document;
use serde_json::json;

fn fixture() -> &'static str {
    include_str!("fixtures/tlv/demo-run-001.md")
}

#[test]
fn encode_then_both_parsers_agree() {
    let call = Call {
        call_id: "c1".into(),
        trace_id: "t1".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
    };
    let mut req = llm_request_record(
        Some("s1".into()),
        None,
        "m",
        "/v1/chat/completions",
        &json!({"messages":[{"role":"user","content":"hi"}]}),
    );
    req.timestamp = Some("2026-01-01T00:00:00Z".into());
    let mut resp = llm_response_record(
        Some("s1".into()),
        None,
        200,
        &json!({"choices":[{"message":{"role":"assistant","content":"yo"}}]}),
        false,
        &call,
    );
    resp.call_id = Some("c1".into());
    resp.timestamp = Some("2026-01-01T00:00:00Z".into());

    let mut out = format_document_preamble(None).unwrap();
    for rec in [req, resp] {
        let block = capture_record_to_agenticmd_block(&rec).unwrap();
        out.push_str(&encode_agenticmd_block_validated(&block).unwrap());
    }

    let a = parse_document(&out).unwrap();
    let b = parse_agenticmd_document(&out).unwrap().blocks;
    assert_eq!(a.len(), b.len());
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        assert_eq!(x.body, y.body, "encoded body[{i}]");
        agenticmd_block_to_capture_record(x).unwrap_or_else(|e| panic!("block[{i}]: {e:#}"));
    }
}

#[test]
fn compact_path_uses_chronicle_parse() {
    let records = markdown_document_to_capture_records(fixture()).expect("compact parse");
    assert_eq!(records.len(), 3);
    assert!(records.iter().any(|r| {
        r.payload
            .get("_tlv")
            .and_then(|v| v.get("role"))
            .and_then(|v| v.as_str())
            == Some("user")
    }));
}

#[test]
fn import_and_compact_share_enriched_dialogue_path() {
    let doc = fixture();
    let via_engine_lines = markdown_document_to_engine_lines(doc).unwrap();
    let records = markdown_document_to_capture_records(doc).unwrap();
    assert_eq!(records.len(), 3);
    for rec in &records {
        assert!(
            rec.payload
                .get("_tlv")
                .and_then(|v| v.get("block_fields"))
                .is_some(),
            "expected _tlv.block_fields on seq={}",
            rec.seq
        );
    }
    assert_eq!(
        records[1].call_id.as_deref(),
        Some("call-demo-1"),
        "assistant call_id preserved"
    );
    assert_eq!(
        records[1].payload["_tlv"]["block_fields"]["trace_id"].as_str(),
        Some("trace-demo-1")
    );
    assert!(!via_engine_lines.is_empty());
}

#[test]
fn capture_record_maps_through_agenticmd_block() {
    use persisting_pchronicle::encode_agenticmd_block;

    let mut req = llm_request_record(
        Some("s1".into()),
        None,
        "m",
        "/v1/chat/completions",
        &json!({"messages":[{"role":"user","content":"hi"}]}),
    );
    req.seq = 7;
    req.call_id = Some("c7".into());
    let block = capture_record_to_agenticmd_block(&req).unwrap();
    assert_eq!(block.header.type_name, "markdown");
    assert_eq!(block.role(), Some("user"));
    let wire = encode_agenticmd_block(&block).unwrap();
    assert!(wire.contains("\"seq\":7") || wire.contains("\"seq\": 7"));
    let back = agenticmd_block_to_capture_record(&block).unwrap();
    assert_eq!(back.seq, 7);
    assert_eq!(back.call_id.as_deref(), Some("c7"));
}

#[test]
fn materialize_writes_chronicle_parseable_markdown() {
    let call = Call {
        call_id: "c1".into(),
        trace_id: "t1".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
    };
    let mut req = llm_request_record(
        Some("s1".into()),
        None,
        "m",
        "/v1/chat/completions",
        &json!({"messages":[{"role":"user","content":"hi"}]}),
    );
    req.timestamp = Some("2026-01-01T00:00:00Z".into());
    let mut resp = llm_response_record(
        Some("s1".into()),
        None,
        200,
        &json!({"choices":[{"message":{"role":"assistant","content":"yo"}}]}),
        false,
        &call,
    );
    resp.call_id = Some("c1".into());
    resp.timestamp = Some("2026-01-01T00:00:00Z".into());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sess.md");
    materialize_records_to_markdown(&path, &[req, resp]).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let blocks = parse_document(&text).unwrap();
    assert_eq!(blocks.len(), 2);
    let chronicle = parse_agenticmd_document(&text).unwrap();
    assert_eq!(chronicle.blocks.len(), 2);
}
