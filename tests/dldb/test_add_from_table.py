import pandas as pd
import pyarrow as pa
import pytest

from dldb.utils import stable_hash


HASH_SCHEMA = pa.schema(
    [
        pa.field("id", pa.int64()),
        pa.field("name", pa.string()),
        pa.field("job_id", pa.string()),
    ]
)
SIMPLE_SCHEMA = pa.schema(
    [
        pa.field("id", pa.int64()),
        pa.field("name", pa.string()),
    ]
)


def _two_hash_jobs(partitions_n: int):
    job_a = job_b = bucket_a = bucket_b = None
    for job in ("job-alpha", "job-beta", "job-gamma", "job-delta"):
        bucket = stable_hash(job) % partitions_n
        if bucket_a is None:
            job_a, bucket_a = job, bucket
        elif bucket != bucket_a and bucket_b is None:
            job_b, bucket_b = job, bucket
        if job_a is not None and job_b is not None:
            return job_a, bucket_a, job_b, bucket_b
    raise RuntimeError("could not find two jobs in different HASH buckets")


def _arrow_rows(ids, names, job_ids=None):
    arrays = {
        "id": pa.array(ids, type=pa.int64()),
        "name": pa.array(names, type=pa.string()),
    }
    if job_ids is not None:
        arrays["job_id"] = pa.array(job_ids, type=pa.string())
        return pa.Table.from_pydict(arrays, schema=HASH_SCHEMA)
    return pa.Table.from_pydict(arrays, schema=SIMPLE_SCHEMA)


def test_simple_add_accepts_pyarrow_table(session):
    session.create_table("staging", SIMPLE_SCHEMA)
    table = _arrow_rows([1, 2], ["a", "b"])
    session.add("staging", table)

    rows = session.filter("staging", "id IS NOT NULL").sort_values("id")
    assert list(rows["id"]) == [1, 2]
    assert list(rows["name"]) == ["a", "b"]


def test_add_from_table_appends_matching_hash_partition(session):
    partitions_n = 8
    job, bucket, _, _ = _two_hash_jobs(partitions_n)
    session.create_table("staging", HASH_SCHEMA)
    session.create_table(
        "ht",
        HASH_SCHEMA,
        partition_column="job_id",
        partition_type="HASH",
        partitions=partitions_n,
    )
    session.add("staging", _arrow_rows([1, 2], ["a", "b"], [job, job]))

    session.add_from_table("ht", "staging", bucket)

    assert session.count_rows("ht", partition=bucket) == 2
    rows = session.filter("ht", "id IS NOT NULL", partitions=[bucket]).sort_values("id")
    assert list(rows["id"]) == [1, 2]
    assert list(rows["job_id"]) == [job, job]
    assert session.count_rows("staging") == 2


def test_add_from_table_rejects_mixed_buckets_without_writing_dest(session):
    partitions_n = 8
    job_a, bucket_a, job_b, _ = _two_hash_jobs(partitions_n)
    session.create_table("staging", HASH_SCHEMA)
    session.create_table(
        "ht",
        HASH_SCHEMA,
        partition_column="job_id",
        partition_type="HASH",
        partitions=partitions_n,
    )
    session.add("staging", _arrow_rows([1, 2], ["a", "b"], [job_a, job_b]))

    with pytest.raises(ValueError, match="HASH partition"):
        session.add_from_table("ht", "staging", bucket_a)

    assert session.count_rows("ht") == 0
    assert session.count_rows("staging") == 2


def test_add_from_table_rejects_wrong_partition(session):
    partitions_n = 8
    job_a, bucket_a, _, bucket_b = _two_hash_jobs(partitions_n)
    session.create_table("staging", HASH_SCHEMA)
    session.create_table(
        "ht",
        HASH_SCHEMA,
        partition_column="job_id",
        partition_type="HASH",
        partitions=partitions_n,
    )
    session.add("staging", _arrow_rows([1], ["a"], [job_a]))

    with pytest.raises(ValueError, match="HASH partition"):
        session.add_from_table("ht", "staging", bucket_b)

    assert session.count_rows("ht") == 0


def test_add_from_table_rejects_non_simple_source(session):
    partitions_n = 8
    job, bucket, _, _ = _two_hash_jobs(partitions_n)
    session.create_table(
        "ht",
        HASH_SCHEMA,
        partition_column="job_id",
        partition_type="HASH",
        partitions=partitions_n,
    )
    session.add(
        "ht",
        pd.DataFrame({"id": [1], "name": ["a"], "job_id": [job]}),
    )

    with pytest.raises(ValueError, match="Simple"):
        session.add_from_table("ht", "ht", bucket)


def test_add_from_table_rejects_non_hash_dest(session):
    session.create_table("staging", SIMPLE_SCHEMA)
    session.create_table("plain", SIMPLE_SCHEMA)
    session.add("staging", _arrow_rows([1], ["a"]))

    with pytest.raises(ValueError, match="HASH"):
        session.add_from_table("plain", "staging", 0)


def test_add_from_table_creates_missing_partition(session):
    partitions_n = 8
    job, bucket, _, _ = _two_hash_jobs(partitions_n)
    session.create_table("staging", HASH_SCHEMA)
    session.create_table(
        "ht",
        HASH_SCHEMA,
        partition_column="job_id",
        partition_type="HASH",
        partitions=partitions_n,
    )
    session.add("staging", _arrow_rows([7], ["g"], [job]))

    session.add_from_table("ht", "staging", bucket)

    assert session.count_rows("ht", partition=bucket) == 1
