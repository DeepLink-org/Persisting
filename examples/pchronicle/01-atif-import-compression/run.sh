#!/usr/bin/env bash
set -euo pipefail

export PATH="../../../target/release:$PATH"

# Remove generated ATIF and Lance data from the previous run.
rm -rf .work
mkdir .work

# Expand the small fixture set, then import it into pChronicle's Lance layout.
python3 ../common/generate_atif.py ../../../crates/persisting-pchronicle/tests/fixtures/atif \
  .work/trajectories.ndjson 64
ppilot chronicle import .work/trajectories.ndjson .work/lance

# Compare exact file bytes, including Lance data, indices and version metadata.
python3 - .work/trajectories.ndjson .work/lance <<'PY'
from pathlib import Path
import sys


def total_bytes(path: Path) -> int:
    if path.is_file():
        return path.stat().st_size
    return sum(item.stat().st_size for item in path.rglob("*") if item.is_file())


def human_bytes(value: int) -> str:
    units = ("B", "KiB", "MiB", "GiB")
    size = float(value)
    for unit in units:
        if size < 1024 or unit == units[-1]:
            return f"{size:.2f} {unit}"
        size /= 1024
    raise AssertionError("unreachable")


atif_bytes = total_bytes(Path(sys.argv[1]))
lance_bytes = total_bytes(Path(sys.argv[2]))
if atif_bytes == 0 or lance_bytes == 0:
    raise SystemExit("cannot compare empty ATIF or Lance output")

lance_ratio = lance_bytes / atif_bytes
space_saved = (1 - lance_ratio) * 100
compression_ratio = atif_bytes / lance_bytes

print("Storage comparison (exact file bytes):")
print(f"  ATIF JSONL:  {atif_bytes:>10} bytes ({human_bytes(atif_bytes)})")
print(f"  Lance store: {lance_bytes:>10} bytes ({human_bytes(lance_bytes)})")
if lance_bytes <= atif_bytes:
    print(
        f"Conclusion: Lance uses {lance_ratio * 100:.2f}% of the ATIF space, "
        f"saving {space_saved:.2f}% (ATIF/Lance={compression_ratio:.2f}x)."
    )
else:
    print(
        f"Conclusion: Lance uses {lance_ratio * 100:.2f}% of the ATIF space, "
        f"an increase of {-space_saved:.2f}% (ATIF/Lance={compression_ratio:.2f}x)."
    )
print(
    "RESULT benchmark=storage "
    f"atif_bytes={atif_bytes} lance_bytes={lance_bytes} "
    f"lance_over_atif={lance_ratio:.4f} "
    f"space_saved_pct={space_saved:.2f} "
    f"atif_over_lance={compression_ratio:.2f}"
)
PY
