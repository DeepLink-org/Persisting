import pandas as pd
import pytest


def _rows(start: int, n: int) -> pd.DataFrame:
    return pd.DataFrame(
        {
            "id": list(range(start, start + n)),
            "name": [f"n{i}" for i in range(start, start + n)],
        }
    )


def test_c1_no_index_empty_coverage(session, simple_table):
    assert session.list_index_coverage(simple_table) == []
    assert session.has_unindexed(simple_table) is False


def test_c2_after_create_scalar_index_fully_covered(session, simple_table):
    session.create_scalar_index(simple_table, "name")
    assert session.has_unindexed(simple_table) is False
    cov = session.list_index_coverage(simple_table)
    assert len(cov) >= 1
    assert all(c.num_unindexed_rows == 0 for c in cov)
    assert all(c.fully_indexed for c in cov)


def test_c3_append_creates_unindexed(session, simple_table):
    session.create_scalar_index(simple_table, "name")
    assert session.has_unindexed(simple_table) is False
    session.add(simple_table, _rows(100, 50))
    assert session.has_unindexed(simple_table) is True
    cov = session.list_index_coverage(simple_table)
    assert any((c.num_unindexed_rows or 0) > 0 for c in cov)


def test_c4_optimize_then_recheck(session, simple_table):
    session.create_scalar_index(simple_table, "name")
    session.add(simple_table, _rows(100, 50))
    assert session.has_unindexed(simple_table) is True
    session.optimize(simple_table)
    indices = session.list_indices(simple_table)
    assert len(indices) >= 1
    assert session.has_unindexed(simple_table) is False
