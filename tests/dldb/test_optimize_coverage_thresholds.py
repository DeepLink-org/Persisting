import pandas as pd
import pyarrow as pa
import pytest

from dldb.table import IndexCoverageExceededError
from dldb.utils import stable_hash


def _noop_optimize_indices(lance_table, **kwargs):
    return None


def test_optimize_indices_default_does_not_raise_when_tail_remains(
    session, simple_schema, monkeypatch
):
    session.create_table("t", simple_schema)
    session.add("t", pd.DataFrame({"id": [1], "name": ["a"]}))
    session.create_scalar_index("t", "name")
    session.add("t", pd.DataFrame({"id": [2], "name": ["b"]}))
    monkeypatch.setattr("dldb.table._optimize_indices_on_lance_table", _noop_optimize_indices)
    session.optimize_indices("t")
    assert session.has_unindexed("t") is True


def test_optimize_indices_max_unindexed_rows_raises_after_tail(
    session, simple_schema, monkeypatch
):
    session.create_table("t", simple_schema)
    session.add("t", pd.DataFrame({"id": [1], "name": ["a"]}))
    session.create_scalar_index("t", "name")
    session.add("t", pd.DataFrame({"id": [2], "name": ["b"]}))
    monkeypatch.setattr("dldb.table._optimize_indices_on_lance_table", _noop_optimize_indices)
    with pytest.raises(IndexCoverageExceededError) as ei:
        session.optimize_indices("t", max_unindexed_rows=0)
    assert len(ei.value.failures) == 1
    failure = ei.value.failures[0]
    assert failure.index_name == "name_idx"
    assert (failure.num_unindexed_rows or 0) > 0


def test_optimize_indices_and_thresholds_fail_if_ratio_exceeds(
    session, simple_schema, monkeypatch
):
    session.create_table("t", simple_schema)
    session.add("t", pd.DataFrame({"id": [1], "name": ["a"]}))
    session.create_scalar_index("t", "name")
    session.add("t", pd.DataFrame({"id": [2], "name": ["b"]}))
    monkeypatch.setattr("dldb.table._optimize_indices_on_lance_table", _noop_optimize_indices)
    with pytest.raises(IndexCoverageExceededError):
        session.optimize_indices("t", max_unindexed_rows=10_000, max_unindexed_ratio=0.01)


def test_optimize_indices_hash_runs_all_partitions_then_raises(
    session, monkeypatch
):
    schema = pa.schema(
        [
            pa.field("id", pa.int64()),
            pa.field("name", pa.string()),
            pa.field("uid", pa.int64()),
        ]
    )
    session.create_table(
        "users",
        schema,
        partition_column="uid",
        partition_type="HASH",
        partitions=4,
    )
    uid_a = uid_b = None
    for u in range(1000):
        bucket = stable_hash(u) % 4
        if bucket == 0 and uid_a is None:
            uid_a = u
        if bucket == 1 and uid_b is None:
            uid_b = u
        if uid_a is not None and uid_b is not None:
            break
    session.add("users", pd.DataFrame({"id": [1], "name": ["a"], "uid": [uid_a]}))
    session.add("users", pd.DataFrame({"id": [2], "name": ["b"], "uid": [uid_b]}))
    session.create_scalar_index("users", "name", partition=0)
    session.create_scalar_index("users", "name", partition=1)
    session.add("users", pd.DataFrame({"id": [3], "name": ["c"], "uid": [uid_a]}))
    session.add("users", pd.DataFrame({"id": [4], "name": ["d"], "uid": [uid_b]}))
    monkeypatch.setattr("dldb.table._optimize_indices_on_lance_table", _noop_optimize_indices)
    with pytest.raises(IndexCoverageExceededError) as ei:
        session.optimize_indices("users", max_unindexed_rows=0)
    partitions = {f.partition for f in ei.value.failures}
    assert partitions == {0, 1}
