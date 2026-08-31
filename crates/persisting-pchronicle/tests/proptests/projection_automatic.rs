use persisting_pchronicle::storage::{
    AutomaticProjectionMaintenanceMode, AutomaticProjectionMaintenanceReport,
    AutomaticProjectionState, storyline_projection_destination_exists,
};
use proptest::prelude::*;

fn maintenance_mode_strategy() -> impl Strategy<Value = AutomaticProjectionMaintenanceMode> {
    prop_oneof![
        Just(AutomaticProjectionMaintenanceMode::Unchanged),
        Just(AutomaticProjectionMaintenanceMode::Built),
        Just(AutomaticProjectionMaintenanceMode::Incremental),
        Just(AutomaticProjectionMaintenanceMode::Rebuilt),
        Just(AutomaticProjectionMaintenanceMode::ConcurrentWinner),
    ]
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_maintenance_report_published_matches_its_mode(
        mode in maintenance_mode_strategy(),
        generation in proptest::string::string_regex("gen-[A-Za-z0-9_-]{1,16}").unwrap(),
        fact_version in any::<u64>(),
        fact_rows in any::<u64>(),
    ) {
        let report = AutomaticProjectionMaintenanceReport {
            mode,
            generation,
            fact_version,
            fact_rows,
            trajectories: None,
        };
        let expected = matches!(
            report.mode,
            AutomaticProjectionMaintenanceMode::Built
                | AutomaticProjectionMaintenanceMode::Incremental
                | AutomaticProjectionMaintenanceMode::Rebuilt
        );
        prop_assert_eq!(report.published(), expected);
    }

    #[test]
    fn public_destination_existence_is_observational(
        suffix in proptest::string::string_regex("[A-Za-z0-9_-]{1,24}").unwrap(),
    ) {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join(format!("storyline-{suffix}"));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let before = runtime
            .block_on(storyline_projection_destination_exists(destination.to_string_lossy()))
            .unwrap();
        prop_assert!(!before);
        prop_assert!(!destination.exists());

        std::fs::create_dir_all(&destination).unwrap();
        let after = runtime
            .block_on(storyline_projection_destination_exists(destination.to_string_lossy()))
            .unwrap();
        prop_assert!(after);
    }

    #[test]
    fn public_projection_states_use_stable_snake_case_wire_names(
        state in prop_oneof![
            Just(AutomaticProjectionState::Fresh),
            Just(AutomaticProjectionState::Stale),
            Just(AutomaticProjectionState::Missing),
        ],
    ) {
        let expected = match state {
            AutomaticProjectionState::Fresh => "fresh",
            AutomaticProjectionState::Stale => "stale",
            AutomaticProjectionState::Missing => "missing",
        };
        prop_assert_eq!(serde_json::to_value(state).unwrap(), serde_json::Value::String(expected.into()));
    }
}
