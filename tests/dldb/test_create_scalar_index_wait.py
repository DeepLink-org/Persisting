from datetime import timedelta

import pytest

from dldb.table import _complete_scalar_index_create


class _Idx:
    def __init__(self, name):
        self.name = name


class _FakeLanceTable:
    def __init__(self, names, wait_error=None):
        self.names = list(names)
        self.wait_error = wait_error
        self.wait_calls = []

    def list_indices(self):
        return [_Idx(n) for n in self.names]

    def wait_for_index(self, index_names, timeout=None):
        self.wait_calls.append((list(index_names), timeout))
        if self.wait_error is not None:
            raise self.wait_error


def test_default_does_not_call_wait_for_index_when_index_exists():
    table = _FakeLanceTable(["job_id_idx"])
    _complete_scalar_index_create(table, "job_id_idx")
    assert table.wait_calls == []


def test_wait_timeout_calls_wait_for_index():
    table = _FakeLanceTable(["job_id_idx"])
    timeout = timedelta(seconds=5)
    _complete_scalar_index_create(table, "job_id_idx", wait_timeout=timeout)
    assert table.wait_calls == [(["job_id_idx"], timeout)]


def test_wait_timeout_swallows_timeout_if_index_already_exists():
    table = _FakeLanceTable(
        ["job_id_idx"],
        wait_error=RuntimeError(
            'Timeout error: timed out waiting for indices: ["job_id_idx"] after 300s'
        ),
    )
    _complete_scalar_index_create(
        table, "job_id_idx", wait_timeout=timedelta(seconds=1)
    )
    assert table.wait_calls == [(["job_id_idx"], timedelta(seconds=1))]


def test_wait_timeout_reraises_if_index_missing():
    table = _FakeLanceTable(
        [],
        wait_error=RuntimeError(
            'Timeout error: timed out waiting for indices: ["job_id_idx"] after 300s'
        ),
    )
    with pytest.raises(RuntimeError, match="timed out waiting for indices"):
        _complete_scalar_index_create(
            table, "job_id_idx", wait_timeout=timedelta(seconds=1)
        )


def test_session_create_scalar_index_forwards_wait_timeout_none_by_default(
    session, simple_table, monkeypatch
):
    from dldb import table as table_mod

    calls = []

    def spy(lance_table, index_name, wait_timeout=None):
        calls.append((index_name, wait_timeout))

    monkeypatch.setattr(table_mod, "_complete_scalar_index_create", spy)
    session.create_scalar_index(simple_table, "name")
    assert calls == [("name_idx", None)]
