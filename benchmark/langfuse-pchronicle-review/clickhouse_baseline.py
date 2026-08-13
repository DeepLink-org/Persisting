#!/usr/bin/env python3
"""Run the Langfuse-shaped fixture against an isolated ClickHouse 25.12 server.

The script uses only the ClickHouse HTTP API, never prints credentials, and
writes one JSON report. It is intended for the disposable database named
``langfuse_pchronicle_review``.
"""

from __future__ import annotations

import argparse
import json
import math
import pathlib
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, Iterable

DATABASE = "langfuse_pchronicle_review"
LOAD_ROWS_PER_SECOND = 10

COLUMNS = """
    row_kind LowCardinality(String),
    project_id String,
    trace_id String,
    span_id String,
    parent_span_id String,
    logical_id String,
    event_ts DateTime64(3),
    start_time DateTime64(3),
    name LowCardinality(String),
    event_type LowCardinality(String),
    session_id String,
    user_id String,
    environment LowCardinality(String),
    tags Array(String),
    metadata_names Array(String),
    metadata_values Array(String),
    tool_names Array(String),
    model LowCardinality(String),
    input String CODEC(ZSTD(3)),
    output String CODEC(ZSTD(3)),
    usage_input UInt64,
    usage_output UInt64,
    total_cost Decimal(38, 12),
    version UInt32,
    is_deleted Bool,
    bookmarked Bool,
    public Bool,
    dataset_id String,
    dataset_run_id String,
    storage_run_id String,
    payload_json String CODEC(ZSTD(3))
"""

COLUMN_NAMES = [
    "row_kind",
    "project_id",
    "trace_id",
    "span_id",
    "parent_span_id",
    "logical_id",
    "event_ts",
    "start_time",
    "name",
    "event_type",
    "session_id",
    "user_id",
    "environment",
    "tags",
    "metadata_names",
    "metadata_values",
    "tool_names",
    "model",
    "input",
    "output",
    "usage_input",
    "usage_output",
    "total_cost",
    "version",
    "is_deleted",
    "bookmarked",
    "public",
    "dataset_id",
    "dataset_run_id",
    "storage_run_id",
    "payload_json",
]


class ClickHouseHttp:
    def __init__(self, url: str, user: str, password: str, timeout: float = 120.0):
        self.url = url.rstrip("/")
        self.timeout = timeout
        self.headers = {
            "X-ClickHouse-User": user,
            "X-ClickHouse-Key": password,
        }

    def request(
        self,
        query: str,
        body: bytes | None = None,
        settings: dict[str, str | int] | None = None,
    ) -> bytes:
        params = dict(settings or {})
        if body is not None:
            params["query"] = query
            data = body
        else:
            data = query.encode("utf-8")
        url = self.url
        if params:
            url += "?" + urllib.parse.urlencode(params)
        request = urllib.request.Request(url, data=data, headers=self.headers, method="POST")
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                return response.read()
        except urllib.error.HTTPError as error:
            detail = error.read().decode("utf-8", errors="replace")
            raise RuntimeError(f"ClickHouse HTTP {error.code}: {detail[:1000]}") from error

    def json_query(self, query: str) -> dict[str, Any]:
        document = self.request(query.rstrip().rstrip(";") + " FORMAT JSON")
        return json.loads(document)

    def scalar(self, query: str) -> Any:
        result = self.json_query(query)
        if len(result.get("data", [])) != 1 or len(result["data"][0]) != 1:
            raise RuntimeError(f"expected one scalar result for query: {query}")
        return next(iter(result["data"][0].values()))

    def stream(self, query: str, output_format: str) -> dict[str, Any]:
        sql = query.rstrip().rstrip(";") + f" FORMAT {output_format}"
        request = urllib.request.Request(
            self.url,
            data=sql.encode("utf-8"),
            headers=self.headers,
            method="POST",
        )
        started = time.perf_counter()
        with urllib.request.urlopen(request, timeout=self.timeout) as response:
            first = response.read(1)
            first_byte = time.perf_counter()
            remainder = response.read()
        finished = time.perf_counter()
        return {
            "bytes": len(first) + len(remainder),
            "first_byte_ms": (first_byte - started) * 1000.0,
            "elapsed_ms": (finished - started) * 1000.0,
        }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--url", required=True, help="ClickHouse HTTP URL")
    parser.add_argument("--user", default="default")
    parser.add_argument("--password", default="")
    parser.add_argument("--fixture", required=True, type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--load-seconds", type=int, default=3)
    parser.add_argument("--insert-batch-rows", type=int, default=10_000)
    return parser.parse_args()


def execute_ddl(client: ClickHouseHttp) -> None:
    client.request(f"DROP DATABASE IF EXISTS {DATABASE}")
    client.request(f"CREATE DATABASE {DATABASE}")
    client.request(
        f"""
        CREATE TABLE {DATABASE}.staging ({COLUMNS})
        ENGINE = MergeTree
        ORDER BY (row_kind, project_id, event_ts, logical_id)
        """
    )
    client.request(
        f"""
        CREATE TABLE {DATABASE}.events_full (
            {COLUMNS},
            INDEX idx_span_id span_id TYPE bloom_filter(0.01) GRANULARITY 1,
            INDEX idx_trace_id trace_id TYPE bloom_filter(0.01) GRANULARITY 1,
            INDEX idx_user_id user_id TYPE bloom_filter(0.01) GRANULARITY 1,
            INDEX idx_session_id session_id TYPE bloom_filter(0.01) GRANULARITY 1,
            INDEX idx_fts_input lower(input) TYPE text(tokenizer = splitByNonAlpha),
            INDEX idx_fts_output lower(output) TYPE text(tokenizer = splitByNonAlpha),
            INDEX idx_fts_metadata_values metadata_values TYPE text(tokenizer = splitByNonAlpha)
        )
        ENGINE = ReplacingMergeTree(event_ts, is_deleted)
        PARTITION BY toYYYYMM(start_time)
        PRIMARY KEY (project_id, toStartOfMinute(start_time), xxHash32(trace_id))
        ORDER BY (project_id, toStartOfMinute(start_time), xxHash32(trace_id), span_id, start_time)
        SAMPLE BY xxHash32(trace_id)
        SETTINGS index_granularity_bytes = '64Mi', enable_full_text_index = 1
        """
    )
    client.request(
        f"""
        CREATE TABLE {DATABASE}.events_core (
            {COLUMNS},
            INDEX idx_trace_id trace_id TYPE bloom_filter(0.01) GRANULARITY 1,
            INDEX idx_fts_metadata_values metadata_values TYPE text(tokenizer = splitByNonAlpha)
        )
        ENGINE = ReplacingMergeTree(event_ts, is_deleted)
        PARTITION BY toYYYYMM(start_time)
        PRIMARY KEY (project_id, toStartOfMinute(start_time), xxHash32(trace_id))
        ORDER BY (project_id, toStartOfMinute(start_time), xxHash32(trace_id), span_id, start_time)
        SAMPLE BY xxHash32(trace_id)
        SETTINGS enable_full_text_index = 1
        """
    )
    projected = []
    for column in COLUMN_NAMES:
        if column == "input":
            projected.append("leftUTF8(input, 200) AS input")
        elif column == "output":
            projected.append("leftUTF8(output, 200) AS output")
        elif column == "metadata_values":
            projected.append(
                "arrayMap(value -> leftUTF8(value, 200), metadata_values) AS metadata_values"
            )
        else:
            projected.append(column)
    client.request(
        f"""
        CREATE MATERIALIZED VIEW {DATABASE}.events_core_mv TO {DATABASE}.events_core AS
        SELECT {", ".join(projected)} FROM {DATABASE}.events_full
        """
    )
    for table, order_by in [
        ("scores", "(project_id, name, event_ts, logical_id)"),
        ("dataset_run_items_rmt", "(project_id, dataset_id, dataset_run_id, logical_id)"),
        ("blob_storage_file_log", "(project_id, event_ts, logical_id)"),
        (
            "mutation_events",
            "(project_id, toStartOfMinute(start_time), trace_id, span_id, start_time)",
        ),
    ]:
        client.request(
            f"""
            CREATE TABLE {DATABASE}.{table} ({COLUMNS})
            ENGINE = ReplacingMergeTree(event_ts, is_deleted)
            PARTITION BY toYYYYMM(start_time)
            ORDER BY {order_by}
            """
        )


def batches(lines: Iterable[bytes], size: int) -> Iterable[bytes]:
    batch: list[bytes] = []
    for line in lines:
        if line.strip():
            batch.append(line)
        if len(batch) == size:
            yield b"".join(batch)
            batch = []
    if batch:
        yield b"".join(batch)


def load_fixture(client: ClickHouseHttp, fixture: pathlib.Path, batch_rows: int) -> dict[str, Any]:
    started = time.perf_counter()
    inserted = 0
    insert_latencies: list[float] = []
    with fixture.open("rb") as source:
        for body in batches(source, batch_rows):
            row_count = body.count(b"\n")
            insert_started = time.perf_counter()
            client.request(
                f"INSERT INTO {DATABASE}.staging FORMAT JSONEachRow",
                body=body,
                settings={
                    "async_insert": 1,
                    "wait_for_async_insert": 1,
                    "input_format_skip_unknown_fields": 1,
                },
            )
            insert_latencies.append((time.perf_counter() - insert_started) * 1000.0)
            inserted += row_count
    elapsed = time.perf_counter() - started
    return {
        "acknowledged_rows": inserted,
        "elapsed_ms": elapsed * 1000.0,
        "rows_per_second": inserted / elapsed,
        "batch_ack_p95_ms": percentile(insert_latencies, 0.95),
    }


def populate_tables(client: ClickHouseHttp) -> None:
    columns = ", ".join(COLUMN_NAMES)
    for table, row_kind in [
        ("events_full", "event"),
        ("scores", "score"),
        ("dataset_run_items_rmt", "dataset_run_item"),
        ("blob_storage_file_log", "blob_storage_file_log"),
    ]:
        client.request(
            f"INSERT INTO {DATABASE}.{table} ({columns}) "
            f"SELECT {columns} FROM {DATABASE}.staging WHERE row_kind = '{row_kind}'"
        )
    client.request(
        f"INSERT INTO {DATABASE}.mutation_events ({columns}) "
        f"SELECT {columns} FROM {DATABASE}.staging WHERE row_kind = 'event'"
    )


def measure_query(client: ClickHouseHttp, sql: str, repetitions: int = 7) -> dict[str, Any]:
    samples: list[float] = []
    result_rows = 0
    for _ in range(repetitions):
        started = time.perf_counter()
        result = client.json_query(sql)
        samples.append((time.perf_counter() - started) * 1000.0)
        result_rows = int(result.get("rows", len(result.get("data", []))))
    return {
        "repetitions": repetitions,
        "result_rows": result_rows,
        "p50_ms": percentile(samples, 0.50),
        "p95_ms": percentile(samples, 0.95),
        "max_ms": max(samples),
    }


def run_load_phase(
    client: ClickHouseHttp, fixture: pathlib.Path, load_seconds: int
) -> dict[str, Any]:
    with fixture.open("r", encoding="utf-8") as source:
        template = json.loads(next(source))
    query_samples: list[float] = []
    query_errors: list[str] = []
    stop = threading.Event()

    def query_loop() -> None:
        while not stop.is_set():
            started = time.perf_counter()
            try:
                client.scalar(
                    f"SELECT count() FROM {DATABASE}.events_core WHERE project_id = 'project-a'"
                )
                query_samples.append((time.perf_counter() - started) * 1000.0)
            except Exception as error:  # recorded and surfaced in the report
                query_errors.append(str(error)[:500])
                return
            stop.wait(0.25)

    thread = threading.Thread(target=query_loop, name="clickhouse-review-query", daemon=True)
    thread.start()
    ack_samples: list[float] = []
    visibility_samples: list[float] = []
    load_rows = 0
    phase_started = time.perf_counter()
    load_elapsed = 0.0
    try:
        for second in range(load_seconds):
            rows = []
            for offset in range(LOAD_ROWS_PER_SECOND):
                index = second * LOAD_ROWS_PER_SECOND + offset
                row = dict(template)
                row["logical_id"] = f"project-a-clickhouse-load-{index:04d}"
                row["trace_id"] = "project-a-clickhouse-load-trace"
                row["span_id"] = f"project-a-clickhouse-load-span-{index:04d}"
                row["storage_run_id"] = row["trace_id"]
                row["event_ts"] = f"2026-01-01 00:05:{index // 10:02d}.{offset * 100:03d}"
                row["start_time"] = row["event_ts"]
                row["payload_json"] = json.dumps(row, ensure_ascii=False, separators=(",", ":"))
                rows.append(json.dumps(row, ensure_ascii=False, separators=(",", ":")))
            target = phase_started + second + 1
            remaining = target - time.perf_counter()
            if remaining > 0:
                time.sleep(remaining)
            body = ("\n".join(rows) + "\n").encode("utf-8")
            started = time.perf_counter()
            client.request(
                f"INSERT INTO {DATABASE}.events_full FORMAT JSONEachRow",
                body=body,
                settings={
                    "async_insert": 1,
                    "wait_for_async_insert": 1,
                    "input_format_skip_unknown_fields": 1,
                },
            )
            ack_samples.append((time.perf_counter() - started) * 1000.0)
            load_rows += len(rows)
            visibility_started = time.perf_counter()
            visible = int(
                client.scalar(
                    f"SELECT count() FROM {DATABASE}.events_full "
                    "WHERE logical_id LIKE 'project-a-clickhouse-load-%'"
                )
            )
            if visible != load_rows:
                raise RuntimeError(
                    f"acknowledged ClickHouse rows not visible: expected {load_rows}, got {visible}"
                )
            visibility_samples.append((time.perf_counter() - visibility_started) * 1000.0)
        load_elapsed = time.perf_counter() - phase_started
    finally:
        stop.set()
        thread.join(timeout=10)
    visible = int(
        client.scalar(
            f"SELECT count() FROM {DATABASE}.events_full "
            "WHERE logical_id LIKE 'project-a-clickhouse-load-%'"
        )
    )
    return {
        "rows": load_rows,
        "elapsed_ms": load_elapsed * 1000.0,
        "effective_rows_per_second": load_rows / load_elapsed,
        "visible_rows": visible,
        "zero_acknowledged_loss": visible == load_rows,
        "ack_p50_ms": percentile(ack_samples, 0.50),
        "ack_p95_ms": percentile(ack_samples, 0.95),
        "visibility_p50_ms": percentile(visibility_samples, 0.50),
        "visibility_p95_ms": percentile(visibility_samples, 0.95),
        "concurrent_query_p95_ms": percentile(query_samples, 0.95),
        "concurrent_query_errors": query_errors,
    }


def run_mutations(client: ClickHouseHttp) -> dict[str, Any]:
    update_started = time.perf_counter()
    for table in ("events_full", "events_core"):
        client.request(
            f"ALTER TABLE {DATABASE}.{table} UPDATE bookmarked = true, public = true "
            "WHERE project_id = 'project-a' AND logical_id = 'project-a-event-00000000' "
            "SETTINGS mutations_sync = 2"
        )
    update_ms = (time.perf_counter() - update_started) * 1000.0
    updated_full = int(
        client.scalar(
            f"SELECT count() FROM {DATABASE}.events_full WHERE project_id = 'project-a' "
            "AND logical_id = 'project-a-event-00000000' AND bookmarked AND public"
        )
    )
    updated_core = int(
        client.scalar(
            f"SELECT count() FROM {DATABASE}.events_core WHERE project_id = 'project-a' "
            "AND logical_id = 'project-a-event-00000000' AND bookmarked AND public"
        )
    )

    trace_started = time.perf_counter()
    client.request(
        f"DELETE FROM {DATABASE}.mutation_events "
        "WHERE project_id = 'project-a' AND trace_id = 'project-a-trace-0000' "
        "SETTINGS mutations_sync = 2"
    )
    trace_delete_ms = (time.perf_counter() - trace_started) * 1000.0
    trace_remaining = int(
        client.scalar(
            f"SELECT count() FROM {DATABASE}.mutation_events "
            "WHERE project_id = 'project-a' AND trace_id = 'project-a-trace-0000'"
        )
    )

    retention_started = time.perf_counter()
    client.request(
        f"DELETE FROM {DATABASE}.mutation_events "
        "WHERE project_id = 'project-a' AND start_time < '2026-01-01 00:00:30.000' "
        "SETTINGS mutations_sync = 2"
    )
    retention_delete_ms = (time.perf_counter() - retention_started) * 1000.0
    retention_remaining = int(
        client.scalar(
            f"SELECT count() FROM {DATABASE}.mutation_events "
            "WHERE project_id = 'project-a' AND start_time < '2026-01-01 00:00:30.000'"
        )
    )

    project_started = time.perf_counter()
    client.request(
        f"DELETE FROM {DATABASE}.mutation_events WHERE project_id = 'project-b' "
        "SETTINGS mutations_sync = 2"
    )
    project_delete_ms = (time.perf_counter() - project_started) * 1000.0
    project_remaining = int(
        client.scalar(
            f"SELECT count() FROM {DATABASE}.mutation_events WHERE project_id = 'project-b'"
        )
    )
    return {
        "update_flags": {
            "elapsed_ms": update_ms,
            "events_full_visible": updated_full > 0,
            "events_core_visible": updated_core > 0,
        },
        "delete_trace": {
            "elapsed_ms": trace_delete_ms,
            "remaining_rows": trace_remaining,
        },
        "delete_retention": {
            "elapsed_ms": retention_delete_ms,
            "remaining_rows": retention_remaining,
        },
        "delete_project": {
            "elapsed_ms": project_delete_ms,
            "remaining_rows": project_remaining,
        },
    }


def percentile(samples: list[float], quantile: float) -> float:
    if not samples:
        return 0.0
    ordered = sorted(samples)
    rank = math.ceil((len(ordered) - 1) * quantile)
    return ordered[min(rank, len(ordered) - 1)]


def main() -> None:
    args = parse_args()
    if args.load_seconds <= 0 or args.insert_batch_rows <= 0:
        raise SystemExit("load seconds and insert batch rows must be positive")
    if not args.fixture.is_file():
        raise SystemExit(f"fixture does not exist: {args.fixture}")
    if args.output.exists():
        raise SystemExit(f"refusing to overwrite report: {args.output}")

    client = ClickHouseHttp(args.url, args.user, args.password)
    version = str(client.scalar("SELECT version()"))
    execute_ddl(client)
    insert_metrics = load_fixture(client, args.fixture, args.insert_batch_rows)
    populate_started = time.perf_counter()
    populate_tables(client)
    populate_ms = (time.perf_counter() - populate_started) * 1000.0

    counts = {
        table: int(client.scalar(f"SELECT count() FROM {DATABASE}.{table}"))
        for table in (
            "staging",
            "events_full",
            "events_core",
            "scores",
            "dataset_run_items_rmt",
            "blob_storage_file_log",
        )
    }
    event_distinct_ids = int(
        client.scalar(f"SELECT uniqExact(logical_id) FROM {DATABASE}.events_full")
    )
    point_sql = f"""
        SELECT logical_id, event_ts, event_type, trace_id, span_id
        FROM {DATABASE}.events_full
        WHERE project_id = 'project-a'
          AND start_time >= '2026-01-01 00:00:00.000'
          AND trace_id = 'project-a-trace-0000'
          AND span_id = 'project-a-span-00000000'
        ORDER BY event_ts DESC LIMIT 1
    """
    list_sql = f"""
        SELECT logical_id, event_ts, event_type, trace_id, span_id
        FROM {DATABASE}.events_core
        WHERE project_id = 'project-a'
          AND start_time >= '2026-01-01 00:00:00.000'
        ORDER BY start_time DESC LIMIT 100
    """
    facet_sql = f"""
        SELECT arrayJoin(tags) AS tag, count() AS rows
        FROM {DATABASE}.events_core
        WHERE project_id = 'project-a'
        GROUP BY tag ORDER BY rows DESC LIMIT 20
    """
    dashboard_sql = f"""
        SELECT toStartOfInterval(start_time, INTERVAL 10 SECOND) AS bucket,
               quantile(0.95)(usage_input) AS p95_input,
               sum(total_cost) AS total_cost
        FROM {DATABASE}.events_core
        WHERE project_id = 'project-a'
          AND start_time >= '2026-01-01 00:00:00.000'
          AND start_time < '2026-01-01 00:02:00.000'
        GROUP BY bucket ORDER BY bucket
        WITH FILL FROM toDateTime('2026-01-01 00:00:00')
                  TO toDateTime('2026-01-01 00:02:00')
                  STEP INTERVAL 10 SECOND
    """
    fts_sql = f"""
        SELECT logical_id
        FROM {DATABASE}.events_full
        WHERE project_id = 'project-a'
          AND hasAllTokens(lower(input),
              arraySlice(arrayDistinct(tokens(lower('needle-token'))), 1, 64))
          AND positionCaseInsensitiveUTF8(input, 'needle-token') > 0
        LIMIT 100
        SETTINGS enable_full_text_index = 1
    """
    score_sql = f"""
        SELECT name, count() AS rows
        FROM {DATABASE}.scores
        WHERE project_id = 'project-a'
        GROUP BY name ORDER BY rows DESC
    """
    query_metrics = {
        "point": measure_query(client, point_sql),
        "list": measure_query(client, list_sql),
        "facet": measure_query(client, facet_sql),
        "dashboard": measure_query(client, dashboard_sql),
        "full_text": measure_query(client, fts_sql),
        "scores": measure_query(client, score_sql),
    }
    cross_project_point_rows = int(
        client.scalar(
            f"SELECT count() FROM {DATABASE}.events_full "
            "WHERE project_id = 'project-a' AND logical_id = 'project-b-event-00000500'"
        )
    )
    export_query = (
        f"SELECT * FROM {DATABASE}.events_full "
        "WHERE project_id = 'project-a' ORDER BY start_time LIMIT 10000"
    )
    exports = {
        "json_each_row": client.stream(export_query, "JSONEachRow"),
        "parquet": client.stream(export_query, "Parquet"),
    }
    load_phase = run_load_phase(client, args.fixture, args.load_seconds)
    mutations = run_mutations(client)
    memory_resident = client.scalar(
        "SELECT value FROM system.asynchronous_metrics WHERE metric = 'MemoryResident'"
    )

    report = {
        "probe": "langfuse-clickhouse-baseline",
        "clickhouse_version": version,
        "database": DATABASE,
        "fixture": str(args.fixture),
        "insert": insert_metrics,
        "populate_ms": populate_ms,
        "counts": counts,
        "event_physical_rows": counts["events_full"],
        "event_distinct_logical_ids": event_distinct_ids,
        "duplicate_versions_present": counts["events_full"] > event_distinct_ids,
        "cross_project_point_rows": cross_project_point_rows,
        "query_latency": query_metrics,
        "exports": exports,
        "load_phase": load_phase,
        "mutations": mutations,
        "memory_resident_bytes": int(memory_resident),
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
