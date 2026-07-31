from __future__ import annotations

from typing import Dict, Tuple, Iterable

from prometheus_client import CollectorRegistry, Counter, Histogram


class MetricsCollector:
    """
    Online aggregation of call timings and throughput-related counters.

    Field names in summary() are treated as a stable external contract.
    """

    def __init__(self) -> None:
        # Session-scoped registry: pure per-connect aggregation.
        # Do NOT use the default global REGISTRY here, otherwise multiple sessions mix.
        self.registry = CollectorRegistry(auto_describe=True)

        # Prometheus metrics (session-scoped). Labels are intentionally low-cardinality.
        self._calls_total = Counter(
            "dldb_api_calls_total",
            "DLDB API calls total",
            labelnames=("api", "ok"),
            registry=self.registry,
        )
        self._latency_seconds = Histogram(
            "dldb_api_latency_seconds",
            "DLDB API latency in seconds",
            labelnames=("api", "ok"),
            buckets=(
                0.001,
                0.0025,
                0.005,
                0.01,
                0.025,
                0.05,
                0.1,
                0.25,
                0.5,
                1.0,
                2.5,
                5.0,
                10.0,
                30.0,
                60.0,
            ),
            registry=self.registry,
        )
        self._rows_total = Counter(
            "dldb_api_rows_total",
            "DLDB rows processed total (best-effort)",
            labelnames=("api",),
            registry=self.registry,
        )
        self._bytes_total = Counter(
            "dldb_api_bytes_total",
            "DLDB bytes processed total (best-effort)",
            labelnames=("api",),
            registry=self.registry,
        )

    def record(self, timing: dict) -> None:
        api = timing.get("api")
        if not api:
            return

        elapsed_ms = float(timing.get("elapsed_ms") or 0.0)
        ok = bool(timing.get("ok", True))
        rows = timing.get("rows")
        bytes_ = timing.get("bytes")
        ok_label = "true" if ok else "false"

        # Prometheus updates (session-scoped).
        try:
            self._calls_total.labels(api=api, ok=ok_label).inc()
            self._latency_seconds.labels(api=api, ok=ok_label).observe(elapsed_ms / 1000.0)
            if rows is not None:
                self._rows_total.labels(api=api).inc(int(rows))
            if bytes_ is not None:
                self._bytes_total.labels(api=api).inc(int(bytes_))
        except Exception:
            # Metrics export should never break the main code path.
            pass

    def _collect_samples(self) -> Iterable[Tuple[str, Dict[str, str], float]]:
        for metric_family in self.registry.collect():
            for s in metric_family.samples:
                yield s.name, dict(s.labels), float(s.value)

    @staticmethod
    def _sum_by_api_ok(samples: Iterable[Tuple[str, Dict[str, str], float]], name: str) -> Dict[Tuple[str, str], float]:
        out: Dict[Tuple[str, str], float] = {}
        for n, labels, v in samples:
            if n != name:
                continue
            api = labels.get("api")
            ok = labels.get("ok")
            if not api or ok is None:
                continue
            out[(api, ok)] = out.get((api, ok), 0.0) + v
        return out

    @staticmethod
    def _sum_by_api(samples: Iterable[Tuple[str, Dict[str, str], float]], name: str) -> Dict[str, float]:
        out: Dict[str, float] = {}
        for n, labels, v in samples:
            if n != name:
                continue
            api = labels.get("api")
            if not api:
                continue
            out[api] = out.get(api, 0.0) + v
        return out

    def summary(self) -> dict:
        samples = list(self._collect_samples())

        calls = self._sum_by_api_ok(samples, "dldb_api_calls_total")
        latency_sum = self._sum_by_api_ok(samples, "dldb_api_latency_seconds_sum")
        latency_count = self._sum_by_api_ok(samples, "dldb_api_latency_seconds_count")
        rows_total = self._sum_by_api(samples, "dldb_api_rows_total")
        bytes_total = self._sum_by_api(samples, "dldb_api_bytes_total")

        apis = set()
        for (api, _ok) in calls.keys():
            apis.add(api)
        for (api, _ok) in latency_count.keys():
            apis.add(api)
        apis.update(rows_total.keys())
        apis.update(bytes_total.keys())

        by_api: Dict[str, dict] = {}
        total_calls = 0
        total_errors = 0
        total_latency_seconds = 0.0
        total_latency_count = 0
        total_rows = 0
        total_bytes = 0

        for api in sorted(apis):
            c_ok = int(calls.get((api, "true"), 0.0))
            c_err = int(calls.get((api, "false"), 0.0))
            c_all = c_ok + c_err

            # Histogram sum/count are in seconds.
            sum_s = float(latency_sum.get((api, "true"), 0.0) + latency_sum.get((api, "false"), 0.0))
            cnt = int(latency_count.get((api, "true"), 0.0) + latency_count.get((api, "false"), 0.0))
            # Prefer call counter if available (e.g., if someone disables histogram)
            if cnt == 0 and c_all > 0:
                cnt = c_all

            r = int(rows_total.get(api, 0.0))
            b = int(bytes_total.get(api, 0.0))

            by_api[api] = {
                "calls_total": c_all,
                "errors_total": c_err,
                "latency_seconds_sum": sum_s,
                "latency_seconds_count": cnt,
                "rows_total": r,
                "bytes_total": b,
            }

            total_calls += c_all
            total_errors += c_err
            total_latency_seconds += sum_s
            total_latency_count += cnt
            total_rows += r
            total_bytes += b

        return {
            "model": "metrics",
            "total_calls": total_calls,
            "total_errors": total_errors,
            "total_latency_seconds_sum": total_latency_seconds,
            "total_latency_seconds_count": total_latency_count,
            "total_rows": total_rows,
            "total_bytes": total_bytes,
            "by_api": by_api,
            "prometheus": {
                "registry": "session",
                "calls_total": "dldb_api_calls_total",
                "latency_seconds": "dldb_api_latency_seconds",
                "rows_total": "dldb_api_rows_total",
                "bytes_total": "dldb_api_bytes_total",
                "notes": "Quantiles (p95/p99) are computed in Prometheus via histogram_quantile().",
            },
        }
