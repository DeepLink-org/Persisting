use persisting_pchronicle::storage::{
    CatalogDataset, CatalogEventProvenance, CatalogSourceKind, CatalogSourceRevision,
    CatalogSourceStatus, DatasetMount, DiscoveredSource,
};
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

    #[test]
    fn public_catalog_dataset_counts_ready_and_error_sources_exactly(
        ready in proptest::collection::vec(any::<bool>(), 0..32),
    ) {
        let sources = ready
            .iter()
            .map(|is_ready| DiscoveredSource {
                file: "source.json".into(),
                format: None,
                kind: CatalogSourceKind::File,
                revision: None,
                projection_status: None,
                projection_generation: None,
                projection_candidates: 0,
                size_bytes: None,
                last_modified: None,
                status: if *is_ready {
                    CatalogSourceStatus::Ready
                } else {
                    CatalogSourceStatus::Error
                },
                error: None,
            })
            .collect();
        let dataset = CatalogDataset {
            mount: DatasetMount::default("memory://catalog/source").unwrap(),
            sources,
        };
        prop_assert_eq!(
            dataset.ready_source_count(),
            ready.iter().filter(|is_ready| **is_ready).count(),
        );
        prop_assert_eq!(
            dataset.error_source_count(),
            ready.iter().filter(|is_ready| !**is_ready).count(),
        );
    }

    #[test]
    fn public_catalog_source_revisions_expose_stable_snapshot_refs(
        generation in "gen-[A-Za-z0-9_-]{1,16}",
        fingerprint in "fp-[A-Za-z0-9_-]{1,16}",
        layout_revision in any::<u64>(),
    ) {
        let revisions = vec![
            (CatalogSourceRevision::Storyline { generation: generation.clone() }, generation),
            (CatalogSourceRevision::LocalFile { fingerprint: fingerprint.clone() }, fingerprint),
            (
                CatalogSourceRevision::Events { fact_version: 1, fact_rows: 2, layout_revision },
                format!("manifest-revision:{layout_revision}"),
            ),
        ];
        for (revision, expected) in revisions {
            prop_assert_eq!(revision.snapshot_ref(), expected);
        }
    }
}
