use persisting_pchronicle::storage::{ProjectionSourceSnapshot, StorylineProjectionLineage};
use proptest::prelude::*;

fn token_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[A-Za-z0-9._:/-]{1,48}").unwrap()
}

fn source_snapshot_strategy() -> impl Strategy<Value = ProjectionSourceSnapshot> {
    prop_oneof![
        (token_strategy(), any::<u64>(), any::<u64>(), any::<u64>(),).prop_map(
            |(source_uri, fact_version, fact_rows, layout_revision)| {
                ProjectionSourceSnapshot::CanonicalEvents {
                    source_uri,
                    fact_version,
                    fact_rows,
                    layout_revision,
                }
            }
        ),
        (
            token_strategy(),
            token_strategy(),
            prop::option::of(token_strategy()),
        )
            .prop_map(|(source_uri, snapshot_ref, content_digest)| {
                ProjectionSourceSnapshot::Exchange {
                    source_uri,
                    snapshot_ref,
                    content_digest,
                }
            }),
    ]
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_projection_lineage_roundtrips_through_json(
        source_id in token_strategy(),
        source_file in token_strategy(),
        source in source_snapshot_strategy(),
        projector_name in token_strategy(),
        recipe_hash in token_strategy(),
        completeness in token_strategy(),
    ) {
        let lineage = StorylineProjectionLineage {
            source_id,
            source_file,
            source,
            projector_name,
            recipe_hash,
            completeness,
        };
        let encoded = serde_json::to_string(&lineage).unwrap();
        prop_assert_eq!(serde_json::from_str::<StorylineProjectionLineage>(&encoded).unwrap(), lineage);
    }

    #[test]
    fn public_projection_source_snapshot_uses_stable_kind_tags(
        source in source_snapshot_strategy(),
    ) {
        let encoded = serde_json::to_value(source).unwrap();
        let kind = encoded.get("kind").and_then(serde_json::Value::as_str);
        prop_assert!(matches!(kind, Some("canonical_events") | Some("exchange")));
    }
}
