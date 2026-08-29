use persisting_pchronicle::storage::{
    CatalogSnapshotOptions, DatasetCatalogSnapshot, DatasetMount, NamespacePath,
};
use proptest::prelude::*;

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_namespace_listing_paginates_without_dropping_children(
        suffixes in proptest::collection::vec(
            proptest::string::string_regex("[A-Za-z0-9_-]{1,12}").unwrap(),
            1..6,
        ),
        page_limit in 1usize..4,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let mounts = suffixes
            .iter()
            .enumerate()
            .map(|(index, suffix)| {
                let namespace = NamespacePath::single(format!("ns-{index}-{suffix}"))?;
                DatasetMount::namespaced(
                    namespace,
                    format!("dataset_{index}"),
                    temp.path().to_string_lossy(),
                )
            })
            .collect::<anyhow::Result<Vec<_>>>()
            .unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let listed = runtime.block_on(async {
            let snapshot = DatasetCatalogSnapshot::discover(
                mounts,
                None,
                CatalogSnapshotOptions::default(),
            )
            .await?;
            let mut token = None;
            let mut names = Vec::new();
            loop {
                let page = snapshot.list_namespaces(None, token.as_deref(), Some(page_limit))?;
                names.extend(
                    page.items
                        .into_iter()
                        .map(|item| item.path.display_name()),
                );
                token = page.next_page_token;
                if token.is_none() {
                    break;
                }
            }
            anyhow::Result::<Vec<_>>::Ok(names)
        }).unwrap();

        prop_assert_eq!(listed.len(), suffixes.len());
        prop_assert!(listed.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
