use persisting_pchronicle::storage::{DatasetLocation, DatasetLocationKind};
use proptest::prelude::*;

fn path_segment_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z0-9-]{1,20}").unwrap()
}

fn bucket_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z0-9][a-z0-9-]{1,18}[a-z0-9]").unwrap()
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_object_store_locations_normalize_trailing_slashes(
        bucket in bucket_strategy(),
        prefix in proptest::collection::vec(path_segment_strategy(), 0..4),
        slash_count in 1usize..8,
    ) {
        let path = prefix.join("/");
        let suffix = if path.is_empty() { String::new() } else { format!("/{path}") };
        let input = format!("s3://{bucket}{suffix}{}", "/".repeat(slash_count));
        let location = DatasetLocation::parse(&input).expect("generated S3 URI is valid");
        prop_assert_eq!(location.kind(), DatasetLocationKind::ObjectStore);
        prop_assert!(location.is_object_store());
        prop_assert!(!location.as_str().ends_with('/'));
    }

    #[test]
    fn public_dataset_uri_query_and_fragment_inputs_fail_closed(
        bucket in bucket_strategy(),
        token in proptest::string::string_regex("[a-zA-Z0-9_-]{1,20}").unwrap(),
        fragment in any::<bool>(),
    ) {
        let input = if fragment {
            format!("s3://{bucket}/dataset#{token}")
        } else {
            format!("s3://{bucket}/dataset?token={token}")
        };
        let error = DatasetLocation::parse(&input)
            .expect_err("unsafe URI must be rejected")
            .to_string();
        prop_assert!(error.contains("query string") || error.contains("fragment"), "{error}");
    }

    #[test]
    fn public_generated_lowercase_dns_buckets_are_accepted(bucket in bucket_strategy()) {
        let location = DatasetLocation::parse(&format!("s3://{bucket}/dataset")).unwrap();
        prop_assert!(location.is_object_store());
    }

    #[test]
    fn public_location_normalization_is_idempotent(
        segments in proptest::collection::vec(path_segment_strategy(), 1..4),
        slash_count in 0usize..8,
    ) {
        let raw = format!("dataset/{}{}", segments.join("/"), "/".repeat(slash_count));
        let once = DatasetLocation::parse(&raw).unwrap();
        let twice = DatasetLocation::parse(once.as_str()).unwrap();
        prop_assert_eq!(twice.as_str(), once.as_str());
        prop_assert_eq!(twice.kind(), once.kind());
    }

    #[test]
    fn public_local_locations_preserve_their_path_without_becoming_object_store_uris(
        segments in proptest::collection::vec(path_segment_strategy(), 1..5),
    ) {
        let raw = segments.join("/");
        let location = DatasetLocation::parse(&raw).expect("generated local path is valid");
        prop_assert_eq!(location.kind(), DatasetLocationKind::Local);
        prop_assert!(!location.is_object_store());
        prop_assert_eq!(location.local_path(), Some(std::path::Path::new(&raw)));
        prop_assert_eq!(location.as_str(), raw);
    }
}
