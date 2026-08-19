//! pChronicle 六种物理文档格式的识别、转换与统一读取入口。

use std::collections::{HashMap, HashSet};
use std::path::Path;

#[cfg(feature = "lance-store")]
use datafusion::prelude::SessionContext;

pub use crate::agenticmd::{
    agenticmd_block_count, agenticmd_structural_issues, count_agenticmd_role, encode_agenticmd,
    index_agenticmd_path, list_agenticmd_paths, rewrite_agenticmd_storyline_metadata,
    upsert_agenticmd_turn, write_agenticmd_storyline, AgenticmdFileIndex,
};
pub use crate::convert::{events_to_storyline, project_event_records, storyline_to_events};
pub use crate::format::DocumentFormat;
pub use crate::formats::detect_format;
pub use crate::input::{InputIssue, InputIssueKind, InputResult};
pub use crate::interop::{events_to_har, events_to_otlp_json, otlp_json_to_events};

pub type Result<T> = anyhow::Result<T>;

use crate::atif::AtifTrajectory;
use crate::convert::{atif_collection_to_storylines, storylines_to_actf, storylines_to_atif};
use crate::formats::actf::ActfDocument;
use crate::formats::{
    has_openai_provenance, parse_openai_msg_corpus_value, recover_openai_msg_files,
    synthesize_openai_msg_corpus, StorylineCollectionShape, StorylineDocument,
};

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
    match format {
        DocumentFormat::Atif => {
            let value: serde_json::Value = serde_json::from_str(input)
                .map_err(|error| InputIssue::invalid(error.to_string()))?;
            let (values, array) = match value {
                serde_json::Value::Array(values) => (values, true),
                value => (vec![value], false),
            };
            if values.is_empty() {
                return Err(InputIssue::unsupported("ATIF document cannot be empty"));
            }
            let mut stories = Vec::new();
            for (ordinal, value) in values.into_iter().enumerate() {
                let trajectory = AtifTrajectory::from_json_str(&value.to_string())?;
                let ordinal = i64::try_from(ordinal)
                    .map_err(|_| InputIssue::invalid("ATIF collection ordinal overflow"))?;
                let shape = if array {
                    StorylineCollectionShape::Sequence
                } else {
                    StorylineCollectionShape::Single
                };
                stories.extend(
                    atif_collection_to_storylines(&trajectory, shape, ordinal)
                        .map_err(|error| InputIssue::invalid(error.to_string()))?,
                );
            }
            Ok(stories)
        }
        DocumentFormat::Actf => {
            let document = ActfDocument::from_json_str(input)?;
            crate::convert::actf_to_storylines(&document)
                .map_err(|error| InputIssue::invalid(error.to_string()))
        }
        DocumentFormat::OpenaiMsg => {
            let value = serde_json::from_str(input)
                .map_err(|error| InputIssue::invalid(error.to_string()))?;
            parse_openai_msg_corpus_value(&value, relative_path)
        }
        unsupported => Err(InputIssue::unsupported(format!(
            "'{unsupported}' is not a peripheral JSON document format"
        ))),
    }
}

/// 将权威 Storyline 编码为一个外围 JSON 文档。
pub fn encode_json_storylines(
    format: DocumentFormat,
    stories: &[StorylineDocument],
) -> Result<serde_json::Value> {
    match format {
        DocumentFormat::Atif => {
            let (stories, collection_shape) = prepare_atif_collection(stories)?;
            let documents = storylines_to_atif(&stories)?;
            if collection_shape == Some(StorylineCollectionShape::Sequence) {
                Ok(serde_json::to_value(documents)?)
            } else if documents.len() == 1 {
                Ok(serde_json::to_value(&documents[0])?)
            } else {
                Ok(serde_json::to_value(documents)?)
            }
        }
        DocumentFormat::Actf => Ok(serde_json::to_value(storylines_to_actf(stories)?)?),
        DocumentFormat::OpenaiMsg => encode_openai_storylines(stories),
        unsupported => anyhow::bail!("'{unsupported}' is not a peripheral JSON document format"),
    }
}

fn prepare_atif_collection(
    stories: &[StorylineDocument],
) -> Result<(Vec<StorylineDocument>, Option<StorylineCollectionShape>)> {
    let mut shape = None;
    let mut missing_shape = false;
    let mut has_ordinal = false;
    let mut missing_ordinal = false;
    for story in stories {
        story.validate()?;
        match story.presence.collection_shape {
            Some(value) if shape.is_some_and(|current| current != value) => {
                anyhow::bail!("Storyline collection contains conflicting container shapes");
            }
            Some(value) => shape = Some(value),
            None => missing_shape = true,
        }
        if story.presence.collection_ordinal.is_some() {
            has_ordinal = true;
        } else {
            missing_ordinal = true;
        }
    }
    if shape.is_some() && missing_shape {
        anyhow::bail!("Storyline collection mixes declared and undeclared container shapes");
    }
    if has_ordinal && missing_ordinal {
        anyhow::bail!("Storyline collection mixes declared and undeclared ordinals");
    }

    let mut ordered = stories.to_vec();
    if has_ordinal {
        ordered.sort_by_key(|story| story.presence.collection_ordinal);
    }

    let by_id = ordered
        .iter()
        .map(|story| (story.document_id().to_string(), story))
        .collect::<HashMap<_, _>>();
    if by_id.len() != ordered.len() {
        anyhow::bail!("Storyline collection contains duplicate document identities");
    }
    let referenced = ordered
        .iter()
        .flat_map(|story| story.child_session_ids.iter().flatten().cloned())
        .collect::<HashSet<_>>();
    let roots = ordered
        .iter()
        .filter(|story| !referenced.contains(story.document_id()))
        .collect::<Vec<_>>();
    let mut root_ordinals = HashSet::new();
    for root in &roots {
        if let Some(ordinal) = root.presence.collection_ordinal {
            if !root_ordinals.insert(ordinal) {
                anyhow::bail!("duplicate Storyline root collection ordinal {ordinal}");
            }
        }
    }
    for story in &ordered {
        if let Some(children) = &story.child_session_ids {
            for child_id in children {
                let child = by_id.get(child_id).ok_or_else(|| {
                    anyhow::anyhow!("Storyline child '{child_id}' has no matching document")
                })?;
                if child.presence.collection_ordinal != story.presence.collection_ordinal {
                    anyhow::bail!(
                        "Storyline child '{child_id}' has a different collection ordinal"
                    );
                }
            }
        }
    }
    if shape == Some(StorylineCollectionShape::Single)
        && (roots.len() != 1 || root_ordinals.iter().any(|ordinal| *ordinal != 0))
    {
        anyhow::bail!(
            "single-document Storyline collection must have exactly one root at ordinal zero"
        );
    }
    Ok((ordered, shape))
}

fn encode_openai_storylines(stories: &[StorylineDocument]) -> Result<serde_json::Value> {
    if stories.iter().any(has_openai_provenance) {
        let mut files = recover_openai_msg_files(stories)?;
        if files.len() != 1 {
            anyhow::bail!(
                "one JSON document cannot preserve {} OpenAI source files",
                files.len()
            );
        }
        Ok(files.remove(0).document)
    } else {
        synthesize_openai_msg_corpus(stories)
    }
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

    #[test]
    fn atif_codec_preserves_singleton_array_shape_in_storyline() {
        let input = serde_json::json!([{
            "schema_version": "ATIF-v1.7",
            "trajectory_id": "one",
            "agent": {"name": "agent", "version": "1"},
            "steps": []
        }]);
        let stories =
            decode_json_storylines(DocumentFormat::Atif, &input.to_string(), "one.json").unwrap();
        assert_eq!(
            encode_json_storylines(DocumentFormat::Atif, &stories).unwrap(),
            input
        );
    }

    #[test]
    fn empty_openai_envelope_fails_closed() {
        let input = serde_json::json!({"custom": null, "session_steps": []});
        assert!(decode_json_storylines(
            DocumentFormat::OpenaiMsg,
            &input.to_string(),
            "empty.json"
        )
        .is_err());
    }

    #[test]
    fn atif_encoder_rejects_conflicting_collection_metadata() {
        let mut first = StorylineDocument::new("first", "agent");
        first.presence.collection_shape = Some(StorylineCollectionShape::Sequence);
        first.presence.collection_ordinal = Some(0);
        let mut second = StorylineDocument::new("second", "agent");
        second.presence.collection_shape = Some(StorylineCollectionShape::Single);
        second.presence.collection_ordinal = Some(0);
        assert!(encode_json_storylines(DocumentFormat::Atif, &[first, second]).is_err());

        let mut first = StorylineDocument::new("first", "agent");
        first.presence.collection_shape = Some(StorylineCollectionShape::Sequence);
        first.presence.collection_ordinal = Some(0);
        let mut second = StorylineDocument::new("second", "agent");
        second.presence.collection_shape = Some(StorylineCollectionShape::Sequence);
        second.presence.collection_ordinal = Some(0);
        assert!(encode_json_storylines(DocumentFormat::Atif, &[first, second]).is_err());
    }
}
