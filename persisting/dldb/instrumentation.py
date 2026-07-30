from __future__ import annotations

from dataclasses import dataclass
from functools import wraps
from time import perf_counter
from types import MethodType
from typing import Any, Callable, Dict, Optional, Tuple

import pandas as pd


@dataclass(frozen=True)
class _ApiSpec:
    method: str
    api: str
    df_attr: bool = False
    meta_fn: Optional[Callable[..., Optional[dict]]] = None
    datas_arg: Optional[str] = None  # only used by "add" to compute rows/bytes from input


def _safe_df_attr_set(df: pd.DataFrame, timing: dict) -> None:
    try:
        df.attrs["dldb"] = timing
    except Exception:
        # attrs may be immutable or blocked in some pandas builds
        pass


def _build_timing(
    *,
    api: str,
    elapsed_ms: float,
    ok: bool,
    rows: Optional[int],
    bytes_: Optional[int],
    model: Optional[str],
    meta: Optional[dict],
) -> dict:
    rows_per_s = (rows / (elapsed_ms / 1000.0)) if (rows is not None and elapsed_ms > 0) else None
    mb_per_s = ((bytes_ / (1024.0 * 1024.0)) / (elapsed_ms / 1000.0)) if (bytes_ is not None and elapsed_ms > 0) else None
    timing = {
        "api": api,
        "elapsed_ms": float(elapsed_ms),
        "ok": bool(ok),
        "rows": rows,
        "bytes": bytes_,
        "rows_per_s": rows_per_s,
        "mb_per_s": mb_per_s,
    }
    if model == "debug" and meta:
        timing.update(meta)
    return timing


def _default_specs() -> Tuple[_ApiSpec, ...]:
    # Keep meta keys identical to the previous _timed(...) usage in session.py
    return (
        _ApiSpec(
            method="create_table",
            api="create_table",
            meta_fn=lambda self, table_name, schema, mode="create", partition_column=None, partition_type="VALUE", partitions=-1: {
                "table_name": table_name,
                "partition_type": partition_type,
                "partition_column": partition_column,
            },
        ),
        _ApiSpec(method="add", api="add", datas_arg="datas", meta_fn=lambda self, table_name, datas, partition=None: {"table_name": table_name, "partition": partition}),
        _ApiSpec(method="count_rows", api="count_rows", meta_fn=lambda self, table_name, partition=None: {"table_name": table_name, "partition": partition}),
        _ApiSpec(
            method="filter",
            api="filter",
            df_attr=True,
            meta_fn=lambda self, table_name, query, limit=None, columns=None, offset=None, *, partitions=None, partition_cond=None, order_by=None, ascending=True, checkout_latest=False: {
                "table_name": table_name,
                "limit": limit,
                "order_by": order_by,
                "ascending": ascending,
                "partition_cond": partition_cond,
                "partitions": partitions,
            },
        ),
        _ApiSpec(
            method="create_scalar_index",
            api="create_scalar_index",
            meta_fn=lambda self, table_name, column, *, partition=None, index_type="BTREE": {
                "table_name": table_name,
                "column": column,
                "partition": partition,
                "index_type": index_type,
            },
        ),
        _ApiSpec(method="list_indices", api="list_indices", meta_fn=lambda self, table_name, partition=None: {"table_name": table_name, "partition": partition}),
        _ApiSpec(
            method="optimize",
            api="optimize",
            meta_fn=lambda self, table_name, *, partition=None, cleanup_older_than=None, delete_unverified=False, retrain=False: {
                "table_name": table_name,
                "partition": partition,
                "cleanup_older_than": cleanup_older_than,
                "delete_unverified": delete_unverified,
            },
        ),
        _ApiSpec(method="drop_table", api="drop_table", meta_fn=lambda self, table_name, partition=None: {"table_name": table_name, "partition": partition}),
        _ApiSpec(method="table_exists", api="table_exists", meta_fn=lambda self, table_name: {"table_name": table_name}),
        _ApiSpec(method="list_tables", api="list_tables"),
        _ApiSpec(method="get_schema", api="get_schema", meta_fn=lambda self, table_name: {"table_name": table_name}),
        _ApiSpec(method="delete", api="delete", meta_fn=lambda self, table_name, where, partition=None: {"table_name": table_name, "partition": partition}),
        _ApiSpec(method="update", api="update", meta_fn=lambda self, table_name, where, values, partition=None: {"table_name": table_name, "partition": partition}),
        _ApiSpec(method="upsert", api="upsert", datas_arg="datas", meta_fn=lambda self, table_name, columns, datas, partition=None: {"table_name": table_name, "partition": partition}),
        _ApiSpec(method="add_columns", api="add_columns", meta_fn=lambda self, table_name, transforms: {"table_name": table_name}),
        _ApiSpec(method="schema", api="schema", meta_fn=lambda self, table_name: {"table_name": table_name}),
    )


def instrument_session(session: Any, *, specs: Tuple[_ApiSpec, ...] | None = None) -> Any:
    """
    Runtime instrumentation for LanceSession methods.

    - model=None: no wrapping
    - model=debug: attach meta, optionally df.attrs['dldb'], update last_call/last_calls
    - model=metrics: aggregate into MetricsCollector and return summary from shutdown()
    """
    model = getattr(getattr(session, "config", None), "model", None)
    if model is None:
        return session

    if specs is None:
        specs = _default_specs()

    originals: Dict[str, Any] = {}

    for spec in specs:
        if not hasattr(session, spec.method):
            continue

        orig = getattr(session, spec.method)
        if not callable(orig):
            continue

        originals[spec.method] = orig

        @wraps(orig)
        def _wrapped(self, *args, __orig=orig, __spec=spec, **kwargs):
            start = perf_counter()
            ok = True
            result = None
            try:
                result = __orig(*args, **kwargs)
                return result
            except Exception:
                ok = False
                raise
            finally:
                elapsed_ms = (perf_counter() - start) * 1000.0

                meta = None
                if __spec.meta_fn is not None:
                    try:
                        meta = __spec.meta_fn(self, *args, **kwargs)
                    except Exception:
                        meta = None

                datas = None
                if __spec.datas_arg is not None:
                    datas = kwargs.get(__spec.datas_arg)
                    if datas is None and len(args) > 0:
                        # Best-effort: handle positional "datas" for known signatures.
                        # add(table_name, datas, ...)
                        # upsert(table_name, columns, datas, ...)
                        if __spec.api == "add" and len(args) >= 2:
                            datas = args[1]
                        elif __spec.api == "upsert" and len(args) >= 3:
                            datas = args[2]

                rows, bytes_ = self._rows_bytes_from_result(__spec.api, result, datas=datas)
                timing = _build_timing(
                    api=__spec.api,
                    elapsed_ms=elapsed_ms,
                    ok=ok,
                    rows=rows,
                    bytes_=bytes_,
                    model=model,
                    meta=meta,
                )

                if model == "debug" and __spec.df_attr and isinstance(result, pd.DataFrame):
                    _safe_df_attr_set(result, timing)

                self._record_call(timing)

        setattr(session, spec.method, MethodType(_wrapped, session))

    # Expose originals for debugging/recovery if needed
    setattr(session, "_dldb_original_methods", originals)
    return session

