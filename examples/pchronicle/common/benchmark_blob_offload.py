#!/usr/bin/env python3
"""Compare inline Lance content with objects.lance content offload."""

import json
import math
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

if len(sys.argv) != 5:
    raise SystemExit(
        "usage: benchmark_blob_offload.py INLINE_STORE OFFLOAD_STORE CORPUS_STATS.json ITERATIONS"
    )

inline_arg, offload_arg, stats_arg, iterations_arg = sys.argv[1:]
inline_store = Path(inline_arg)
offload_store = Path(offload_arg)
stats = json.loads(Path(stats_arg).read_text(encoding="utf-8"))
iterations = int(iterations_arg)
if iterations <= 0:
    raise SystemExit("ITERATIONS must be greater than zero")

content_expression = (
    "LENGTH(message_json) + LENGTH(reasoning_effort_json) + "
    "LENGTH(metrics_json) + LENGTH(extra_json)"
)
metadata_sql = "SELECT source, COUNT(*) AS steps FROM steps GROUP BY source ORDER BY source"
content_sql = f"SELECT SUM({content_expression}) AS content_chars FROM steps"

commands = {
    "metadata_inline": [
        "ppilot",
        "query",
        "sql",
        inline_arg,
        "--sql",
        metadata_sql,
    ],
    "metadata_offload": [
        "ppilot",
        "query",
        "sql",
        offload_arg,
        "--sql",
        metadata_sql,
    ],
    "content_inline": [
        "ppilot",
        "query",
        "sql",
        inline_arg,
        "--sql",
        content_sql,
    ],
    "content_offload": [
        "ppilot",
        "query",
        "sql",
        offload_arg,
        "--sql",
        content_sql,
    ],
    "content_preview": [
        "ppilot",
        "query",
        "sql",
        offload_arg,
        "--content-read-mode",
        "preview",
        "--sql",
        content_sql,
    ],
}


def peak_rss_bytes(raw_rss: int) -> int:
    # getrusage reports bytes on macOS and KiB on Linux/BSD.
    return raw_rss if sys.platform == "darwin" else raw_rss * 1024


def run_measured(command: list[str]) -> tuple[int, bytes, bytes, int, float]:
    if not hasattr(os, "wait4"):
        raise SystemExit("this benchmark requires os.wait4 for per-process peak RSS measurement")
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        started = time.perf_counter()
        process = subprocess.Popen(command, stdout=stdout, stderr=stderr)
        _, status, usage = os.wait4(process.pid, 0)
        elapsed = time.perf_counter() - started
        process.returncode = os.waitstatus_to_exitcode(status)
        stdout.seek(0)
        stderr.seek(0)
        return (
            process.returncode,
            stdout.read(),
            stderr.read(),
            peak_rss_bytes(usage.ru_maxrss),
            elapsed,
        )


def percentile_ms(values: list[float], percentile: float) -> float:
    ordered = sorted(values)
    index = max(0, math.ceil(percentile * len(ordered)) - 1)
    return ordered[index] * 1000


def total_bytes(path: Path) -> int:
    return sum(item.stat().st_size for item in path.rglob("*") if item.is_file())


def named_dataset_bytes(store: Path, dataset_name: str) -> int:
    return sum(total_bytes(path) for path in store.rglob(dataset_name) if path.is_dir())


samples: dict[str, list[float]] = {name: [] for name in commands}
rss_samples: dict[str, list[int]] = {name: [] for name in commands}
outputs: dict[str, bytes] = {}
names = tuple(commands)
for iteration in range(iterations):
    # Rotate paths so startup and page-cache effects are not assigned to one mode.
    offset = iteration % len(names)
    for name in names[offset:] + names[:offset]:
        returncode, stdout, stderr, peak_rss, elapsed = run_measured(commands[name])
        if returncode != 0:
            sys.stderr.buffer.write(stderr)
            raise SystemExit(returncode)
        samples[name].append(elapsed)
        rss_samples[name].append(peak_rss)
        previous = outputs.setdefault(name, stdout)
        if stdout != previous:
            raise SystemExit(f"{name} output changed between timing iterations")

if outputs["metadata_inline"] != outputs["metadata_offload"]:
    raise SystemExit("metadata query differs between inline and offloaded stores")
if outputs["content_inline"] != outputs["content_offload"]:
    raise SystemExit("full-content query differs between inline and offloaded stores")


def one_json_row(output: bytes) -> dict:
    rows = [json.loads(line) for line in output.splitlines() if line.strip()]
    if len(rows) != 1:
        raise SystemExit(f"expected one query row, got {len(rows)}")
    return rows[0]


full_content_chars = one_json_row(outputs["content_offload"])["content_chars"]
preview_content_chars = one_json_row(outputs["content_preview"])["content_chars"]
if not 0 < preview_content_chars < full_content_chars:
    raise SystemExit("preview mode did not return a bounded content prefix")

metrics = {
    name: {
        "median_ms": percentile_ms(values, 0.50),
        "p95_ms": percentile_ms(values, 0.95),
        "rows_per_second": stats["steps"] / (percentile_ms(values, 0.50) / 1000),
        "peak_rss_mib": max(rss_samples[name]) / (1024 * 1024),
    }
    for name, values in samples.items()
}

inline_bytes = total_bytes(inline_store)
offload_bytes = total_bytes(offload_store)
inline_objects_bytes = named_dataset_bytes(inline_store, "objects.lance")
offload_objects_bytes = named_dataset_bytes(offload_store, "objects.lance")
inline_non_object_bytes = inline_bytes - inline_objects_bytes
offload_non_object_bytes = offload_bytes - offload_objects_bytes
size_ratio = offload_bytes / inline_bytes
logical_blob_bytes = stats["logical_blob_bytes"]
unique_blob_bytes = stats["unique_blob_json_bytes"]
inline_logical_over_physical = logical_blob_bytes / inline_bytes
logical_over_physical = logical_blob_bytes / offload_bytes
logical_over_objects = logical_blob_bytes / offload_objects_bytes
unique_over_objects = unique_blob_bytes / offload_objects_bytes
effective_ratio_gain = logical_over_physical / inline_logical_over_physical
metadata_ratio = metrics["metadata_inline"]["median_ms"] / metrics["metadata_offload"]["median_ms"]
full_ratio = metrics["content_inline"]["median_ms"] / metrics["content_offload"]["median_ms"]
preview_ratio = metrics["content_offload"]["median_ms"] / metrics["content_preview"]["median_ms"]


def mib(value: int) -> float:
    return value / (1024 * 1024)


def print_query(label: str, name: str) -> None:
    value = metrics[name]
    print(
        f"  {label:<27} {value['median_ms']:8.3f} ms median, "
        f"p95={value['p95_ms']:.3f} ms, "
        f"{value['rows_per_second']:.0f} rows/s, "
        f"peak RSS={value['peak_rss_mib']:.1f} MiB"
    )


print(
    "dataset: "
    f"{stats['trajectories']} trajectories, {stats['steps']} steps, "
    f"{stats['blob_references']} blob references, {stats['unique_blobs']} unique blobs"
)
print("logical content:")
print(f"  referenced blob bytes:      {logical_blob_bytes} ({mib(logical_blob_bytes):.2f} MiB)")
print(f"  unique serialized blobs:    {unique_blob_bytes} ({mib(unique_blob_bytes):.2f} MiB)")
print(f"  cross-reference reuse:      {stats['logical_dedup_ratio']:.2f}x")
print("physical storage (exact file bytes, including Lance metadata):")
print(
    f"  inline store:               {inline_bytes} ({mib(inline_bytes):.2f} MiB); "
    f"objects.lance={inline_objects_bytes} bytes"
)
print(
    f"  offloaded store:            {offload_bytes} ({mib(offload_bytes):.2f} MiB); "
    f"objects.lance={offload_objects_bytes} bytes, "
    f"three-table/control={offload_non_object_bytes} bytes"
)
print(
    f"  inline three-table/control: {inline_non_object_bytes} bytes; "
    f"offload/inline={size_ratio:.3f}x, saved={(1 - size_ratio) * 100:.2f}%"
)
print(
    f"  unique-content compression: unique/objects.lance={unique_over_objects:.2f}x "
    "(objects.lance metadata included)"
)
print(
    f"  effective logical/physical: inline={inline_logical_over_physical:.2f}x, "
    f"offloaded={logical_over_physical:.2f}x, gain={effective_ratio_gain:.2f}x; "
    f"logical/objects.lance={logical_over_objects:.2f}x"
)
print(f"cold process query ({iterations} iterations; process start + open + SQL execution):")
print_query("metadata / inline", "metadata_inline")
print_query("metadata / offloaded", "metadata_offload")
print(f"    inline/offloaded speed ratio: {metadata_ratio:.3f}x (same result)")
print_query("full content / inline", "content_inline")
print_query("full content / offloaded", "content_offload")
print(f"    inline/offloaded speed ratio: {full_ratio:.3f}x (same result)")
print_query("content prefix / preview", "content_preview")
print(
    f"    full-offload/preview speed ratio: {preview_ratio:.3f}x; "
    f"preview chars={preview_content_chars}, full chars={full_content_chars}"
)
print("Conclusion:")
print(
    f"  Storage: shared objects.lance uses {size_ratio * 100:.2f}% of the inline store, "
    f"saving {(1 - size_ratio) * 100:.2f}% for this deliberately high-reuse corpus."
)
print(
    "  Metadata analysis does not hydrate objects.lance; the measured ratio above "
    "shows whether offload is neutral or beneficial on this machine."
)
print(
    "  Full-content analysis preserves the exact result but pays descriptor lookup, "
    "object read, decompression, and hash/length verification costs."
)
print(
    "  Preview mode is intentionally different analysis semantics: it exposes only "
    "the embedded user-content prefix, never the descriptor, and avoids loading the full blob."
)
print(
    "RESULT benchmark=objects_lance_blob_offload "
    f"iterations={iterations} trajectories={stats['trajectories']} steps={stats['steps']} "
    f"blob_references={stats['blob_references']} unique_blobs={stats['unique_blobs']} "
    f"logical_blob_bytes={logical_blob_bytes} unique_blob_bytes={unique_blob_bytes} "
    f"inline_bytes={inline_bytes} offload_bytes={offload_bytes} "
    f"objects_bytes={offload_objects_bytes} offload_over_inline={size_ratio:.4f} "
    f"space_saved_pct={(1 - size_ratio) * 100:.2f} "
    f"unique_over_objects={unique_over_objects:.3f} "
    f"inline_logical_over_store={inline_logical_over_physical:.3f} "
    f"logical_over_store={logical_over_physical:.3f} "
    f"effective_ratio_gain={effective_ratio_gain:.3f} "
    f"metadata_inline_ms={metrics['metadata_inline']['median_ms']:.3f} "
    f"metadata_offload_ms={metrics['metadata_offload']['median_ms']:.3f} "
    f"metadata_inline_over_offload={metadata_ratio:.3f} "
    f"content_inline_ms={metrics['content_inline']['median_ms']:.3f} "
    f"content_offload_ms={metrics['content_offload']['median_ms']:.3f} "
    f"content_inline_over_offload={full_ratio:.3f} "
    f"preview_ms={metrics['content_preview']['median_ms']:.3f} "
    f"full_offload_over_preview={preview_ratio:.3f} semantic_checks=true"
)
