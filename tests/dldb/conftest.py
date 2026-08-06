import pandas as pd
import pyarrow as pa
import pytest

import dldb


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
