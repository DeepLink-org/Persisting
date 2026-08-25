import pandas as pd
import pyarrow as pa
import pytest

import dldb
from dldb.utils import stable_hash


HASH_SCHEMA = pa.schema(
    [
        pa.field("id", pa.int64()),
        pa.field("name", pa.string()),
        pa.field("job_id", pa.string()),
    ]
)
VALUE_SCHEMA = pa.schema(
    [
        pa.field("id", pa.int64()),
        pa.field("name", pa.string()),
        pa.field("dt", pa.string()),
    ]
)


def _wrap_list_tables(session):
    orig = session.db_conn.list_tables
    counter = {"n": 0}

    def wrapped(*args, **kwargs):
        counter["n"] += 1
        return orig(*args, **kwargs)

    session.db_conn.list_tables = wrapped
    return counter


def _reconnect(tmp_path):
    return dldb.connect(str(tmp_path / "dldb"))


def test_overlapping_prefix_different_partition_types(tmp_path):
    session = _reconnect(tmp_path)
    session.create_table(
        "aaaa",
        HASH_SCHEMA,
        partition_column="job_id",
        partition_type="HASH",
        partitions=4,
    )
    session.create_table(
        "aaaa_legacy",
        VALUE_SCHEMA,
        partition_column="dt",
        partition_type="VALUE",
    )
    session.add("aaaa", pd.DataFrame({"id": [1], "name": ["hash"], "job_id": ["job-a"]}))
    session.add(
        "aaaa_legacy",
        pd.DataFrame({"id": [2], "name": ["value"], "dt": ["20260101"]}),
    )

    other = _reconnect(tmp_path)
    hash_rows = other.filter("aaaa", query="id IS NOT NULL")
    legacy_rows = other.filter("aaaa_legacy", query="id IS NOT NULL")
    assert list(hash_rows["name"]) == ["hash"]
    assert list(legacy_rows["name"]) == ["value"]
    assert other.count_rows("aaaa") == 1
    assert other.count_rows("aaaa_legacy") == 1


def test_overlapping_prefix_same_partition_type(tmp_path):
    session = _reconnect(tmp_path)
    session.create_table(
        "aaaa",
        HASH_SCHEMA,
        partition_column="job_id",
        partition_type="HASH",
        partitions=4,
    )
    session.create_table(
        "aaaa_backup",
        HASH_SCHEMA,
        partition_column="job_id",
        partition_type="HASH",
        partitions=8,
    )
    session.add("aaaa", pd.DataFrame({"id": [1], "name": ["live"], "job_id": ["job-a"]}))
    session.add("aaaa_backup", pd.DataFrame({"id": [2], "name": ["bak"], "job_id": ["job-b"]}))

    other = _reconnect(tmp_path)
    live = other.filter("aaaa", query="id IS NOT NULL")
    backup = other.filter("aaaa_backup", query="id IS NOT NULL")
    assert list(live["name"]) == ["live"]
    assert list(backup["name"]) == ["bak"]
    assert other.get_schema("aaaa") is not None
    record = other.schema_table.get("aaaa")
    backup_record = other.schema_table.get("aaaa_backup")
    assert record.partitions == 4
    assert backup_record.partitions == 8


def test_repeated_hash_bucket_access_skips_catalog_scan(session):
    session.create_table(
        "ht",
        HASH_SCHEMA,
        partition_column="job_id",
        partition_type="HASH",
        partitions=4,
    )
    job = None
    bucket = None
    for candidate in ("job-a", "job-b", "job-c", "job-d", "job-e"):
        bucket = stable_hash(candidate) % 4
        job = candidate
        break
    session.add("ht", pd.DataFrame({"id": [1], "name": ["a"], "job_id": [job]}))
    session.filter("ht", query="id IS NOT NULL", partitions=[bucket])

    counter = _wrap_list_tables(session)
    session.filter("ht", query="id IS NOT NULL", partitions=[bucket])
    session.filter("ht", query="id IS NOT NULL", partitions=[bucket])
    assert counter["n"] == 0


def test_drop_and_recreate_does_not_reuse_stale_wrapper(session):
    session.create_table("t", HASH_SCHEMA, partition_column="job_id", partition_type="HASH", partitions=4)
    session.add("t", pd.DataFrame({"id": [1], "name": ["old"], "job_id": ["job-a"]}))
    session.drop_table("t")
    with pytest.raises(AssertionError, match="not exist"):
        session.filter("t", query="id IS NOT NULL")

    session.create_table("t", HASH_SCHEMA, partition_column="job_id", partition_type="HASH", partitions=4)
    session.add("t", pd.DataFrame({"id": [2], "name": ["new"], "job_id": ["job-a"]}))
    rows = session.filter("t", query="id IS NOT NULL")
    assert list(rows["name"]) == ["new"]
    assert list(rows["id"]) == [2]


def test_missing_logical_table_does_not_open_physical_name(session):
    session.create_table(
        "ht",
        HASH_SCHEMA,
        partition_column="job_id",
        partition_type="HASH",
        partitions=4,
    )
    session.add("ht", pd.DataFrame({"id": [1], "name": ["a"], "job_id": ["job-a"]}))
    physical_names = [n for n in session.db_conn.list_tables().tables if n.startswith("ht_type_HASH_")]
    assert physical_names
    with pytest.raises(AssertionError, match="not exist"):
        session.filter(physical_names[0], query="id IS NOT NULL")
