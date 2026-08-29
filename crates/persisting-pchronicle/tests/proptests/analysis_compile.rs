use persisting_pchronicle::analysis_compile::{AnalysisSpec, CompileScope, TableSchema, compile};
use proptest::prelude::*;

fn schema() -> Vec<TableSchema> {
    vec![
        TableSchema {
            name: "dataset.runs".into(),
            columns: vec![
                "_file_".into(),
                "document_id".into(),
                "session_id".into(),
                "agent_id".into(),
                "agent_name".into(),
                "agent_version".into(),
                "agent_model_name".into(),
                "trajectory_id_explicit".into(),
                "final_metrics".into(),
            ],
        },
        TableSchema {
            name: "dataset.steps".into(),
            columns: vec![
                "_file_".into(),
                "document_id".into(),
                "session_id".into(),
                "step_id".into(),
                "source".into(),
                "effective_kind".into(),
                "model_name".into(),
                "had_tool_calls".into(),
                "latency".into(),
                "ttft".into(),
            ],
        },
        TableSchema {
            name: "dataset.tool_calls".into(),
            columns: vec![
                "_file_".into(),
                "document_id".into(),
                "session_id".into(),
                "step_id".into(),
                "tool_call_id".into(),
                "function_name".into(),
                "duration".into(),
            ],
        },
    ]
}

fn analysis_for(case: u8) -> AnalysisSpec {
    match case {
        0 => AnalysisSpec {
            intent: "distribution".into(),
            grain: "step".into(),
            measure: "step_latency_ms".into(),
            dimension: None,
            filters: Vec::new(),
            ranking: None,
            output: "distribution".into(),
            assumptions: Vec::new(),
            identity_columns: Vec::new(),
            uncomputable_reason: None,
        },
        1 => AnalysisSpec {
            intent: "compare".into(),
            grain: "run".into(),
            measure: "step_count_per_run".into(),
            dimension: Some("agent_model_name".into()),
            filters: Vec::new(),
            ranking: None,
            output: "comparison".into(),
            assumptions: Vec::new(),
            identity_columns: Vec::new(),
            uncomputable_reason: None,
        },
        2 => AnalysisSpec {
            intent: "composition".into(),
            grain: "tool_call".into(),
            measure: "tool_call_count".into(),
            dimension: Some("function_name".into()),
            filters: Vec::new(),
            ranking: None,
            output: "table".into(),
            assumptions: Vec::new(),
            identity_columns: Vec::new(),
            uncomputable_reason: None,
        },
        _ => AnalysisSpec {
            intent: "drilldown".into(),
            grain: "run".into(),
            measure: "step_count_per_run".into(),
            dimension: None,
            filters: Vec::new(),
            ranking: None,
            output: "table".into(),
            assumptions: Vec::new(),
            identity_columns: Vec::new(),
            uncomputable_reason: None,
        },
    }
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_supported_analysis_specs_compile_deterministically(case in 0u8..4) {
        let analysis = analysis_for(case);
        let scope = CompileScope {
            dataset: "dataset".into(),
            ..CompileScope::default()
        };
        let left = compile(analysis.clone(), &schema(), &scope).unwrap();
        let right = compile(analysis, &schema(), &scope).unwrap();
        prop_assert_eq!(left.sql, right.sql);
        prop_assert_eq!(left.identity_columns, right.identity_columns);
        prop_assert_eq!(left.expected_columns, right.expected_columns);
    }

    #[test]
    fn public_unknown_analysis_measures_fail_with_a_structured_error(
        measure in proptest::string::string_regex("[A-Za-z0-9_]{1,32}").unwrap(),
    ) {
        prop_assume!(measure != "row_count");
        prop_assume!(measure != "tool_call_count");
        prop_assume!(measure != "step_count_per_run");
        prop_assume!(measure != "tool_call_count_per_run");
        prop_assume!(measure != "step_latency_ms");
        prop_assume!(measure != "step_ttft_ms");
        prop_assume!(measure != "tool_duration_ms");
        let mut analysis = analysis_for(0);
        analysis.measure = measure;
        let error = compile(
            analysis,
            &schema(),
            &CompileScope { dataset: "dataset".into(), ..CompileScope::default() },
        ).unwrap_err();
        prop_assert_eq!(error.code, "unknown_measure");
        prop_assert_eq!(error.field.as_deref(), Some("measure"));
    }
}
