"""One file: plan() + execute(item), with optional argparse.

Local::

    python3 examples/ppilot/plan_simple.py
    python3 examples/ppilot/plan_simple.py --n 2

Scale (same argv after ``--``)::

    persisting ppilot examples/ppilot/plan_simple.py -w 4 -- --n 2
"""

from __future__ import annotations

import argparse


def _parse_args(argv=None):
    p = argparse.ArgumentParser(description="demo plan")
    p.add_argument("-n", "--n", type=int, default=4, help="number of tasks")
    return p.parse_args(argv)


def plan():
    args = _parse_args()
    for i in range(args.n):
        yield {"id": f"t-{i}", "x": i}


def execute(item):
    x = item["x"]
    return {"x": x, "x2": x * 2}


if __name__ == "__main__":
    for xx in plan():
        print(execute(xx))
