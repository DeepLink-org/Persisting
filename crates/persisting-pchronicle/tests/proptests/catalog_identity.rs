use persisting_pchronicle::storage::{CatalogSourceRevision, DatasetMount, NamespacePath};
use proptest::prelude::*;

fn namespace_component_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[A-Za-z0-9_.-]{1,20}").unwrap()
}

fn alias_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[A-Za-z_][A-Za-z0-9_]{0,24}")
        .unwrap()
        .prop_filter("reserved aliases are rejected", |alias| {
            alias != "public" && alias != "information_schema"
        })
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_namespace_paths_preserve_components_and_display_order(
        components in proptest::collection::vec(namespace_component_strategy(), 1..6),
    ) {
        let namespace = NamespacePath::new(components.clone()).expect("generated namespace is valid");
        prop_assert_eq!(namespace.components(), components.as_slice());
        prop_assert_eq!(namespace.display_name(), components.join("/"));
    }

    #[test]
    fn public_dataset_mount_keeps_namespace_separate_from_sql_alias(
        component in namespace_component_strategy(),
        alias in alias_strategy(),
    ) {
        let namespace = NamespacePath::single(component).unwrap();
        let mount = DatasetMount::namespaced(
            namespace.clone(),
            alias.clone(),
            "memory://catalog/source",
        ).unwrap();
        prop_assert_eq!(mount.namespace, namespace);
        prop_assert_eq!(mount.name, alias.to_ascii_lowercase());
        prop_assert_eq!(mount.uri, "memory://catalog/source");
    }

    #[test]
    fn public_source_revisions_always_expose_a_stable_snapshot_reference(
        value in proptest::string::string_regex("[A-Za-z0-9._:/-]{1,32}").unwrap(),
        revision_kind in 0u8..4,
    ) {
        let revision = match revision_kind {
            0 => CatalogSourceRevision::Storyline { generation: value.clone() },
            1 => CatalogSourceRevision::Events {
                fact_version: 1,
                fact_rows: 2,
                layout_revision: 3,
            },
            2 => CatalogSourceRevision::LocalFile { fingerprint: value.clone() },
            _ => CatalogSourceRevision::Object {
                version: Some(value.clone()),
                etag: None,
                size_bytes: 0,
                last_modified: "2026-01-01T00:00:00Z".into(),
                location: "memory://catalog/source".into(),
            },
        };
        let snapshot = revision.snapshot_ref();
        prop_assert!(!snapshot.is_empty());
        if revision_kind == 1 {
            prop_assert_eq!(snapshot, "manifest-revision:3");
        } else if revision_kind == 0 || revision_kind == 2 {
            prop_assert_eq!(snapshot, value);
        } else {
            prop_assert_eq!(snapshot, format!("version:{value}"));
        }
    }

    #[test]
    fn public_namespace_paths_reject_path_separators_and_empty_components(
        invalid in prop::sample::select(vec![
            String::new(),
            "a/b".into(),
            "a\\\\b".into(),
            "a b".into(),
            "a?b".into(),
        ]),
    ) {
        prop_assert!(NamespacePath::single(invalid).is_err());
    }
}
