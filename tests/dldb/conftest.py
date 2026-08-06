import importlib.util
import sys
from pathlib import Path

import pandas as pd
import pyarrow as pa
import pytest

_dldb = sys.modules.get("dldb")
if _dldb is None or not hasattr(_dldb, "connect"):
    if _dldb is not None:
        for name in [k for k in sys.modules if k == "dldb" or k.startswith("dldb.")]:
            del sys.modules[name]
    _pkg_root = Path(__file__).resolve().parents[2] / "persisting" / "dldb"
    _spec = importlib.util.spec_from_file_location(
        "dldb",
        _pkg_root / "__init__.py",
        submodule_search_locations=[str(_pkg_root)],
    )
    dldb = importlib.util.module_from_spec(_spec)
    sys.modules["dldb"] = dldb
    _spec.loader.exec_module(dldb)
else:
    dldb = _dldb


@pytest.fixture
def session(tmp_path):
    return dldb.connect(str(tmp_path / "dldb"))


@pytest.fixture
def simple_schema():
    return pa.schema(
        [
            pa.field("id", pa.int64()),
            pa.field("name", pa.string()),
        ]
    )


def _rows(start: int, n: int) -> pd.DataFrame:
    return pd.DataFrame(
        {
            "id": list(range(start, start + n)),
            "name": [f"n{i}" for i in range(start, start + n)],
        }
    )


@pytest.fixture
def simple_table(session, simple_schema):
    session.create_table("t", simple_schema)
    session.add("t", _rows(0, 100))
    return "t"


def lance_table_for(session, table_name: str, partition=None):
    table = session._get_table(table_name, partition)
    if partition is None and hasattr(table, "table"):
        if table.table is None:
            table.open_table()
        return table.table
    table.open_table([partition], create_when_missing=False)
    return table.tables[partition]


def num_unindexed_rows(session, table_name: str, index_name: str, partition=None) -> int:
    lance_tbl = lance_table_for(session, table_name, partition=partition)
    try:
        lance_tbl.checkout_latest()
    except Exception:
        pass
    stats = lance_tbl.index_stats(index_name)
    assert stats is not None, f"missing index_stats for {index_name}"
    return int(stats.num_unindexed_rows)
