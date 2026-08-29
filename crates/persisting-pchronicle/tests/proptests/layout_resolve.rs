use persisting_pchronicle::storage::{merge_story_location, resolve_story_read_location};
use proptest::prelude::*;

fn component_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[A-Za-z0-9._-]{1,32}").unwrap()
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_explicit_location_flags_always_override_inference(
        path in component_strategy(),
        agent in component_strategy(),
        session in component_strategy(),
        root in prop::option::of(component_strategy()),
    ) {
        let path = format!("__pchronicle_missing_{path}");
        let merged = merge_story_location(
            path.clone(),
            Some(agent.clone()),
            Some(session.clone()),
            root.clone(),
        );
        prop_assert_eq!(merged.storage, path);
        prop_assert_eq!(merged.agent_id, Some(agent));
        prop_assert_eq!(merged.session_id, Some(session));
        prop_assert_eq!(merged.root_session_id, root);
    }

    #[test]
    fn public_resolving_explicit_ids_is_stable_for_arbitrary_storage_names(
        storage in component_strategy(),
        agent in component_strategy(),
        session in component_strategy(),
    ) {
        let location = resolve_story_read_location(
            "trajectory stats",
            storage.clone(),
            Some(agent.clone()),
            Some(session.clone()),
            None,
        ).expect("explicit ids resolve without touching storage");
        prop_assert_eq!(location.storage, storage);
        prop_assert_eq!(location.agent_id, agent);
        prop_assert_eq!(location.session_id, session);
    }

    #[test]
    fn public_partial_location_merge_is_idempotent_without_inference(
        path in component_strategy(),
        agent in prop::option::of(component_strategy()),
        session in prop::option::of(component_strategy()),
        root in prop::option::of(component_strategy()),
    ) {
        let path = format!("__pchronicle_missing_{path}");
        let merged = merge_story_location(path.clone(), agent.clone(), session.clone(), root.clone());
        let again = merge_story_location(
            merged.storage.clone(),
            merged.agent_id.clone(),
            merged.session_id.clone(),
            merged.root_session_id.clone(),
        );
        prop_assert_eq!(again, merged);
    }
}
