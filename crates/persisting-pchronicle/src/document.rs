//! pChronicle 物理文档格式的识别、转换与统一读取入口。
//!
//! Codex 与 Claude Code 会话 JSONL 只解码为 Storyline，不提供编码。

use std::path::Path;

#[cfg(feature = "lance-store")]
use datafusion::prelude::SessionContext;

pub use crate::agenticmd::{
    AgenticmdFileIndex, agenticmd_block_count, agenticmd_structural_issues, count_agenticmd_role,
    encode_agenticmd, index_agenticmd_path, list_agenticmd_paths,
    rewrite_agenticmd_storyline_metadata, upsert_agenticmd_turn, write_agenticmd_storyline,
};
pub use crate::convert::{events_to_storyline, project_event_records, storyline_to_events};
pub use crate::format::DocumentFormat;
pub use crate::formats::detect_format;
pub use crate::input::{InputIssue, InputIssueKind, InputResult};
pub use crate::interop::{
    events_to_har, events_to_otlp_json, langfuse_otlp_json_to_events, otlp_json_to_events,
};

pub type Result<T> = anyhow::Result<T>;

use crate::formats::StorylineDocument;
use crate::formats::unknown_fields::{UnknownFieldLimits, validate_unknown_fields};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DocumentCodecOptions {
    pub unknown_fields: UnknownFieldLimits,
}

/// 将 AgenticMD 语义文档解码为权威 Storyline，不暴露 Markdown AST。
pub fn decode_agenticmd(input: &str) -> InputResult<StorylineDocument> {
    crate::agenticmd::parse_agenticmd(input)
}

/// 将外围 JSON 文档解码为权威 Storyline；wire DTO 不进入公共 API。
///
/// 容器形态与顺序属于 Storyline 的集合语义，会随 Storyline 一起持久化；
/// 不依赖进程内 sidecar。
pub fn decode_json_storylines(
    format: DocumentFormat,
    input: &str,
    relative_path: impl AsRef<Path>,
) -> InputResult<Vec<StorylineDocument>> {
    decode_json_storylines_with_options(
        format,
        input,
        relative_path,
        DocumentCodecOptions::default(),
    )
}

pub fn decode_json_storylines_with_options(
    format: DocumentFormat,
    input: &str,
    relative_path: impl AsRef<Path>,
    options: DocumentCodecOptions,
) -> InputResult<Vec<StorylineDocument>> {
    options.unknown_fields.validate()?;
    let relative_path = relative_path.as_ref();
    if let Some(handler) = crate::formats::registry::get(format) {
        let stories = crate::formats::codec::decode_all(
            handler,
            &mut std::io::Cursor::new(input.as_bytes()),
            &crate::formats::codec::DocumentSource::new(
                relative_path
                    .to_str()
                    .unwrap_or("session.jsonl")
                    .replace('\\', "/"),
            ),
        )?;
        for story in &stories {
            story.validate()?;
            validate_unknown_fields(&story.unknown_fields, options.unknown_fields)?;
        }
        return Ok(stories);
    }
    Err(InputIssue::unsupported(format!(
        "'{format}' is not a peripheral JSON document format"
    )))
}

/// 将权威 Storyline 编码为一个外围 JSON 文档。
pub fn encode_json_storylines(
    format: DocumentFormat,
    stories: &[StorylineDocument],
) -> Result<serde_json::Value> {
    encode_json_storylines_with_options(format, stories, DocumentCodecOptions::default())
}

pub fn encode_json_storylines_with_options(
    format: DocumentFormat,
    stories: &[StorylineDocument],
    options: DocumentCodecOptions,
) -> Result<serde_json::Value> {
    options.unknown_fields.validate()?;
    for story in stories {
        story.validate().map_err(anyhow::Error::from)?;
        validate_unknown_fields(&story.unknown_fields, options.unknown_fields)?;
    }
    if let Some(handler) = crate::formats::registry::get(format) {
        if !handler.capabilities().encode {
            anyhow::bail!("'{format}' is decode-only and cannot be encoded");
        }
        let mut encoded = Vec::new();
        handler
            .encode(stories, &mut encoded)
            .map_err(anyhow::Error::from)?;
        return Ok(serde_json::from_slice(&encoded)?);
    }
    anyhow::bail!("'{format}' is not a peripheral JSON document format")
}

/// 文档源声明的 filter pushdown 保证。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(feature = "lance-store")]
pub enum FilterPushdown {
    Unsupported,
    Inexact,
    Exact,
    /// 保证级别取决于具体表达式和涉及的列。
    ExpressionDependent,
}

/// 文档源注册的逻辑表族。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(feature = "lance-store")]
pub enum QueryTables {
    Events,
    Storyline,
}

/// 已打开 provider 的真实优化能力；调用方不应按格式名称自行推断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(feature = "lance-store")]
pub struct QueryCapabilities {
    pub projection_pushdown: bool,
    pub filter_pushdown: FilterPushdown,
    pub limit_pushdown: bool,
    pub scalar_indexes: bool,
    pub streaming_decode: bool,
    pub late_content_materialization: bool,
    pub snapshot_consistent: bool,
}

/// 便捷物化 API 最多保留的 Storyline 行数。
#[cfg(feature = "lance-store")]
pub const DEFAULT_DOCUMENT_MATERIALIZE_ROWS: usize = 10_000;
/// 便捷物化 API 最多保留的 Storyline 序列化字节数。
#[cfg(feature = "lance-store")]
pub const DEFAULT_DOCUMENT_MATERIALIZE_BYTES: usize = 64 * 1024 * 1024;

/// 一个已打开的物理文档源；具体 provider variant 保持私有。
#[derive(Debug)]
#[cfg(feature = "lance-store")]
pub struct DocumentSource {
    pub(crate) inner: crate::store::DocumentSourceImpl,
}

/// 打开六种 pChronicle 物理文档格式之一。
#[cfg(feature = "lance-store")]
pub async fn open_document(format: DocumentFormat, path: &Path) -> anyhow::Result<DocumentSource> {
    Ok(DocumentSource {
        inner: crate::store::open_document_source(format, path).await?,
    })
}

#[cfg(feature = "lance-store")]
impl DocumentSource {
    pub fn format(&self) -> DocumentFormat {
        self.inner.format()
    }

    pub fn capabilities(&self) -> QueryCapabilities {
        self.inner.capabilities()
    }

    /// 物化全部 Storyline；累计行数或字节数超出预算时 fail closed。
    pub async fn project_storylines(&self) -> anyhow::Result<Vec<StorylineDocument>> {
        self.inner.project_storylines().await
    }

    /// 逐条访问 Storyline，不在内存中保留完整数据源。
    pub async fn for_each_storyline<F>(&self, on_storyline: F) -> anyhow::Result<()>
    where
        F: FnMut(StorylineDocument) -> anyhow::Result<()>,
    {
        self.inner.for_each_storyline(on_storyline).await
    }

    pub fn register_datafusion(&self, context: &SessionContext) -> anyhow::Result<QueryTables> {
        self.inner.register_datafusion(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::unknown_fields::UnknownFieldLimits;

    fn atif_fixture_value() -> serde_json::Value {
        serde_json::json!({
            "schema_version": "ATIF-v1.7",
            "trajectory_id": "one",
            "agent": {"name": "agent", "version": "1"},
            "steps": []
        })
    }

    #[test]
    fn storyline_json_codec_roundtrips_the_strict_wire() {
        let mut story = StorylineDocument::new("storyline-session", "storyline-agent");
        story.turns.push(crate::StorylineTurn {
            id: 7,
            kind: None,
            timestamp: Some(
                crate::model::StorylineTimestamp::from_rfc3339("2026-08-20T12:00:00Z").unwrap(),
            ),
            source: "user".into(),
            message: serde_json::json!("hello"),
            reasoning_content: None,
            reasoning_effort: None,
            tool_calls: Some(Vec::new()),
            observation: Some(serde_json::json!({"results": [], "vendor": true})),
            metrics: None,
            model_name: None,
            llm_call_count: None,
            is_copied_context: None,
            latency_ms: None,
            ttft_ms: None,
            extra: None,
            env: None,
            prompt: None,
            finished_at: None,
        });

        let encoded =
            encode_json_storylines(DocumentFormat::Storyline, std::slice::from_ref(&story))
                .unwrap();
        let decoded = decode_json_storylines(
            DocumentFormat::Storyline,
            &encoded.to_string(),
            "trajectory.storyline.json",
        )
        .unwrap();

        assert_eq!(decoded, vec![story]);
    }

    #[test]
    fn atif_singleton_object_and_array_encode_canonically() {
        let object = atif_fixture_value();
        let from_object =
            decode_json_storylines(DocumentFormat::Atif, &object.to_string(), "a.json").unwrap();
        let from_array = decode_json_storylines(
            DocumentFormat::Atif,
            &serde_json::json!([object]).to_string(),
            "a.json",
        )
        .unwrap();
        assert_eq!(
            encode_json_storylines(DocumentFormat::Atif, &from_object).unwrap(),
            encode_json_storylines(DocumentFormat::Atif, &from_array).unwrap(),
        );
    }

    #[test]
    fn atif_jsonl_and_ndjson_decode_each_non_empty_record() {
        let first = atif_fixture_value();
        let mut second = atif_fixture_value();
        second["trajectory_id"] = serde_json::json!("two");
        let input = format!("{}\n\n{}\n", first, second);

        for relative_path in ["records.jsonl", "records.ndjson"] {
            let stories =
                decode_json_storylines(DocumentFormat::Atif, &input, relative_path).unwrap();
            assert_eq!(stories.len(), 2, "path={relative_path}");
            assert_eq!(stories[0].document_id(), "one");
            assert_eq!(stories[1].document_id(), "two");
        }
    }

    #[test]
    fn atif_json_lines_reports_the_failing_record_location() {
        let input = format!("{}\n{{\"not\":\"atif\"}}\n", atif_fixture_value());
        let error =
            decode_json_storylines(DocumentFormat::Atif, &input, "records.jsonl").unwrap_err();

        assert_eq!(error.location(), Some("line 2"));
    }

    #[test]
    fn atif_unknown_fields_round_trip_canonically() {
        let input = serde_json::json!({
            "schema_version": "ATIF-v1.7",
            "session_id": null,
            "trajectory_id": "t1",
            "vendor_root": null,
            "agent": {"name": "a", "version": "1", "vendor_agent": {"x": 1}},
            "steps": [{
                "step_id": 1, "source": "user", "message": "hi",
                "vendor_step": [1, 2], "0": "numeric-object-key"
            }]
        });
        let stories =
            decode_json_storylines(DocumentFormat::Atif, &input.to_string(), "t.json").unwrap();
        assert_eq!(
            stories[0].unknown_fields.sources["atif"].fields["/vendor_root"],
            serde_json::Value::Null
        );
        assert_eq!(
            stories[0].unknown_key_counts["atif"]["/steps/*/vendor_step"],
            1
        );
        assert_eq!(stories[0].unknown_key_counts["atif"]["/steps/*/0"], 1);
        let output = encode_json_storylines(DocumentFormat::Atif, &stories).unwrap();
        assert_eq!(output["vendor_root"], serde_json::Value::Null);
        assert!(output.get("session_id").is_some());
    }

    #[test]
    fn empty_openai_envelope_fails_closed() {
        let input = serde_json::json!({"custom": null, "session_steps": []});
        assert!(
            decode_json_storylines(DocumentFormat::OpenaiMsg, &input.to_string(), "empty.json")
                .is_err()
        );
    }

    #[test]
    fn openai_envelope_rejects_target_source_unknown_fields() {
        let input = serde_json::json!({
            "session_steps": [{
                "session_id": "s-1",
                "step_id": 1,
                "messages": [{"role": "user", "content": "inspect"}],
                "response": {"role": "assistant", "content": "done"}
            }],
            "_storyline": {"unknown_fields": {
                "version": 1,
                "by_trajectory": {"/session_steps/0": {"sources": {
                    "openai-msg": {
                        "source_document_id": "same-target.json",
                        "fields": {"/session_steps/0/enveloped_vendor": true}
                    }
                }}}
            }}
        });

        let error = decode_json_storylines(
            DocumentFormat::OpenaiMsg,
            &input.to_string(),
            "same-target.json",
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("target source 'openai-msg'"),
            "{error:#}"
        );
    }

    #[test]
    fn unknown_fields_options_are_validated_before_json_decode() {
        let options = DocumentCodecOptions {
            unknown_fields: UnknownFieldLimits {
                max_fields: 0,
                max_bytes: 1,
            },
        };
        let decode_error = decode_json_storylines_with_options(
            DocumentFormat::Atif,
            "not JSON",
            "invalid.json",
            options,
        )
        .unwrap_err();
        assert!(decode_error.to_string().contains("unknown field limit"));

        let encode_error =
            encode_json_storylines_with_options(DocumentFormat::Atif, &[], options).unwrap_err();
        assert!(encode_error.to_string().contains("unknown field limit"));
    }

    #[test]
    fn decode_only_session_formats_cannot_be_encoded() {
        let stories = decode_json_storylines(DocumentFormat::Codex, "", "empty.jsonl").unwrap();
        for format in [DocumentFormat::Codex, DocumentFormat::ClaudeCode] {
            let error = encode_json_storylines(format, &stories).unwrap_err();
            assert!(
                error.to_string().contains("decode-only"),
                "{format}: {error:#}"
            );
        }
    }
}
