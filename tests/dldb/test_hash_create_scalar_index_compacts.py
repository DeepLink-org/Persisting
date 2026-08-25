import pandas as pd
import pyarrow as pa
from lance.dataset import DatasetOptimizer

from dldb.utils import stable_hash
from tests.dldb.conftest import lance_table_for


def _fragment_count(session, table_name, partition) -> int:
    tbl = lance_table_for(session, table_name, partition=partition)
    try:
        tbl.checkout_latest()
    except Exception:
        pass
    return len(list(tbl.to_lance().get_fragments()))


def test_hash_create_scalar_index_compacts_target_bucket(session):
    partitions_n = 8
    schema = pa.schema(
        [
            pa.field("id", pa.int64()),
            pa.field("name", pa.string()),
            pa.field("job_id", pa.string()),
        ]
    )
    session.create_table(
        "ht",
        schema,
        partition_column="job_id",
        partition_type="HASH",
        partitions=partitions_n,
    )
    job_a = job_b = bucket_a = bucket_b = None
    for job in ("job-alpha", "job-beta", "job-gamma", "job-delta"):
        bucket = stable_hash(job) % partitions_n
        if bucket_a is None:
            job_a, bucket_a = job, bucket
        elif bucket != bucket_a and bucket_b is None:
            job_b, bucket_b = job, bucket
        if job_a is not None and job_b is not None:
            break

    for i in range(6):
        session.add(
            "ht",
            pd.DataFrame({"id": [i], "name": [f"a{i}"], "job_id": [job_a]}),
        )
    session.add(
        "ht",
        pd.DataFrame({"id": [100], "name": ["b0"], "job_id": [job_b]}),
    )
    before_a = _fragment_count(session, "ht", bucket_a)
    before_b = _fragment_count(session, "ht", bucket_b)
    assert before_a >= 6

    session.create_scalar_index("ht", "name", partition=bucket_a)

    after_a = _fragment_count(session, "ht", bucket_a)
    after_b = _fragment_count(session, "ht", bucket_b)
    assert after_a < before_a
    assert after_b == before_b
    names = {idx.name for idx in session.list_indices("ht", partition=bucket_a)}
    assert "name_idx" in names


def test_hash_create_scalar_index_uses_default_compact_batch_size(
    session, monkeypatch
):
    partitions_n = 4
    schema = pa.schema(
        [
            pa.field("id", pa.int64()),
            pa.field("name", pa.string()),
            pa.field("job_id", pa.string()),
        ]
    )
    session.create_table(
        "ht",
        schema,
        partition_column="job_id",
        partition_type="HASH",
        partitions=partitions_n,
    )
    job = "job-alpha"
    bucket = stable_hash(job) % partitions_n
    session.add("ht", pd.DataFrame({"id": [1], "name": ["a"], "job_id": [job]}))

    calls = []
    orig = DatasetOptimizer.compact_files

    def spy(self, *args, **kwargs):
        calls.append(kwargs)
        return orig(self, *args, **kwargs)

    monkeypatch.setattr(DatasetOptimizer, "compact_files", spy)
    session.create_scalar_index("ht", "name", partition=bucket)
    assert calls, "HASH create_scalar_index must compact_files first"
    assert calls[0].get("batch_size") == 64


def test_value_create_scalar_index_compacts_target_partition(session):
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
    for i in range(6):
        session.add(
            "events",
            pd.DataFrame({"id": [i], "name": [f"a{i}"], "date": ["20260101"]}),
        )
    session.add(
        "events",
        pd.DataFrame({"id": [100], "name": ["b0"], "date": ["20260102"]}),
    )
    before_a = _fragment_count(session, "events", "20260101")
    before_b = _fragment_count(session, "events", "20260102")
    assert before_a >= 6

    session.create_scalar_index("events", "name", partition="20260101")

    after_a = _fragment_count(session, "events", "20260101")
    after_b = _fragment_count(session, "events", "20260102")
    assert after_a < before_a
    assert after_b == before_b
    names = {
        idx.name for idx in session.list_indices("events", partition="20260101")
    }
    assert "name_idx" in names


def test_value_create_scalar_index_uses_default_compact_batch_size(
    session, monkeypatch
):
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
        pd.DataFrame({"id": [1], "name": ["a"], "date": ["20260101"]}),
    )
    calls = []
    orig = DatasetOptimizer.compact_files

    def spy(self, *args, **kwargs):
        calls.append(kwargs)
        return orig(self, *args, **kwargs)

    monkeypatch.setattr(DatasetOptimizer, "compact_files", spy)
    session.create_scalar_index("events", "name", partition="20260101")
    assert calls, "VALUE create_scalar_index must compact_files first"
    assert calls[0].get("batch_size") == 64


def test_simple_create_scalar_index_compacts(session):
    schema = pa.schema(
        [
            pa.field("id", pa.int64()),
            pa.field("name", pa.string()),
        ]
    )
    session.create_table("t", schema)
    for i in range(6):
        session.add("t", pd.DataFrame({"id": [i], "name": [f"a{i}"]}))
    before = _fragment_count(session, "t", None)
    assert before >= 6

    session.create_scalar_index("t", "name")

    after = _fragment_count(session, "t", None)
    assert after < before
    names = {idx.name for idx in session.list_indices("t")}
    assert "name_idx" in names


def test_simple_create_scalar_index_uses_default_compact_batch_size(
    session, monkeypatch
):
    schema = pa.schema(
        [
            pa.field("id", pa.int64()),
            pa.field("name", pa.string()),
        ]
    )
    session.create_table("t", schema)
    session.add("t", pd.DataFrame({"id": [1], "name": ["a"]}))
    calls = []
    orig = DatasetOptimizer.compact_files

    def spy(self, *args, **kwargs):
        calls.append(kwargs)
        return orig(self, *args, **kwargs)

    monkeypatch.setattr(DatasetOptimizer, "compact_files", spy)
    session.create_scalar_index("t", "name")
    assert calls, "Simple create_scalar_index must compact_files first"
    assert calls[0].get("batch_size") == 64
