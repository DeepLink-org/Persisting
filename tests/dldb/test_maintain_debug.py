import pandas as pd
import pyarrow as pa
import pytest
from loguru import logger

import dldb
from dldb.utils import stable_hash


@pytest.fixture
def debug_session(tmp_path):
    return dldb.connect(str(tmp_path / "dldb-debug"), model="debug")


@pytest.fixture
def debug_logs():
    lines = []
    handler_id = logger.add(lambda message: lines.append(message.record["message"]), level="INFO")
    try:
        yield lines
    finally:
        logger.remove(handler_id)


def _coverage_names(state):
    return [row["index_name"] for row in state.get("coverage") or []]


def test_model_none_does_not_record_last_call(session, simple_table):
    session.create_scalar_index(simple_table, "name")
    assert session.last_call is None


def test_debug_create_scalar_index_records_steps_version_and_state(
    debug_session, simple_schema, debug_logs
):
    debug_session.create_table("t", simple_schema)
    debug_session.add("t", pd.DataFrame({"id": [1, 2, 3], "name": ["a", "b", "c"]}))
    debug_session.create_scalar_index("t", "name")

    rec = debug_session.last_call
    assert rec["api"] == "create_scalar_index"
    assert rec["ok"] is True
    assert rec["table_name"] == "t"
    assert rec["elapsed_ms"] > 0
    assert rec["version_before"] is not None
    assert rec["version_after"] is not None
    assert rec["version_after"] >= rec["version_before"]
    assert rec["state"]["rows"] == 3
    assert rec["state"]["fragments"] >= 1
    assert "name_idx" in _coverage_names(rec["state"])
    step_apis = [step["api"] for step in rec["steps"]]
    assert step_apis == ["compact_files", "create_index"]
    assert all(step["ok"] and step["elapsed_ms"] >= 0 for step in rec["steps"])
    assert any(line.startswith("dldb_debug step=compact_files parent=create_scalar_index") for line in debug_logs)
    assert any(line.startswith("dldb_debug api=create_scalar_index ok=true") for line in debug_logs)


def test_debug_optimize_records_nested_steps(debug_session, simple_schema):
    debug_session.create_table("t", simple_schema)
    debug_session.add("t", pd.DataFrame({"id": [1], "name": ["a"]}))
    debug_session.create_scalar_index("t", "name")
    debug_session.optimize("t")

    rec = debug_session.last_call
    assert rec["api"] == "optimize"
    assert rec["ok"] is True
    assert [step["api"] for step in rec["steps"]] == [
        "compact_files",
        "cleanup_old_versions",
        "optimize_indices",
    ]
    assert rec["state"]["rows"] == 1
    assert rec["version_before"] is not None
    assert rec["version_after"] is not None


def test_debug_list_indices_has_state_without_steps(debug_session, simple_schema):
    debug_session.create_table("t", simple_schema)
    debug_session.add("t", pd.DataFrame({"id": [1], "name": ["a"]}))
    debug_session.create_scalar_index("t", "name")
    debug_session.list_indices("t")

    rec = debug_session.last_call
    assert rec["api"] == "list_indices"
    assert rec["ok"] is True
    assert "steps" not in rec
    assert rec["state"]["rows"] == 1
    assert rec["version_before"] == rec["version_after"]


def test_debug_filter_does_not_attach_maintain_state(debug_session, simple_schema):
    debug_session.create_table("t", simple_schema)
    debug_session.add("t", pd.DataFrame({"id": [1], "name": ["a"]}))
    debug_session.filter("t", "id > 0")

    rec = debug_session.last_call
    assert rec["api"] == "filter"
    assert "version_before" not in rec
    assert "steps" not in rec
    assert "state" not in rec


def test_debug_optimize_exception_keeps_completed_steps(
    debug_session, simple_schema, monkeypatch, debug_logs
):
    debug_session.create_table("t", simple_schema)
    debug_session.add("t", pd.DataFrame({"id": [1], "name": ["a"]}))
    debug_session.create_scalar_index("t", "name")

    def boom(*args, **kwargs):
        raise RuntimeError("cleanup boom")

    monkeypatch.setattr("dldb.table._cleanup_on_lance_table", boom)
    with pytest.raises(RuntimeError, match="cleanup boom"):
        debug_session.optimize("t")

    rec = debug_session.last_call
    assert rec["api"] == "optimize"
    assert rec["ok"] is False
    assert rec["version_before"] is not None
    assert [step["api"] for step in rec["steps"]] == ["compact_files"]
    assert rec["steps"][0]["ok"] is True
    assert any("dldb_debug step=compact_files parent=optimize" in line for line in debug_logs)
    assert any("dldb_debug api=optimize ok=false" in line for line in debug_logs)


def test_debug_hash_create_scalar_index_snapshots_target_partition(
    debug_session,
):
    partitions_n = 8
    schema = pa.schema(
        [
            pa.field("id", pa.int64()),
            pa.field("name", pa.string()),
            pa.field("job_id", pa.string()),
        ]
    )
    debug_session.create_table(
        "ht",
        schema,
        partition_column="job_id",
        partition_type="HASH",
        partitions=partitions_n,
    )
    job = "job-alpha"
    bucket = stable_hash(job) % partitions_n
    debug_session.add(
        "ht",
        pd.DataFrame({"id": [1, 2], "name": ["a", "b"], "job_id": [job, job]}),
    )
    debug_session.create_scalar_index("ht", "name", partition=bucket)

    rec = debug_session.last_call
    assert rec["api"] == "create_scalar_index"
    assert rec["partition"] == bucket
    assert rec["state"]["rows"] == 2
    assert rec["state"]["fragments"] >= 1
    assert "name_idx" in _coverage_names(rec["state"])
    assert [step["api"] for step in rec["steps"]] == ["compact_files", "create_index"]
