"""insert_missing=False + partition= must open by name, not list the catalog."""

from __future__ import annotations

import pandas as pd
import pyarrow as pa
import pytest

from dldb.utils import stable_hash


HASH_SCHEMA = pa.schema(
    [
        pa.field("id", pa.int64()),
        pa.field("name", pa.string()),
        pa.field("bio", pa.string()),
        pa.field("job_id", pa.string()),
    ]
)
PARTITIONS_N = 8


def _two_hash_jobs(partitions_n: int = PARTITIONS_N):
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


def _rows(ids, names, bios, job_ids):
    return pd.DataFrame(
        {"id": ids, "name": names, "bio": bios, "job_id": job_ids}
    )


def _wrap_list_tables(session):
    orig = session.db_conn.list_tables
    counter = {"n": 0}

    def wrapped(*args, **kwargs):
        counter["n"] += 1
        return orig(*args, **kwargs)

    session.db_conn.list_tables = wrapped
    return counter


def _create_and_seed(session):
    session.create_table(
        "ht",
        HASH_SCHEMA,
        partition_column="job_id",
        partition_type="HASH",
        partitions=PARTITIONS_N,
    )
    job, bucket, job_b, bucket_b = _two_hash_jobs()
    session.add("ht", _rows([1], ["alice"], ["wide-alice"], [job]))
    # Warm logical table handle so _get_table does not re-OPEN via catalog.
    _ = session._get_table("ht")
    return job, bucket, job_b, bucket_b


def test_hash_upsert_with_partition_skips_list_tables(session):
    job, bucket, _, _ = _create_and_seed(session)
    # Drop any cached physical open so upsert must open the bucket again.
    session._get_table("ht").tables.clear()

    counter = _wrap_list_tables(session)
    session.upsert(
        "ht",
        ["job_id", "id"],
        _rows([1], ["ALICE"], ["wide-alice"], [job]),
        partition=bucket,
        insert_missing=False,
    )
    assert counter["n"] == 0
    rows = session.filter("ht", "id = 1", partitions=[bucket])
    assert list(rows["name"]) == ["ALICE"]


def test_hash_upsert_without_partition_still_lists_catalog(session):
    job, bucket, _, _ = _create_and_seed(session)
    session._get_table("ht").tables.clear()

    counter = _wrap_list_tables(session)
    session.upsert(
        "ht",
        ["job_id", "id"],
        _rows([1], ["ALICE"], ["wide-alice"], [job]),
        insert_missing=False,
    )
    assert counter["n"] >= 1
    rows = session.filter("ht", "id = 1", partitions=[bucket])
    assert list(rows["name"]) == ["ALICE"]


def test_hash_upsert_partition_unmaterialized_bucket_is_noop(session):
    _, _, job_b, bucket_b = _create_and_seed(session)
    session._get_table("ht").tables.clear()

    counter = _wrap_list_tables(session)
    session.upsert(
        "ht",
        ["job_id", "id"],
        _rows([2], ["bob"], ["wide-bob"], [job_b]),
        partition=bucket_b,
        insert_missing=False,
    )
    assert counter["n"] == 0
    table = session._get_table("ht")
    assert bucket_b not in table.list_partitions()
    with pytest.raises(ValueError, match="does not exist"):
        session.count_rows("ht", partition=bucket_b)
