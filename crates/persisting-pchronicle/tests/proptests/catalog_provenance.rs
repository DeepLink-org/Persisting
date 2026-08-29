use persisting_pchronicle::storage::CatalogEventProvenance;
use proptest::prelude::*;

fn provenance_strategy() -> impl Strategy<Value = CatalogEventProvenance> {
    prop_oneof![
        Just(CatalogEventProvenance::Canonical),
        Just(CatalogEventProvenance::SyntheticFromStoryline),
    ]
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_event_provenance_exposes_its_canonicality_and_transform(
        provenance in provenance_strategy(),
    ) {
        let expected_canonical = matches!(provenance, CatalogEventProvenance::Canonical);
        let expected_transform = if expected_canonical {
            None
        } else {
            Some("storyline_to_events_v1")
        };
        prop_assert_eq!(provenance.is_canonical(), expected_canonical);
        prop_assert_eq!(provenance.transform(), expected_transform);
    }

    #[test]
    fn public_event_provenance_serializes_to_stable_snake_case(
        provenance in provenance_strategy(),
    ) {
        let encoded = serde_json::to_value(provenance).unwrap();
        let expected = match provenance {
            CatalogEventProvenance::Canonical => "canonical",
            CatalogEventProvenance::SyntheticFromStoryline => "synthetic_from_storyline",
        };
        prop_assert_eq!(encoded.as_str(), Some(expected));
    }
}
