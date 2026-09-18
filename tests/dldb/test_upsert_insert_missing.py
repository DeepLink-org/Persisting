import pandas as pd
import pyarrow as pa
import pytest

from dldb.utils import stable_hash


WIDE_SCHEMA = pa.schema(
    [
        pa.field("id", pa.int64()),
        pa.field("name", pa.string()),
        pa.field("bio", pa.string()),
    ]
)
HASH_SCHEMA = pa.schema(
    [
        pa.field("id", pa.int64()),
        pa.field("name", pa.string()),
        pa.field("bio", pa.string()),
        pa.field("job_id", pa.string()),
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


def test_upsert_default_still_inserts_missing_ids(session):
    session.create_table("t", WIDE_SCHEMA)
    session.add(
        "t",
        pd.DataFrame({"id": [1], "name": ["alice"], "bio": ["wide-alice"]}),
    )
    session.upsert(
        "t",
        ["id"],
        pd.DataFrame({"id": [1, 2], "name": ["ALICE", "bob"], "bio": ["wide-alice", "wide-bob"]}),
    )
    rows = session.filter("t", "id IS NOT NULL").sort_values("id")
    assert list(rows["id"]) == [1, 2]
    assert list(rows["name"]) == ["ALICE", "bob"]
    assert list(rows["bio"]) == ["wide-alice", "wide-bob"]


def test_upsert_insert_missing_false_updates_existing_and_skips_new_ids(session):
    session.create_table("t", WIDE_SCHEMA)
    session.add(
        "t",
        pd.DataFrame({"id": [1], "name": ["alice"], "bio": ["wide-alice"]}),
    )
    session.upsert(
        "t",
        ["id"],
        pd.DataFrame({"id": [1, 2], "name": ["ALICE", "bob"]}),
        insert_missing=False,
    )
    rows = session.filter("t", "id IS NOT NULL").sort_values("id")
    assert list(rows["id"]) == [1]
    assert list(rows["name"]) == ["ALICE"]
    assert list(rows["bio"]) == ["wide-alice"]


def test_hash_upsert_insert_missing_false_does_not_create_buckets(session):
    partitions_n = 8
    session.create_table(
        "ht",
        HASH_SCHEMA,
        partition_column="job_id",
        partition_type="HASH",
        partitions=partitions_n,
    )
    job_a, bucket_a, job_b, bucket_b = _two_hash_jobs(partitions_n)
    session.add(
        "ht",
        pd.DataFrame(
            {"id": [1], "name": ["alice"], "bio": ["wide-alice"], "job_id": [job_a]}
        ),
    )
    assert session._get_table("ht").list_partitions() == [bucket_a]

    session.upsert(
        "ht",
        ["id"],
        pd.DataFrame({"id": [1, 2], "name": ["ALICE", "bob"], "job_id": [job_a, job_b]}),
        insert_missing=False,
    )
    table = session._get_table("ht")
    assert table.list_partitions() == [bucket_a]
    rows = session.filter("ht", "id IS NOT NULL")
    assert list(rows["id"]) == [1]
    assert list(rows["name"]) == ["ALICE"]
    assert list(rows["bio"]) == ["wide-alice"]
    with pytest.raises(ValueError, match="does not exist"):
        session.count_rows("ht", partition=bucket_b)
