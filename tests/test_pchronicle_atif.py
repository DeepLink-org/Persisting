from __future__ import annotations

import pytest

from persisting.pchronicle import (
    ATIF_TRAJECTORY_VIEW,
    AtifTrajectoryView,
    MemoryChronicleStore,
    atif_trajectory_sql_ddl,
    ingest_trajectory,
    reconstruct_trajectory,
)


SAMPLE = {
    "schema_version": "ATIF-v1.7",
    "session_id": "sess-1",
    "trajectory_id": "traj-1",
    "agent": {"name": "harbor-agent", "version": "1.0.0", "model_name": "gemini-2.5-flash"},
    "steps": [
        {
            "step_id": 1,
            "source": "user",
            "message": "What is the price of GOOGL?",
        },
        {
            "step_id": 2,
            "source": "agent",
            "message": "I will search.",
            "tool_calls": [
                {
                    "tool_call_id": "call_price_1",
                    "function_name": "financial_search",
                    "arguments": {"ticker": "GOOGL", "metric": "price"},
                },
                {
                    "tool_call_id": "call_volume_2",
                    "function_name": "financial_search",
                    "arguments": {"ticker": "GOOGL", "metric": "volume"},
                },
            ],
            "observation": {
                "results": [
                    {"source_call_id": "call_price_1", "content": "$185.35"},
                    {"source_call_id": "call_volume_2", "content": "1.5M"},
                ]
            },
        },
    ],
}


def test_split_ingest_view_roundtrip():
    store = MemoryChronicleStore()
    sid = ingest_trajectory(store, SAMPLE)
    assert sid == "sess-1"
    rebuilt = reconstruct_trajectory(store, sid)
    assert rebuilt["agent"]["name"] == "harbor-agent"
    assert len(rebuilt["steps"][1]["tool_calls"]) == 2

    rows = AtifTrajectoryView(store).query("sess-1")
    assert len(rows) == 3
    assert rows[0].get("tool_call_id") is None
    assert rows[1]["tool_call_id"] == "call_price_1"
    assert rows[2]["tool_call_id"] == "call_volume_2"


def test_sql_ddl():
    ddl = atif_trajectory_sql_ddl()
    assert ATIF_TRAJECTORY_VIEW in ddl
    assert "LEFT JOIN tool_calls" in ddl


def test_python_validation_matches_rust_duplicate_rules():
    invalid = {**SAMPLE, "steps": [SAMPLE["steps"][0], SAMPLE["steps"][0]]}
    with pytest.raises(ValueError, match="duplicate step_id"):
        ingest_trajectory(MemoryChronicleStore(), invalid)


def test_falsy_tool_arguments_roundtrip_without_coercion():
    sample = {
        **SAMPLE,
        "steps": [
            SAMPLE["steps"][0],
            {
                **SAMPLE["steps"][1],
                "tool_calls": [
                    {
                        "tool_call_id": "falsy",
                        "function_name": "f",
                        "arguments": [],
                    }
                ],
            },
        ],
    }
    store = MemoryChronicleStore()
    ingest_trajectory(store, sample)
    rebuilt = reconstruct_trajectory(store, "sess-1")
    assert rebuilt["steps"][1]["tool_calls"][0]["arguments"] == []
