#!/usr/bin/env python3
"""可复现基准数据：固定 seed 生成 search JSONL 与 trajectory（默认 **TOML** `[[records]]`，可选 JSONL）。

示例：
  python3 scripts/generate_benchmark_data.py --seed 42 --search-rows 100000 --traj-rows 100000 \\
    --search-out /tmp/docs.jsonl --traj-out /tmp/traj_records.toml

集成测试入口见仓库根 `justfile` 中的 `integration` / `smoke`。
"""

from __future__ import annotations

import argparse
import json
import random
from pathlib import Path

WORDS = [
    "alpha",
    "beta",
    "gamma",
    "delta",
    "epsilon",
    "zeta",
    "eta",
    "theta",
    "integration",
    "keyword",
    "document",
    "retrieval",
    "embedding",
    "vector",
    "sparse",
]


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--seed", type=int, default=42, help="deterministic RNG seed")
    ap.add_argument("--search-rows", type=int, default=10_000)
    ap.add_argument("--traj-rows", type=int, default=10_000)
    ap.add_argument("--search-out", type=Path, required=True)
    ap.add_argument("--traj-out", type=Path, required=True)
    ap.add_argument(
        "--traj-format",
        choices=("toml", "jsonl"),
        default="toml",
        help="trajectory output: toml (default, [[records]] per row) or jsonl",
    )
    args = ap.parse_args()

    rng = random.Random(args.seed)

    args.search_out.parent.mkdir(parents=True, exist_ok=True)
    args.traj_out.parent.mkdir(parents=True, exist_ok=True)

    n_s = args.search_rows
    with args.search_out.open("w", encoding="utf-8", buffering=1024 * 1024) as f:
        for i in range(n_s):
            a = WORDS[i % len(WORDS)]
            b = WORDS[(i + 3) % len(WORDS)]
            c = WORDS[(i + 5) % len(WORDS)]
            tok = rng.randrange(2**31)
            line = json.dumps(
                {
                    "id": f"doc-{i:08d}",
                    "text": f"cli benchmark row {i} {a} {b} {c} token-{tok}",
                },
                ensure_ascii=False,
            )
            f.write(line + "\n")

    n_t = args.traj_rows
    with args.traj_out.open("w", encoding="utf-8", buffering=1024 * 1024) as f:
        if args.traj_format == "toml":
            f.write("# benchmark trajectory (generated; root `records` via [[records]] tables)\n\n")
            for i in range(n_t):
                tok = rng.randrange(2**31)
                f.write("[[records]]\n")
                f.write(f"step = {i}\n")
                f.write('kind = "tool"\n')
                f.write(f"detail = \"mock step {i} rnd={tok}\"\n\n")
        else:
            for i in range(n_t):
                tok = rng.randrange(2**31)
                rec = {"step": i, "kind": "tool", "detail": f"mock step {i} rnd={tok}"}
                f.write(json.dumps(rec) + "\n")

    print(
        f"generated search_rows={n_s} traj_rows={n_t} traj_format={args.traj_format} seed={args.seed} "
        f"-> {args.search_out} , {args.traj_out}",
        flush=True,
    )


if __name__ == "__main__":
    main()
