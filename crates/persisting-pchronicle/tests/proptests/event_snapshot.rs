use persisting_pchronicle::storage::EventFactSnapshot;
use proptest::prelude::*;

fn source_uri_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z0-9:/._-]{1,64}").unwrap()
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_event_fact_snapshots_roundtrip_through_json(
        source_uri in source_uri_strategy(),
        fact_version in any::<u64>(),
        fact_rows in any::<u64>(),
        layout_revision in any::<u64>(),
    ) {
        let snapshot = EventFactSnapshot {
            source_uri,
            fact_version,
            fact_rows,
            layout_revision,
        };
        let encoded = serde_json::to_string(&snapshot).unwrap();
        prop_assert_eq!(serde_json::from_str::<EventFactSnapshot>(&encoded).unwrap(), snapshot);
    }

    #[test]
    fn public_event_fact_snapshots_use_stable_wire_field_names(
        source_uri in source_uri_strategy(),
        fact_version in any::<u64>(),
        fact_rows in any::<u64>(),
        layout_revision in any::<u64>(),
    ) {
        let snapshot = EventFactSnapshot {
            source_uri,
            fact_version,
            fact_rows,
            layout_revision,
        };
        let value = serde_json::to_value(snapshot).unwrap();
        let object = value.as_object().unwrap();
        prop_assert_eq!(object.len(), 4);
        for field in ["source_uri", "fact_version", "fact_rows", "layout_revision"] {
            prop_assert!(object.contains_key(field), "missing field {field}");
        }
    }

    #[test]
    fn public_event_fact_snapshot_json_is_deterministic(
        source_uri in source_uri_strategy(),
        fact_version in any::<u64>(),
        fact_rows in any::<u64>(),
        layout_revision in any::<u64>(),
    ) {
        let snapshot = EventFactSnapshot {
            source_uri: source_uri.clone(),
            fact_version,
            fact_rows,
            layout_revision,
        };
        let expected = format!(
            "{{\"source_uri\":{},\"fact_version\":{},\"fact_rows\":{},\"layout_revision\":{}}}",
            serde_json::to_string(&source_uri).unwrap(),
            fact_version,
            fact_rows,
            layout_revision,
        );
        prop_assert_eq!(serde_json::to_string(&snapshot).unwrap(), expected);
    }
}
