use std::path::Path;

use super::actf::ActfFormat;
use super::atif::AtifFormat;
use super::claude_code::ClaudeCodeFormat;
use super::codec::{ProbeConfidence, TrajectoryFormat};
use super::codex::CodexFormat;
use super::openai_corpus::OpenaiMsgFormat;
use super::storyline::StorylineFormat;
use crate::InputResult;
use crate::format::DocumentFormat;

pub(crate) static FORMATS: &[&dyn TrajectoryFormat] = &[
    &ClaudeCodeFormat,
    &CodexFormat,
    &AtifFormat,
    &ActfFormat,
    &StorylineFormat,
    &OpenaiMsgFormat,
];

pub fn get(format: DocumentFormat) -> Option<&'static dyn TrajectoryFormat> {
    FORMATS.iter().copied().find(|codec| codec.id() == format)
}

#[cfg_attr(not(feature = "lance-store"), allow(dead_code))]
pub fn supports_direct_query(format: DocumentFormat) -> bool {
    get(format).is_some_and(|codec| codec.capabilities().direct_query)
}

#[cfg_attr(not(feature = "lance-store"), allow(dead_code))]
pub fn is_direct_query_candidate(path: &Path) -> bool {
    FORMATS
        .iter()
        .any(|codec| codec.capabilities().direct_query && codec.is_candidate(path))
}

/// Pick the registered codec with the highest probe confidence.
pub fn detect(path: Option<&Path>, content: &[u8]) -> InputResult<Option<DocumentFormat>> {
    let mut best: Option<(ProbeConfidence, DocumentFormat)> = None;
    for codec in FORMATS {
        let confidence = codec.probe(path, content)?;
        if confidence == ProbeConfidence::None {
            continue;
        }
        if best.is_none_or(|(current, _)| confidence > current) {
            best = Some((confidence, codec.id()));
        }
    }
    Ok(best.map(|(_, format)| format))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::codec::{DecodeContext, DocumentSource};
    use std::io::Cursor;

    #[test]
    fn claude_registry_detects_fingerprint_over_rollout_path() {
        let claude = br#"{"type":"user","sessionId":"sess-1","uuid":"u1","message":{"role":"user","content":"hi"}}"#;
        assert_eq!(
            detect(Some(Path::new("rollout-mismatch.jsonl")), claude).unwrap(),
            Some(DocumentFormat::ClaudeCode)
        );
    }

    #[test]
    fn claude_registry_decodes_through_trait() {
        let input = br#"{"type":"user","sessionId":"s","uuid":"u1","message":{"role":"user","content":"hi"}}
"#;
        let codec = get(DocumentFormat::ClaudeCode).expect("claude codec");
        let stories = crate::formats::codec::decode_all(
            codec,
            &mut Cursor::new(&input[..]),
            &DocumentSource::new("sess.jsonl"),
        )
        .unwrap();
        assert_eq!(stories[0].session_id, "s");
        assert!(codec.encode(&stories, &mut Vec::new()).is_err());
    }

    #[test]
    fn codex_registry_detects_fingerprint_and_rollout_path() {
        let codex = br#"{"timestamp":"2026-08-03T08:15:11.000Z","type":"session_meta","payload":{"id":"s"}}"#;
        assert_eq!(detect(None, codex).unwrap(), Some(DocumentFormat::Codex));
        assert_eq!(
            detect(Some(Path::new("rollout-empty.jsonl")), b"\n").unwrap(),
            Some(DocumentFormat::Codex)
        );
        let claude = br#"{"type":"user","sessionId":"sess-1","uuid":"u1","message":{"role":"user","content":"hi"}}"#;
        assert_eq!(
            detect(Some(Path::new("rollout-mismatch.jsonl")), claude).unwrap(),
            Some(DocumentFormat::ClaudeCode)
        );
    }

    #[test]
    fn codex_registry_decodes_through_trait() {
        let input =
            br#"{"timestamp":"2026-08-03T08:15:11.000Z","type":"session_meta","payload":{"id":"s"}}
"#;
        let codec = get(DocumentFormat::Codex).expect("codex codec");
        let stories = crate::formats::codec::decode_all(
            codec,
            &mut Cursor::new(&input[..]),
            &DocumentSource::new("rollout-s.jsonl"),
        )
        .unwrap();
        assert_eq!(stories[0].session_id, "s");
        assert!(codec.encode(&stories, &mut Vec::new()).is_err());
        assert!(codec.capabilities().direct_query);
        assert!(!codec.capabilities().encode);
    }

    const ATIF_DOC: &str = r#"{"schema_version":"ATIF-v1.7","trajectory_id":"one","agent":{"name":"agent","version":"1"},"steps":[]}"#;

    #[test]
    fn atif_registry_detects_schema_and_steps_agent_fingerprint() {
        assert_eq!(
            detect(None, ATIF_DOC.as_bytes()).unwrap(),
            Some(DocumentFormat::Atif)
        );
        let without_version =
            br#"{"session_id":"s","agent":{"name":"a","version":"1"},"steps":[]}"#;
        assert_eq!(
            detect(None, without_version).unwrap(),
            Some(DocumentFormat::Atif)
        );
        assert_eq!(detect(None, b"\n").unwrap(), None);
    }

    #[test]
    fn atif_registry_roundtrips_through_trait() {
        let codec = get(DocumentFormat::Atif).expect("atif codec");
        let stories = crate::formats::codec::decode_all(
            codec,
            &mut Cursor::new(ATIF_DOC.as_bytes()),
            &DocumentSource::new("a.json"),
        )
        .unwrap();
        assert_eq!(stories[0].document_id(), "one");
        assert!(codec.capabilities().encode);
        assert!(codec.capabilities().direct_query);
        let mut encoded = Vec::new();
        codec.encode(&stories, &mut encoded).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(value["trajectory_id"], "one");
        assert_eq!(value["schema_version"], "ATIF-v1.7");
    }

    const ACTF_DOC: &str = r#"{"task_id":"one","category":"software-engineering","k":1,"correct":false,"attempts_tried":1,"solved_at":null,"attempts":{"1":{"correct":false,"trajectory":{"schema_version":"ACTF_v1.0","steps":[{"step_id":1,"assistant_content":{"content":"done"},"metric":{},"system_prompt":"sys","user_content":"task","started_at":"2026-01-01 00:00:00+00:00","finished_at":"2026-01-01 00:00:01+00:00"}],"started_at":"2026-01-01 00:00:00+00:00","finished_at":"2026-01-01 00:00:01+00:00"}}}}"#;

    #[test]
    fn actf_registry_detects_schema_and_event_log_fingerprint() {
        assert_eq!(
            detect(None, ACTF_DOC.as_bytes()).unwrap(),
            Some(DocumentFormat::Actf)
        );
        let event_log = br#"{"task_id":"t","category":"c","k":1,"correct":false,"attempts_tried":1,"attempts":{"1":{"correct":false,"trajectory":[{"type":"session","id":"s"}]}}}"#;
        assert_eq!(detect(None, event_log).unwrap(), Some(DocumentFormat::Actf));
        assert_eq!(
            detect(Some(Path::new("task.actf.json")), b"\n").unwrap(),
            Some(DocumentFormat::Actf)
        );
        assert_eq!(detect(None, b"\n").unwrap(), None);
    }

    #[test]
    fn actf_registry_roundtrips_through_trait() {
        let codec = get(DocumentFormat::Actf).expect("actf codec");
        let stories = crate::formats::codec::decode_all(
            codec,
            &mut Cursor::new(ACTF_DOC.as_bytes()),
            &DocumentSource::new("task.actf.json"),
        )
        .unwrap();
        assert_eq!(stories[0].session_id, "one");
        assert!(codec.capabilities().encode);
        assert!(codec.capabilities().direct_query);
        let mut encoded = Vec::new();
        codec.encode(&stories, &mut encoded).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(value["task_id"], "one");
        assert_eq!(
            value["attempts"]["1"]["trajectory"]["schema_version"],
            "ACTF_v1.0"
        );
    }

    const STORYLINE_DOC: &str =
        r#"{"schema_version":"storyline/v1","session":"s","agent":{"id":"a"},"turns":[]}"#;

    #[test]
    fn storyline_registry_detects_schema_and_suffix_fingerprint() {
        assert_eq!(
            detect(None, STORYLINE_DOC.as_bytes()).unwrap(),
            Some(DocumentFormat::Storyline)
        );
        let array = format!("[{STORYLINE_DOC}]");
        assert_eq!(
            detect(None, array.as_bytes()).unwrap(),
            Some(DocumentFormat::Storyline)
        );
        assert_eq!(
            detect(Some(Path::new("trajectory.storyline.json")), b"\n").unwrap(),
            Some(DocumentFormat::Storyline)
        );
        assert_eq!(detect(None, b"\n").unwrap(), None);
    }

    #[test]
    fn storyline_registry_roundtrips_through_trait() {
        let codec = get(DocumentFormat::Storyline).expect("storyline codec");
        let stories = crate::formats::codec::decode_all(
            codec,
            &mut Cursor::new(STORYLINE_DOC.as_bytes()),
            &DocumentSource::new("trajectory.storyline.json"),
        )
        .unwrap();
        assert_eq!(stories[0].session_id, "s");
        assert!(codec.capabilities().encode);
        assert!(codec.capabilities().direct_query);
        let mut encoded = Vec::new();
        codec.encode(&stories, &mut encoded).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(value["schema_version"], "storyline/v1");
        assert_eq!(value["session"], "s");
        assert!(value.is_object());
    }

    const OPENAI_DOC: &str = r#"{"session_steps":[{"session_id":"s","step_id":1,"messages":[{"role":"user","content":"hi"}],"response":{"role":"assistant","content":"ok"}}]}"#;

    #[test]
    fn openai_registry_detects_row_and_envelope_fingerprint() {
        assert_eq!(
            detect(None, OPENAI_DOC.as_bytes()).unwrap(),
            Some(DocumentFormat::OpenaiMsg)
        );
        let row = br#"{"session_id":"s","step_id":1,"messages":[{"role":"user","content":"hi"}]}"#;
        assert_eq!(detect(None, row).unwrap(), Some(DocumentFormat::OpenaiMsg));
        assert_eq!(
            detect(Some(Path::new("session_steps.json")), b"\n").unwrap(),
            Some(DocumentFormat::OpenaiMsg)
        );
        assert_eq!(detect(None, b"\n").unwrap(), None);
    }

    #[test]
    fn openai_registry_roundtrips_through_trait() {
        let codec = get(DocumentFormat::OpenaiMsg).expect("openai codec");
        let stories = crate::formats::codec::decode_all(
            codec,
            &mut Cursor::new(OPENAI_DOC.as_bytes()),
            &DocumentSource::new("corpus.json"),
        )
        .unwrap();
        assert_eq!(stories[0].session_id, "s");
        assert!(codec.capabilities().encode);
        assert!(codec.capabilities().direct_query);
        let mut encoded = Vec::new();
        codec.encode(&stories, &mut encoded).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(value["session_steps"][0]["session_id"], "s");
        assert_eq!(value["session_steps"][0]["step_id"], 1);
    }

    #[test]
    fn handlers_own_candidate_file_selection() {
        let claude = get(DocumentFormat::ClaudeCode).expect("claude codec");
        assert!(claude.is_candidate(Path::new("session.jsonl")));
        assert!(!claude.is_candidate(Path::new(".meta.json")));
        assert!(!claude.is_candidate(Path::new("agent.meta.json")));
        assert!(!claude.is_candidate(Path::new("notes.md")));

        let openai = get(DocumentFormat::OpenaiMsg).expect("openai codec");
        assert!(openai.is_candidate(Path::new("session_steps.json")));
        assert!(!openai.is_candidate(Path::new("session.jsonl")));

        let atif = get(DocumentFormat::Atif).expect("atif codec");
        assert!(atif.is_candidate(Path::new("run.json")));
        assert!(atif.is_candidate(Path::new("run.jsonl")));

        assert!(is_direct_query_candidate(Path::new("session.jsonl")));
        assert!(!is_direct_query_candidate(Path::new(".meta.json")));
        assert!(!is_direct_query_candidate(Path::new("notes.md")));
    }

    #[test]
    fn atif_decode_context_enforces_record_limits_and_reports_peak() {
        let codec = get(DocumentFormat::Atif).expect("atif codec");
        let oversized = format!(
            r#"{{"schema_version":"ATIF-v1.7","trajectory_id":"one","agent":{{"name":"a","version":"1"}},"steps":[],"pad":"{}"}}"#,
            "x".repeat(600)
        );
        let source = DocumentSource::new("t.jsonl");
        let ctx = DecodeContext::new(&source).with_limits(u64::MAX, 512);
        let error = codec
            .decode(
                &mut Cursor::new(oversized.as_bytes()),
                &ctx,
                &mut |_| Ok(()),
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("max_record_bytes 512"),
            "{error}"
        );

        let source = DocumentSource::new("a.jsonl");
        let ctx = DecodeContext::new(&source);
        let report = codec
            .decode(
                &mut Cursor::new(format!("{ATIF_DOC}\n").as_bytes()),
                &ctx,
                &mut |_| Ok(()),
            )
            .unwrap();
        assert_eq!(report.documents, 1);
        assert!(report.peak_record_bytes >= ATIF_DOC.len());
    }
}
