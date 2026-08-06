#!/usr/bin/env python3
import json
import subprocess
import sys
import time
from pathlib import Path


if len(sys.argv) != 7:
    raise SystemExit(
        "usage: compare_ppilot_query.py ATIF LANCE ATIF_OUTPUT LANCE_OUTPUT ITERATIONS SQL"
    )

atif_input, lance_input, atif_output_arg, lance_output_arg, iterations_arg, sql = sys.argv[1:]
iterations = int(iterations_arg)
if iterations <= 0:
    raise SystemExit("ITERATIONS must be greater than zero")

commands = {
    "atif": ["ppilot", "query", atif_input, "--sql", sql],
    "lance": ["ppilot", "query", lance_input, "--sql", sql],
}
elapsed = {"atif": 0.0, "lance": 0.0}
outputs: dict[str, bytes] = {}

for iteration in range(iterations):
    # Alternate backend order so process warm-up and filesystem cache effects
    # are not assigned to the same backend on every iteration.
    order = ("atif", "lance") if iteration % 2 == 0 else ("lance", "atif")
    for backend in order:
        started = time.perf_counter()
        completed = subprocess.run(commands[backend], stdout=subprocess.PIPE)
        elapsed[backend] += time.perf_counter() - started
        if completed.returncode != 0:
            raise SystemExit(completed.returncode)
        previous = outputs.setdefault(backend, completed.stdout)
        if completed.stdout != previous:
            raise SystemExit(f"{backend} output changed between timing iterations")

atif_output = outputs["atif"]
lance_output = outputs["lance"]
Path(atif_output_arg).write_bytes(atif_output)
Path(lance_output_arg).write_bytes(lance_output)

atif_ms = elapsed["atif"] * 1000 / iterations
lance_ms = elapsed["lance"] * 1000 / iterations
atif_over_lance = atif_ms / lance_ms
if lance_ms <= atif_ms:
    winner = "Lance"
    speedup = atif_ms / lance_ms
else:
    winner = "ATIF"
    speedup = lance_ms / atif_ms

print(
    json.dumps(
        {
            "equal": atif_output == lance_output,
            "iterations": iterations,
            "atif_ms": round(atif_ms, 3),
            "lance_ms": round(lance_ms, 3),
            "atif_over_lance": round(atif_over_lance, 3),
            "winner": winner,
            "speedup": round(speedup, 3),
        },
        separators=(",", ":"),
    )
)
