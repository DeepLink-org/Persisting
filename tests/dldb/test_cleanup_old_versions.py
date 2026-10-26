from datetime import timedelta

import pandas as pd
import pyarrow as pa

from tests.dldb.conftest import lance_table_for, num_unindexed_rows


def _fragment_count(session, table_name: str, partition=None) -> int:
    tbl = lance_table_for(session, table_name, partition=partition)
    try:
        tbl.checkout_latest()
    except Exception:
        pass
    return len(list(tbl.to_lance().get_fragments()))


def _version_count(session, table_name: str, partition=None) -> int:
    tbl = lance_table_for(session, table_name, partition=partition)
    try:
        tbl.checkout_latest()
    except Exception:
        pass
    return len(tbl.to_lance().versions())


def _seed_fragmented_indexed_table(session, name: str, schema) -> None:
    session.create_table(name, schema)
    for i in range(8):
        session.add(name, pd.DataFrame({"id": [i], "name": [f"n{i}"]}))
    session.create_scalar_index(name, "name")
    session.add(name, pd.DataFrame({"id": [100], "name": ["tail"]}))
    assert num_unindexed_rows(session, name, "name_idx") > 0
    assert _fragment_count(session, name) > 1


def _snapshot(session, table_name: str) -> dict:
    return {
        "rows": session.count_rows(table_name),
        "unindexed": num_unindexed_rows(session, table_name, "name_idx"),
        "fragments": _fragment_count(session, table_name),
        "versions": _version_count(session, table_name),
        "ids": sorted(
            session.filter(table_name, "id IS NOT NULL")["id"].tolist()
        ),
    }


def test_cleanup_old_versions_passthrough_returns_stats(session, simple_schema):
    session.create_table("t", simple_schema)
    session.add("t", pd.DataFrame({"id": [1], "name": ["a"]}))
    session.add("t", pd.DataFrame({"id": [2], "name": ["b"]}))

    stats = session.cleanup_old_versions(
        "t",
        cleanup_older_than=timedelta(0),
        delete_unverified=True,
    )
    assert stats is not None
    assert hasattr(stats, "old_versions")
    assert hasattr(stats, "bytes_removed")
    assert int(stats.old_versions) >= 1


def test_three_step_maintain_matches_optimize(session, simple_schema):
    """compact_files + cleanup_old_versions + optimize_indices == optimize."""
    _seed_fragmented_indexed_table(session, "via_optimize", simple_schema)
    _seed_fragmented_indexed_table(session, "via_steps", simple_schema)

    cleanup_kwargs = {
        "cleanup_older_than": timedelta(0),
        "delete_unverified": True,
    }

    session.optimize("via_optimize", **cleanup_kwargs)

    session.compact_files("via_steps")
    session.cleanup_old_versions("via_steps", **cleanup_kwargs)
    session.optimize_indices("via_steps")

    assert _snapshot(session, "via_steps") == _snapshot(session, "via_optimize")


def test_three_step_maintain_matches_optimize_on_hash_partition(session):
    from dldb.utils import stable_hash

    partitions_n = 4
    job = "job-a"
    bucket = stable_hash(job) % partitions_n
    schema = pa.schema(
        [
            pa.field("id", pa.int64()),
            pa.field("name", pa.string()),
            pa.field("job_id", pa.string()),
        ]
    )
    for name in ("via_optimize", "via_steps"):
        session.create_table(
            name,
            schema,
            partition_column="job_id",
            partition_type="HASH",
            partitions=partitions_n,
        )
        for i in range(6):
            session.add(
                name,
                pd.DataFrame(
                    {
                        "id": [i],
                        "name": [f"n{i}"],
                        "job_id": [job],
                    }
                ),
            )
        session.create_scalar_index(name, "name", partition=bucket)
        session.add(
            name,
            pd.DataFrame({"id": [100], "name": ["tail"], "job_id": [job]}),
        )
        assert num_unindexed_rows(session, name, "name_idx", partition=bucket) > 0

    cleanup_kwargs = {
        "cleanup_older_than": timedelta(0),
        "delete_unverified": True,
    }
    session.optimize("via_optimize", partition=bucket, **cleanup_kwargs)
    session.compact_files("via_steps", partition=bucket)
    session.cleanup_old_versions("via_steps", partition=bucket, **cleanup_kwargs)
    session.optimize_indices("via_steps", partition=bucket)

    assert session.count_rows("via_steps") == session.count_rows("via_optimize")
    assert num_unindexed_rows(session, "via_steps", "name_idx", partition=bucket) == 0
    assert num_unindexed_rows(session, "via_optimize", "name_idx", partition=bucket) == 0
    assert _fragment_count(session, "via_steps", partition=bucket) == _fragment_count(
        session, "via_optimize", partition=bucket
    )
