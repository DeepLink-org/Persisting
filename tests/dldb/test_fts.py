import pandas as pd
import pyarrow as pa
import pytest


@pytest.fixture
def fts_schema():
    return pa.schema(
        [
            pa.field("id", pa.int64()),
            pa.field("name", pa.string()),
            pa.field("bio", pa.string()),
            pa.field("status", pa.string()),
        ]
    )


def _users() -> pd.DataFrame:
    return pd.DataFrame(
        {
            "id": [1, 2, 3, 4],
            "name": ["alice", "bob", "carol", "dave"],
            "bio": [
                "loves hiking and dogs",
                "enjoys cooking pasta",
                "hiking with cats",
                "reads science fiction",
            ],
            "status": ["active", "active", "inactive", "active"],
        }
    )


@pytest.fixture
def simple_users(session, fts_schema):
    session.create_table("users", fts_schema)
    session.add("users", _users())
    return "users"


def test_simple_create_fts_index_and_search(session, simple_users):
    session.create_fts_index(simple_users, "bio")
    names = {idx.name for idx in session.list_indices(simple_users)}
    assert "bio_idx" in names

    result = session.fts_search(simple_users, "hiking")
    assert isinstance(result, pd.DataFrame)
    assert set(result["name"]) == {"alice", "carol"}


def test_simple_fts_search_with_where_and_limit(session, simple_users):
    session.create_fts_index(simple_users, "bio")
    result = session.fts_search(
        simple_users,
        "hiking",
        where="status = 'active'",
        limit=1,
        columns=["id", "name", "status"],
    )
    assert len(result) == 1
    assert result.iloc[0]["name"] == "alice"
    assert result.iloc[0]["status"] == "active"
    assert {"id", "name", "status"}.issubset(set(result.columns))


def test_simple_fts_rejects_partition_kwarg(session, simple_users):
    with pytest.raises(NotImplementedError, match="partition="):
        session.create_fts_index(simple_users, "bio", partition="x")
    session.create_fts_index(simple_users, "bio")
    with pytest.raises(NotImplementedError, match="partition="):
        session.fts_search(simple_users, "hiking", partition="x")


def test_value_partition_fts_not_implemented(session, fts_schema):
    session.create_table(
        "events",
        fts_schema,
        partition_column="status",
        partition_type="VALUE",
    )
    session.add(
        "events",
        pd.DataFrame(
            {
                "id": [1],
                "name": ["alice"],
                "bio": ["loves hiking"],
                "status": ["active"],
            }
        ),
    )
    with pytest.raises(NotImplementedError, match="Simple"):
        session.create_fts_index("events", "bio")
    with pytest.raises(NotImplementedError, match="Simple"):
        session.fts_search("events", "hiking")


def test_hash_partition_fts_not_implemented(session, fts_schema):
    session.create_table(
        "users",
        fts_schema,
        partition_column="id",
        partition_type="HASH",
        partitions=4,
    )
    session.add(
        "users",
        pd.DataFrame(
            {
                "id": [1],
                "name": ["alice"],
                "bio": ["loves hiking"],
                "status": ["active"],
            }
        ),
    )
    with pytest.raises(NotImplementedError, match="Simple"):
        session.create_fts_index("users", "bio")
    with pytest.raises(NotImplementedError, match="Simple"):
        session.fts_search("users", "hiking")


def test_create_fts_index_wait_false_still_builds(session, simple_users):
    session.create_fts_index(simple_users, "bio", wait=False)
    session.create_fts_index(simple_users, "bio", wait=True, replace=True)
    result = session.fts_search(simple_users, "pasta")
    assert set(result["name"]) == {"bob"}
