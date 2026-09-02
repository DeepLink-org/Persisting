"""Regression for Persisting#101: cold partitioned filter must not N+1 list_tables()."""

from __future__ import annotations

import configparser
import time
import uuid
from pathlib import Path

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
PARTITIONS = 4
RCLONE_CONF = Path("/home/fangtianshun/Workspace/rclone-v1.74.4-linux-amd64/safactory.conf")
S3_BUCKET = "wind-tunnel-debug"
S3_REMOTE = "safactory"


def _wrap_list_tables(session):
    orig = session.db_conn.list_tables
    counter = {"n": 0, "last_elapsed": 0.0, "total_elapsed": 0.0}

    def wrapped(*args, **kwargs):
        counter["n"] += 1
        t0 = time.perf_counter()
        try:
            return orig(*args, **kwargs)
        finally:
            elapsed = time.perf_counter() - t0
            counter["last_elapsed"] = elapsed
            counter["total_elapsed"] += elapsed

    session.db_conn.list_tables = wrapped
    return counter


def _jobs_for_distinct_buckets(n: int = PARTITIONS) -> list[str]:
    """Pick job_id values that land in distinct HASH buckets."""
    found: dict[int, str] = {}
    i = 0
    while len(found) < n:
        job = f"job-{i}"
        bucket = stable_hash(job) % PARTITIONS
        found.setdefault(bucket, job)
        i += 1
        if i > 10_000:
            raise RuntimeError(f"could not find {n} distinct HASH buckets")
    return [found[b] for b in range(n)]


def _seed_hash_table(session, table_name: str = "ht") -> list[str]:
    session.create_table(
        table_name,
        HASH_SCHEMA,
        partition_column="job_id",
        partition_type="HASH",
        partitions=PARTITIONS,
    )
    jobs = _jobs_for_distinct_buckets(PARTITIONS)
    session.add(
        table_name,
        pd.DataFrame(
            {
                "id": list(range(PARTITIONS)),
                "name": [f"n{i}" for i in range(PARTITIONS)],
                "job_id": jobs,
            }
        ),
    )
    return jobs


def test_cold_full_hash_filter_lists_catalog_once(tmp_path):
    uri = str(tmp_path / "dldb")
    writer = dldb.connect(uri)
    _seed_hash_table(writer, "ht")

    reader = dldb.connect(uri)
    counter = _wrap_list_tables(reader)
    rows = reader.filter("ht", query="id IS NOT NULL")
    assert counter["n"] == 1
    assert len(rows) == PARTITIONS


def test_warm_explicit_hash_filter_skips_catalog(tmp_path):
    uri = str(tmp_path / "dldb")
    session = dldb.connect(uri)
    jobs = _seed_hash_table(session, "ht")
    bucket = stable_hash(jobs[0]) % PARTITIONS
    # Warm the requested bucket in this session.
    session.filter("ht", query="id IS NOT NULL", partitions=[bucket])

    counter = _wrap_list_tables(session)
    rows = session.filter("ht", query="id IS NOT NULL", partitions=[bucket])
    assert counter["n"] == 0
    assert len(rows) == 1


def _parse_rclone_s3(conf_path: Path, remote: str = S3_REMOTE) -> dict:
    parser = configparser.ConfigParser()
    if not parser.read(conf_path) or remote not in parser:
        raise FileNotFoundError(f"missing rclone remote [{remote}] in {conf_path}")
    section = parser[remote]
    endpoint = section.get("endpoint")
    if not endpoint:
        raise ValueError(f"rclone remote [{remote}] missing endpoint")
    return {
        "allow_http": "true",
        "aws_access_key_id": section["access_key_id"],
        "aws_secret_access_key": section["secret_access_key"],
        "aws_endpoint": endpoint,
    }


def _s3_storage_options():
    if not RCLONE_CONF.is_file():
        pytest.skip(f"rclone conf not found: {RCLONE_CONF}")
    try:
        return _parse_rclone_s3(RCLONE_CONF)
    except (FileNotFoundError, KeyError, ValueError) as exc:
        pytest.skip(str(exc))


@pytest.fixture
def s3_session():
    storage = _s3_storage_options()
    run_id = uuid.uuid4().hex[:12]
    uri = f"s3://{S3_BUCKET}/dldb-tests/issue-101/{run_id}"
    table_name = "ht"
    session = None
    try:
        session = dldb.connect(uri, storage_options=storage)
        yield session, uri, table_name, storage
    finally:
        if session is not None:
            try:
                if session.table_exists(table_name):
                    session.drop_table(table_name)
            except Exception:
                pass
            try:
                session.shutdown()
            except Exception:
                pass


@pytest.mark.s3
def test_s3_cold_full_hash_filter_timing(s3_session):
    writer, uri, table_name, storage = s3_session
    _seed_hash_table(writer, table_name)

    reader = dldb.connect(uri, storage_options=storage)
    try:
        counter = _wrap_list_tables(reader)
        reader.db_conn.list_tables()
        t_list = counter["last_elapsed"]
        # Reset after measuring a single list_tables() baseline.
        counter["n"] = 0
        counter["total_elapsed"] = 0.0

        t0 = time.perf_counter()
        rows = reader.filter(table_name, query="id IS NOT NULL")
        elapsed = time.perf_counter() - t0

        print(
            f"s3 cold full filter: list_tables={counter['n']} "
            f"T={t_list:.3f}s list_cost={counter['total_elapsed']:.3f}s "
            f"elapsed={elapsed:.3f}s rows={len(rows)}"
        )
        assert counter["n"] == 1
        assert len(rows) == PARTITIONS
        # Listing cost during filter must stay ~1×T, not ~(N+1)×T.
        # Use 2×T slack for S3 jitter; wall-clock filter includes open/scan.
        assert counter["total_elapsed"] < 2.0 * max(t_list, 1e-6), (
            f"listing cost {counter['total_elapsed']:.3f}s exceeded 2*T ({2.0 * t_list:.3f}s); "
            f"likely still N+1 listing"
        )
        # When catalog listing is expensive, wall-clock must also beat the old N+1 budget.
        if t_list >= 1.0:
            assert elapsed < 4.5 * t_list, (
                f"cold filter {elapsed:.3f}s exceeded 4.5*T ({4.5 * t_list:.3f}s)"
            )
    finally:
        try:
            reader.shutdown()
        except Exception:
            pass


@pytest.mark.s3
def test_s3_warm_explicit_hash_filter_timing(s3_session):
    session, _uri, table_name, _storage = s3_session
    jobs = _seed_hash_table(session, table_name)
    bucket = stable_hash(jobs[0]) % PARTITIONS

    counter = _wrap_list_tables(session)
    session.db_conn.list_tables()
    t_list = counter["last_elapsed"]

    session.filter(table_name, query="id IS NOT NULL", partitions=[bucket])

    counter["n"] = 0
    counter["total_elapsed"] = 0.0
    t0 = time.perf_counter()
    rows = session.filter(table_name, query="id IS NOT NULL", partitions=[bucket])
    elapsed = time.perf_counter() - t0

    print(
        f"s3 warm explicit filter: list_tables={counter['n']} "
        f"T={t_list:.3f}s list_cost={counter['total_elapsed']:.3f}s "
        f"elapsed={elapsed:.3f}s rows={len(rows)}"
    )
    assert counter["n"] == 0
    assert counter["total_elapsed"] == 0.0
    assert len(rows) == 1
    if t_list >= 1.0:
        assert elapsed < t_list, (
            f"warm explicit filter {elapsed:.3f}s not faster than one list_tables T={t_list:.3f}s"
        )
