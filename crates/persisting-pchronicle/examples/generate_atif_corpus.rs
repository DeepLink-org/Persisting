//! Regenerate the deterministic ATIF fixture corpus used by tests and benches.

use std::path::PathBuf;

use anyhow::{Context, Result};
use persisting_pchronicle::{AtifAgent, AtifObservation, AtifStep, AtifToolCall, AtifTrajectory};
use serde_json::{json, Value};

#[derive(Clone, Copy)]
struct Case {
    name: &'static str,
    steps: usize,
    tool_every: Option<usize>,
    parallel_tools: bool,
    reasoning: bool,
    multimodal: bool,
    long_text: bool,
    unicode: bool,
    sparse: bool,
}

const CASES: &[Case] = &[
    Case {
        name: "dialogue_10",
        steps: 10,
        tool_every: None,
        parallel_tools: false,
        reasoning: false,
        multimodal: false,
        long_text: false,
        unicode: false,
        sparse: false,
    },
    Case {
        name: "sequential_tools_12",
        steps: 12,
        tool_every: Some(2),
        parallel_tools: false,
        reasoning: false,
        multimodal: false,
        long_text: false,
        unicode: false,
        sparse: false,
    },
    Case {
        name: "sparse_13",
        steps: 13,
        tool_every: Some(4),
        parallel_tools: false,
        reasoning: false,
        multimodal: false,
        long_text: false,
        unicode: false,
        sparse: true,
    },
    Case {
        name: "parallel_tools_14",
        steps: 14,
        tool_every: Some(2),
        parallel_tools: true,
        reasoning: false,
        multimodal: false,
        long_text: false,
        unicode: false,
        sparse: false,
    },
    Case {
        name: "unicode_zh_15",
        steps: 15,
        tool_every: Some(4),
        parallel_tools: false,
        reasoning: true,
        multimodal: false,
        long_text: false,
        unicode: true,
        sparse: false,
    },
    Case {
        name: "reasoning_16",
        steps: 16,
        tool_every: Some(2),
        parallel_tools: false,
        reasoning: true,
        multimodal: false,
        long_text: false,
        unicode: false,
        sparse: false,
    },
    Case {
        name: "multimodal_18",
        steps: 18,
        tool_every: Some(4),
        parallel_tools: false,
        reasoning: false,
        multimodal: true,
        long_text: false,
        unicode: false,
        sparse: false,
    },
    Case {
        name: "long_context_20",
        steps: 20,
        tool_every: Some(2),
        parallel_tools: true,
        reasoning: true,
        multimodal: false,
        long_text: true,
        unicode: false,
        sparse: false,
    },
];

fn main() -> Result<()> {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/atif"));
    std::fs::create_dir_all(&output)
        .with_context(|| format!("create fixture directory {}", output.display()))?;
    for case in CASES {
        let trajectory = build_case(*case);
        trajectory.validate().map_err(anyhow::Error::from)?;
        let path = output.join(format!("{}.json", case.name));
        let bytes = serde_json::to_vec_pretty(&trajectory)?;
        std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
        println!("{}: {} steps", path.display(), case.steps);
    }
    Ok(())
}

fn build_case(case: Case) -> AtifTrajectory {
    let mut steps = Vec::with_capacity(case.steps);
    for index in 1..=case.steps {
        let source = if index == 1 {
            "system"
        } else if index % 2 == 0 {
            "user"
        } else {
            "agent"
        };
        let agent_ordinal = (index.saturating_sub(1)) / 2;
        let has_tools = source == "agent"
            && case
                .tool_every
                .is_some_and(|every| agent_ordinal % every == 0);
        let call_count = if has_tools && case.parallel_tools {
            2
        } else if has_tools {
            1
        } else {
            0
        };
        let tool_calls = (0..call_count)
            .map(|tool_index| {
                let call_id = format!("{}-step-{index}-call-{tool_index}", case.name);
                AtifToolCall {
                    tool_call_id: call_id,
                    function_name: if tool_index == 0 { "knowledge_search" } else { "calculator" }.into(),
                    arguments: if tool_index == 0 {
                        json!({"query": format!("{} topic {index}", case.name), "limit": 5})
                    } else {
                        json!({"expression": format!("{index} * {}", index + 7)})
                    },
                    extra: (!case.sparse).then(|| json!({"duration_ms": 7 + index + tool_index, "cache_hit": index % 3 == 0})),
                }
            })
            .collect::<Vec<_>>();
        let observation = (!tool_calls.is_empty()).then(|| AtifObservation {
            results: tool_calls
                .iter()
                .enumerate()
                .map(|(tool_index, call)| {
                    json!({
                        "source_call_id": call.tool_call_id,
                        "content": format!("deterministic result {index}-{tool_index}: alpha beta gamma"),
                        "status": "success"
                    })
                })
                .collect(),
        });
        steps.push(AtifStep {
            step_id: index as i64,
            timestamp: (!case.sparse)
                .then(|| format!("2026-06-15T09:{:02}:{:02}Z", index / 60, index % 60)),
            source: source.into(),
            model_name: (source == "agent" && !case.sparse).then(|| "fixture-model-v1".into()),
            reasoning_effort: (case.reasoning && source == "agent")
                .then(|| json!(if index % 4 == 1 { "high" } else { "medium" })),
            message: message(case, index, source),
            reasoning_content: (case.reasoning && source == "agent").then(|| {
                format!("Check constraints for step {index}, select evidence, then answer.")
            }),
            tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
            observation,
            metrics: (!case.sparse && source == "agent").then(|| {
                json!({
                    "prompt_tokens": 120 + index * 11,
                    "completion_tokens": 32 + index * 3,
                    "latency_ms": 40 + index * 2,
                    "ttft_ms": 9 + index
                })
            }),
            extra: (!case.sparse).then(|| json!({"fixture_case": case.name, "ordinal": index})),
            llm_call_count: (source == "agent").then_some(1),
            is_copied_context: (index > 1 && !case.sparse).then_some(index % 5 == 0),
        });
    }
    AtifTrajectory {
        schema_version: "ATIF-v1.7".into(),
        session_id: Some(format!("fixture-{}", case.name)),
        trajectory_id: Some(format!("run-{}", case.name)),
        agent: AtifAgent {
            name: "pchronicle-fixture-agent".into(),
            version: "1.0.0".into(),
            model_name: Some("fixture-model-v1".into()),
            tool_definitions: case.tool_every.map(|_| json!([
                {"name": "knowledge_search", "description": "Search deterministic fixture data"},
                {"name": "calculator", "description": "Evaluate an arithmetic expression"}
            ])),
            extra: Some(json!({"generator": "generate_atif_corpus", "case": case.name})),
        },
        steps,
        notes: Some(format!("Deterministic {} fixture for conversion and storage benchmarks.", case.name)),
        final_metrics: Some(json!({"steps": case.steps, "fixture": true})),
        continued_trajectory_ref: (case.name == "long_context_20").then(|| "run-long-context-next".into()),
        extra: Some(json!({"coverage": {"tools": case.tool_every.is_some(), "reasoning": case.reasoning, "multimodal": case.multimodal}})),
        subagent_trajectories: None,
    }
}

fn message(case: Case, index: usize, source: &str) -> Value {
    let base = if case.unicode {
        format!("第 {index} 轮：验证中文、标点与 Unicode 数据的无损存储。")
    } else {
        format!("Turn {index} from {source}: deterministic fixture content for pChronicle.")
    };
    if case.multimodal && source == "user" && index % 4 == 2 {
        return json!([
            {"type": "text", "text": base},
            {"type": "image_url", "image_url": {"url": format!("https://example.invalid/fixture-{index}.png")}}
        ]);
    }
    if case.long_text {
        let repeated = "The storage benchmark repeats realistic context tokens so column compression can exploit shared structure. ".repeat(24);
        return Value::String(format!("{base} {repeated}"));
    }
    Value::String(base)
}
