import pandas as pd
import pyarrow as pa
import pytest

from dldb.utils import stable_hash


def test_p2_value_partition_requires_partition_and_is_isolated(session):
    schema = pa.schema(
        [
            pa.field("id", pa.int64()),
            pa.field("name", pa.string()),
            pa.field("date", pa.string()),
        ]
    )
    session.create_table(
        "events",
        schema,
        partition_column="date",
        partition_type="VALUE",
    )
    session.add(
        "events",
        pd.DataFrame(
            {
                "id": [1, 2],
                "name": ["a", "b"],
                "date": ["20260101", "20260101"],
            }
        ),
    )
    session.create_scalar_index("events", "name", partition="20260101")
    assert session.has_unindexed("events", partition="20260101") is False

    with pytest.raises(AssertionError):
        session.list_index_coverage("events")

    with pytest.raises(AssertionError):
        session.has_unindexed("events")

    session.add(
        "events",
        pd.DataFrame(
            {
                "id": [3, 4],
                "name": ["c", "d"],
                "date": ["20260102", "20260102"],
            }
        ),
    )
    session.create_scalar_index("events", "name", partition="20260102")
    session.add(
        "events",
        pd.DataFrame(
            {
                "id": [5],
                "name": ["e"],
                "date": ["20260102"],
            }
        ),
    )
    assert session.has_unindexed("events", partition="20260101") is False
    assert session.has_unindexed("events", partition="20260102") is True


def test_p3_hash_partition_requires_partition_and_is_isolated(session):
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
    uid_a, uid_b = None, None
    for u in range(0, 1000):
        bucket = stable_hash(u) % 4
        if bucket == 0 and uid_a is None:
            uid_a = u
        if bucket == 1 and uid_b is None:
            uid_b = u
        if uid_a is not None and uid_b is not None:
            break
    assert uid_a is not None and uid_b is not None

    session.add(
        "users",
        pd.DataFrame({"id": [1], "name": ["a"], "uid": [uid_a]}),
    )
    session.create_scalar_index("users", "name", partition=0)
    assert session.has_unindexed("users", partition=0) is False

    with pytest.raises(AssertionError):
        session.list_index_coverage("users")

    session.add(
        "users",
        pd.DataFrame({"id": [2], "name": ["b"], "uid": [uid_b]}),
    )
    session.create_scalar_index("users", "name", partition=1)
    session.add(
        "users",
        pd.DataFrame({"id": [3], "name": ["c"], "uid": [uid_b]}),
    )
    assert session.has_unindexed("users", partition=0) is False
    assert session.has_unindexed("users", partition=1) is True
