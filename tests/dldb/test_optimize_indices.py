import pandas as pd
import pyarrow as pa

from dldb.utils import stable_hash
from tests.dldb.conftest import num_unindexed_rows

def test_i1_optimize_indices_clears_unindexed_after_append(session, simple_schema):
    session.create_table("t", simple_schema)
    session.add("t", pd.DataFrame({"id": [1, 2], "name": ["a", "b"]}))
    session.create_scalar_index("t", "name")
    assert num_unindexed_rows(session, "t", "name_idx") == 0

    session.add("t", pd.DataFrame({"id": [3], "name": ["c"]}))
    assert num_unindexed_rows(session, "t", "name_idx") > 0

    result = session.optimize_indices("t")
    assert result is None
    assert num_unindexed_rows(session, "t", "name_idx") == 0

    df = session.filter("t", "name = 'c'")
    assert len(df) == 1
    assert int(df.iloc[0]["id"]) == 3


def test_i2_optimize_after_optimize_indices_then_incremental_again(session, simple_schema):
    session.create_table("t", simple_schema)
    session.add("t", pd.DataFrame({"id": [1], "name": ["a"]}))
    session.create_scalar_index("t", "name")
    assert num_unindexed_rows(session, "t", "name_idx") == 0

    session.add("t", pd.DataFrame({"id": [2], "name": ["b"]}))
    assert num_unindexed_rows(session, "t", "name_idx") > 0
    session.optimize_indices("t")
    assert num_unindexed_rows(session, "t", "name_idx") == 0

    session.optimize("t")
    assert num_unindexed_rows(session, "t", "name_idx") == 0
    assert session.count_rows("t") == 2

    session.add("t", pd.DataFrame({"id": [3], "name": ["c"]}))
    assert num_unindexed_rows(session, "t", "name_idx") > 0
    session.optimize_indices("t")
    assert num_unindexed_rows(session, "t", "name_idx") == 0
    assert session.count_rows("t") == 3


def test_i3_optimize_then_append_then_optimize_indices_only(session, simple_schema):
    session.create_table("t", simple_schema)
    session.add("t", pd.DataFrame({"id": [1], "name": ["a"]}))
    session.create_scalar_index("t", "name")
    session.optimize("t")
    assert num_unindexed_rows(session, "t", "name_idx") == 0

    session.add("t", pd.DataFrame({"id": [2], "name": ["b"]}))
    assert num_unindexed_rows(session, "t", "name_idx") > 0

    session.optimize_indices("t")
    assert num_unindexed_rows(session, "t", "name_idx") == 0

    df = session.filter("t", "name = 'b'")
    assert len(df) == 1
    assert int(df.iloc[0]["id"]) == 2


def test_i5_optimize_indices_without_index_is_noop(session, simple_schema):
    session.create_table("t", simple_schema)
    session.add("t", pd.DataFrame({"id": [1], "name": ["a"]}))
    assert session.list_indices("t") == []

    result = session.optimize_indices("t")
    assert result is None
    assert session.list_indices("t") == []


def test_i4_hash_partition_optimize_indices_only_target_bucket(session):
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

    job_a, job_b, bucket_a, bucket_b = None, None, None, None
    for job in ("job-alpha", "job-beta", "job-gamma", "job-delta"):
        bucket = stable_hash(job) % partitions_n
        if bucket_a is None:
            job_a, bucket_a = job, bucket
        elif bucket != bucket_a and bucket_b is None:
            job_b, bucket_b = job, bucket
        if job_a is not None and job_b is not None:
            break
    assert job_a is not None and job_b is not None
    assert bucket_a != bucket_b

    session.add(
        "ht",
        pd.DataFrame(
            {
                "id": [1, 2],
                "name": ["a", "b"],
                "job_id": [job_a, job_b],
            }
        ),
    )
    session.create_scalar_index("ht", "name", partition=bucket_a)
    session.create_scalar_index("ht", "name", partition=bucket_b)
    assert num_unindexed_rows(session, "ht", "name_idx", partition=bucket_a) == 0
    assert num_unindexed_rows(session, "ht", "name_idx", partition=bucket_b) == 0

    session.add(
        "ht",
        pd.DataFrame({"id": [3], "name": ["c"], "job_id": [job_a]}),
    )
    unindexed_a = num_unindexed_rows(session, "ht", "name_idx", partition=bucket_a)
    unindexed_b = num_unindexed_rows(session, "ht", "name_idx", partition=bucket_b)
    assert unindexed_a > 0

    session.optimize_indices("ht", partition=bucket_a)
    assert num_unindexed_rows(session, "ht", "name_idx", partition=bucket_a) == 0
    assert num_unindexed_rows(session, "ht", "name_idx", partition=bucket_b) == unindexed_b
