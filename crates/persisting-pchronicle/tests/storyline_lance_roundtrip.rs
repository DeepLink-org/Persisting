use anyhow::Result;
use persisting_pchronicle::document::{
    DocumentFormat, decode_json_storylines, encode_json_storylines, open_document,
};
use persisting_pchronicle::model::StorylineDocument;
use persisting_pchronicle::storage::StorylineLanceStore;

mod support;

use support::{LookupStrategy, fixture_path, persist_and_restore};

#[tokio::test]
async fn storyline_lance_preserves_order_presence_origin_and_raw_observation() -> Result<()> {
    let expected = serde_json::json!({
        "schema_version": "storyline/v1",
        "origin": {
            "format": "atif",
            "schema_version": "ATIF-v1.7",
            "document_id": "input/trajectory.json"
        },
        "trajectory": "order-presence-observation",
        "session": "shared-session",
        "agent": {"id": "agent", "name": "agent"},
        "turns": [
            {
                "id": 9,
                "src": "user",
                "msg": "first",
                "tool_calls": []
            },
            {
                "id": 3,
                "src": "agent",
                "msg": "second",
                "tool_calls": [
                    {"tcid": "call-b", "fn": "second", "args": {"n": 2}},
                    {"tcid": "call-a", "fn": "first", "args": {"n": 1}}
                ],
                "observation": {
                    "vendor": {"trace": 7},
                    "results": [
                        {"source_call_id": "call-b", "content": "b-1"},
                        {"source_call_id": "call-a", "content": "a-1"},
                        {"source_call_id": "call-b", "content": "b-2"}
                    ]
                }
            }
        ]
    });
    let stories = decode_json_storylines(
        DocumentFormat::Storyline,
        &expected.to_string(),
        "trajectory.storyline.json",
    )?;

    let restored = persist_and_restore(&stories, LookupStrategy::DocumentIds).await?;

    assert_eq!(
        encode_json_storylines(DocumentFormat::Storyline, &restored)?,
        expected
    );
    Ok(())
}

#[tokio::test]
async fn nested_atif_and_null_canonicalization_are_stable_through_storyline_lance() -> Result<()> {
    let expected = serde_json::json!({
        "schema_version": "ATIF-v1.7",
        "session_id": "shared-run",
        "trajectory_id": "root",
        "agent": {"name": "root", "version": "1", "model_name": null},
        "steps": [{
            "step_id": 1,
            "timestamp": "2026-08-14T12:34:56.789123+08:00",
            "source": "agent",
            "message": "root",
            "llm_call_count": null
        }],
        "subagent_trajectories": [{
            "schema_version": "ATIF-v1.7",
            "trajectory_id": "child",
            "agent": {"name": "child", "version": "1"},
            "steps": [],
            "notes": null
        }]
    });
    let stories =
        decode_json_storylines(DocumentFormat::Atif, &expected.to_string(), "nested.json")?;
    let expected = encode_json_storylines(DocumentFormat::Atif, &stories)?;
    let restored = persist_and_restore(&stories, LookupStrategy::DocumentIds).await?;
    assert_eq!(
        encode_json_storylines(DocumentFormat::Atif, &restored)?,
        expected
    );
    Ok(())
}

#[tokio::test]
async fn atif_parallel_tools_fixture_is_lossless_through_storyline_lance() -> Result<()> {
    let atif_path = fixture_path("atif/parallel_tools_14.json");
    let atif_raw = std::fs::read_to_string(&atif_path)?;
    let atif_stories = decode_json_storylines(DocumentFormat::Atif, &atif_raw, &atif_path)?;
    let atif_expected = encode_json_storylines(DocumentFormat::Atif, &atif_stories)?;
    let atif_restored = persist_and_restore(&atif_stories, LookupStrategy::DocumentIds).await?;
    assert_eq!(
        encode_json_storylines(DocumentFormat::Atif, &atif_restored)?,
        atif_expected
    );

    Ok(())
}

#[tokio::test]
async fn atif_root_order_is_lossless_through_the_unified_storyline_source() -> Result<()> {
    let expected = serde_json::json!([
        {
            "schema_version": "ATIF-v1.7",
            "trajectory_id": "z-document",
            "session_id": "shared",
            "agent": {"name": "z", "version": "1"},
            "steps": []
        },
        {
            "schema_version": "ATIF-v1.7",
            "trajectory_id": "a-document",
            "session_id": "shared",
            "agent": {"name": "a", "version": "1"},
            "steps": []
        }
    ]);
    let decoded =
        decode_json_storylines(DocumentFormat::Atif, &expected.to_string(), "ordered.json")?;
    let temporary = tempfile::tempdir()?;
    let store = StorylineLanceStore::open(temporary.path()).await?;
    store.replace_storylines(&decoded).await?;

    let restored = open_document(DocumentFormat::StorylineLance, temporary.path())
        .await?
        .project_storylines()
        .await?;
    assert_eq!(
        encode_json_storylines(DocumentFormat::Atif, &restored)?,
        expected
    );
    Ok(())
}

#[tokio::test]
async fn atif_singleton_array_shape_is_lossless_through_storyline_lance() -> Result<()> {
    let expected = serde_json::json!([{
        "schema_version": "ATIF-v1.7",
        "trajectory_id": "only-document",
        "agent": {"name": "agent", "version": "1"},
        "steps": []
    }]);
    let temporary = tempfile::tempdir()?;
    let input = temporary.path().join("singleton.json");
    std::fs::write(&input, expected.to_string())?;
    let expected = encode_json_storylines(
        DocumentFormat::Atif,
        &decode_json_storylines(DocumentFormat::Atif, &expected.to_string(), &input)?,
    )?;
    let lance = temporary.path().join("storyline");
    let store = StorylineLanceStore::open(&lance).await?;
    store.import_atif_stream(&input).await?;

    let restored = open_document(DocumentFormat::StorylineLance, &lance)
        .await?
        .project_storylines()
        .await?;
    assert_eq!(
        encode_json_storylines(DocumentFormat::Atif, &restored)?,
        expected
    );
    Ok(())
}

#[tokio::test]
async fn incremental_storyline_replace_preserves_global_collection_order() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let store = StorylineLanceStore::open(temporary.path()).await?;
    let mut z = StorylineDocument::new("z-document", "agent");
    z.trajectory_id = Some("z-document".into());
    let mut a = StorylineDocument::new("a-document", "agent");
    a.trajectory_id = Some("a-document".into());
    store.replace_storyline(&z).await?;
    store.replace_storyline(&a).await?;

    let source = open_document(DocumentFormat::StorylineLance, temporary.path()).await?;
    let first = source.project_storylines().await?;
    assert_eq!(
        first
            .iter()
            .map(StorylineDocument::document_id)
            .collect::<Vec<_>>(),
        ["z-document", "a-document"]
    );

    z.notes = Some("updated".into());
    store.replace_storyline(&z).await?;
    let second = open_document(DocumentFormat::StorylineLance, temporary.path())
        .await?
        .project_storylines()
        .await?;
    assert_eq!(
        second
            .iter()
            .map(StorylineDocument::document_id)
            .collect::<Vec<_>>(),
        ["z-document", "a-document"]
    );
    assert_eq!(second[0].notes.as_deref(), Some("updated"));
    Ok(())
}
