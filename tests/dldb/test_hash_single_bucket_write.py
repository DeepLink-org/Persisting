import pandas as pd
import pyarrow as pa
import pytest

from dldb.table import _split_arrow_by_hash, _split_pandas_by_hash
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


def _two_jobs_same_bucket(partitions_n: int = PARTITIONS_N):
    seen = {}
    for i in range(10_000):
        job = f"job-same-{i}"
        bucket = stable_hash(job) % partitions_n
        if bucket in seen and seen[bucket] != job:
            return seen[bucket], job, bucket
        seen.setdefault(bucket, job)
    raise RuntimeError("could not find two jobs in the same HASH bucket")


def _rows(ids, names, bios, job_ids):
    return pd.DataFrame(
        {"id": ids, "name": names, "bio": bios, "job_id": job_ids}
    )


def _arrow_rows(ids, names, bios, job_ids):
    return pa.table(
        {
            "id": pa.array(ids, type=pa.int64()),
            "name": pa.array(names, type=pa.string()),
            "bio": pa.array(bios, type=pa.string()),
            "job_id": pa.array(job_ids, type=pa.string()),
        },
        schema=HASH_SCHEMA,
    )


def _create_hash_table(session, name="ht"):
    session.create_table(
        name,
        HASH_SCHEMA,
        partition_column="job_id",
        partition_type="HASH",
        partitions=PARTITIONS_N,
    )
    return name


def test_split_pandas_single_bucket_returns_original_frame():
    job, bucket, _, _ = _two_hash_jobs()
    df = _rows([1, 2], ["a", "b"], ["wide-a", "wide-b"], [job, job])
    groups = _split_pandas_by_hash(df, "job_id", PARTITIONS_N)
    assert list(groups) == [bucket]
    assert groups[bucket] is df
    assert "_hash_partition" not in df.columns


def test_split_pandas_same_bucket_two_jobs_returns_original_frame():
    job_a, job_b, bucket = _two_jobs_same_bucket()
    df = _rows([1, 2], ["a", "b"], ["wide-a", "wide-b"], [job_a, job_b])
    groups = _split_pandas_by_hash(df, "job_id", PARTITIONS_N)
    assert list(groups) == [bucket]
    assert groups[bucket] is df


def test_split_pandas_multi_bucket_copies_and_splits():
    job_a, bucket_a, job_b, bucket_b = _two_hash_jobs()
    df = _rows([1, 2], ["a", "b"], ["wide-a", "wide-b"], [job_a, job_b])
    groups = _split_pandas_by_hash(df, "job_id", PARTITIONS_N)
    assert set(groups) == {bucket_a, bucket_b}
    assert groups[bucket_a] is not df
    assert "_hash_partition" not in df.columns
    assert list(groups[bucket_a]["id"]) == [1]
    assert list(groups[bucket_b]["id"]) == [2]


def test_split_arrow_single_bucket_returns_original_table():
    job, bucket, _, _ = _two_hash_jobs()
    table = _arrow_rows([1, 2], ["a", "b"], ["wide-a", "wide-b"], [job, job])
    groups = _split_arrow_by_hash(table, "job_id", PARTITIONS_N)
    assert list(groups) == [bucket]
    assert groups[bucket] is table


def test_hash_add_single_job_does_not_mutate_caller_frame(session):
    _create_hash_table(session)
    job, bucket, _, _ = _two_hash_jobs()
    df = _rows([1], ["a"], ["wide-a"], [job])
    session.add("ht", df)
    assert "_hash_partition" not in df.columns
    assert session.count_rows("ht", partition=bucket) == 1
    assert list(session.filter("ht", "id IS NOT NULL", partitions=[bucket])["name"]) == ["a"]


def test_hash_add_accepts_pyarrow_table(session):
    _create_hash_table(session)
    job, bucket, _, _ = _two_hash_jobs()
    session.add("ht", _arrow_rows([1, 2], ["a", "b"], ["wide-a", "wide-b"], [job, job]))
    assert session.count_rows("ht", partition=bucket) == 2
    rows = session.filter("ht", "id IS NOT NULL", partitions=[bucket]).sort_values("id")
    assert list(rows["id"]) == [1, 2]
    assert list(rows["job_id"]) == [job, job]


def test_hash_add_splits_pyarrow_table_across_buckets(session):
    _create_hash_table(session)
    job_a, bucket_a, job_b, bucket_b = _two_hash_jobs()
    session.add("ht", _arrow_rows([1, 2], ["a", "b"], ["wide-a", "wide-b"], [job_a, job_b]))
    assert session.count_rows("ht", partition=bucket_a) == 1
    assert session.count_rows("ht", partition=bucket_b) == 1
    assert session.filter("ht", f"job_id = '{job_a}'", partitions=[bucket_a])["id"].tolist() == [1]
    assert session.filter("ht", f"job_id = '{job_b}'", partitions=[bucket_b])["id"].tolist() == [2]


def test_hash_add_specified_partition_rejects_wrong_bucket(session):
    _create_hash_table(session)
    job_a, _, _, bucket_b = _two_hash_jobs()
    with pytest.raises(AssertionError, match="datas must belong partition"):
        session.add("ht", _rows([1], ["a"], ["wide-a"], [job_a]), partition=bucket_b)
    assert session.count_rows("ht") == 0


def test_hash_upsert_single_job_pandas(session):
    _create_hash_table(session)
    job, bucket, _, _ = _two_hash_jobs()
    session.add("ht", _rows([1], ["alice"], ["wide-alice"], [job]))
    session.upsert(
        "ht",
        ["job_id", "id"],
        _rows([1], ["ALICE"], ["wide-alice"], [job]),
        insert_missing=False,
    )
    rows = session.filter("ht", "id = 1", partitions=[bucket])
    assert list(rows["name"]) == ["ALICE"]


def test_hash_upsert_accepts_pyarrow_table(session):
    _create_hash_table(session)
    job, bucket, _, _ = _two_hash_jobs()
    session.add("ht", _rows([1], ["alice"], ["wide-alice"], [job]))
    session.upsert(
        "ht",
        ["job_id", "id"],
        _arrow_rows([1], ["ALICE"], ["wide-alice"], [job]),
        insert_missing=False,
    )
    rows = session.filter("ht", "id = 1", partitions=[bucket])
    assert list(rows["name"]) == ["ALICE"]


def test_hash_upsert_specified_partition_rejects_other_job(session):
    _create_hash_table(session)
    job_a, bucket_a, job_b, _ = _two_hash_jobs()
    session.add("ht", _rows([1], ["alice"], ["wide-alice"], [job_a]))
    with pytest.raises(AssertionError, match="datas must belong partition"):
        session.upsert(
            "ht",
            ["job_id", "id"],
            _rows([2], ["bob"], ["wide-bob"], [job_b]),
            partition=bucket_a,
        )
    rows = session.filter("ht", "id IS NOT NULL", partitions=[bucket_a])
    assert list(rows["name"]) == ["alice"]
