#!/usr/bin/env python3
"""HTTP load against persisting traj proxy (mock upstream behind proxy)."""

from __future__ import annotations

import argparse
import json
import statistics
import sys
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed


def one_request(proxy_base: str, session_id: str, req_id: int, timeout: float) -> tuple[bool, float, str | None]:
    url = f"{proxy_base.rstrip('/')}/v1/chat/completions"
    body = json.dumps(
        {
            "model": "mock-model",
            "messages": [{"role": "user", "content": f"stress-{req_id}"}],
        }
    ).encode()
    req = urllib.request.Request(
        url,
        data=body,
        headers={
            "Content-Type": "application/json",
            "x-persisting-session-id": session_id,
        },
        method="POST",
    )
    t0 = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            resp.read()
        if resp.status != 200:
            return False, (time.perf_counter() - t0) * 1000, f"http_{resp.status}"
        return True, (time.perf_counter() - t0) * 1000, None
    except urllib.error.HTTPError as e:
        return False, (time.perf_counter() - t0) * 1000, f"http_{e.code}"
    except Exception as e:
        return False, (time.perf_counter() - t0) * 1000, type(e).__name__


def percentile(sorted_vals: list[float], p: float) -> float:
    if not sorted_vals:
        return 0.0
    k = (len(sorted_vals) - 1) * p
    f = int(k)
    c = min(f + 1, len(sorted_vals) - 1)
    if f == c:
        return sorted_vals[f]
    return sorted_vals[f] + (sorted_vals[c] - sorted_vals[f]) * (k - f)


def main() -> int:
    ap = argparse.ArgumentParser(description="Capture proxy stress load")
    ap.add_argument("--proxy", required=True, help="e.g. http://127.0.0.1:8080")
    ap.add_argument("--session-id", required=True)
    ap.add_argument("-n", "--requests", type=int, default=100)
    ap.add_argument("-c", "--concurrency", type=int, default=10)
    ap.add_argument("--timeout", type=float, default=30.0)
    ap.add_argument("--warmup", type=int, default=3)
    ap.add_argument("--json-out", help="write summary JSON to file")
    args = ap.parse_args()

    if args.requests < 1 or args.concurrency < 1:
        print("requests and concurrency must be >= 1", file=sys.stderr)
        return 2

    for i in range(args.warmup):
        one_request(args.proxy, args.session_id, -i, args.timeout)

    latencies: list[float] = []
    errors: dict[str, int] = {}
    ok = 0
    fail = 0

    t_wall0 = time.perf_counter()
    with ThreadPoolExecutor(max_workers=args.concurrency) as pool:
        futs = [
            pool.submit(one_request, args.proxy, args.session_id, i, args.timeout)
            for i in range(args.requests)
        ]
        for fut in as_completed(futs):
            success, ms, err = fut.result()
            if success:
                ok += 1
                latencies.append(ms)
            else:
                fail += 1
                key = err or "unknown"
                errors[key] = errors.get(key, 0) + 1
    wall_s = time.perf_counter() - t_wall0

    latencies.sort()
    summary = {
        "requests": args.requests,
        "concurrency": args.concurrency,
        "ok": ok,
        "fail": fail,
        "success_rate": ok / args.requests if args.requests else 0.0,
        "wall_seconds": round(wall_s, 3),
        "rps": round(ok / wall_s, 2) if wall_s > 0 else 0.0,
        "latency_ms": {
            "min": round(latencies[0], 2) if latencies else 0,
            "mean": round(statistics.mean(latencies), 2) if latencies else 0,
            "p50": round(percentile(latencies, 0.50), 2),
            "p95": round(percentile(latencies, 0.95), 2),
            "p99": round(percentile(latencies, 0.99), 2),
            "max": round(latencies[-1], 2) if latencies else 0,
        },
        "errors": errors,
        "session_id": args.session_id,
    }

    text = json.dumps(summary, indent=2)
    print(text)
    if args.json_out:
        with open(args.json_out, "w", encoding="utf-8") as f:
            f.write(text)
            f.write("\n")

    return 0 if fail == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
