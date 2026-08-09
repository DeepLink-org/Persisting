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
print(f"  Raw ATIF JSONL baseline:   {atif_bytes:>10} bytes ({human_bytes(atif_bytes)})")
print(f"  pChronicle Lance store:    {lance_bytes:>10} bytes ({human_bytes(lance_bytes)})")
if lance_bytes <= atif_bytes:
    print(
        f"Conclusion: pChronicle Lance uses {lance_ratio * 100:.2f}% of the raw JSON "
        f"baseline, saving {space_saved:.2f}% (JSON/Lance={compression_ratio:.2f}x)."
    )
else:
    print(
        f"Conclusion: pChronicle Lance uses {lance_ratio * 100:.2f}% of the raw JSON "
        f"baseline, an increase of {-space_saved:.2f}% "
        f"(JSON/Lance={compression_ratio:.2f}x)."
    )
print(
    "RESULT benchmark=storage baseline=raw_json "
    f"baseline_json_bytes={atif_bytes} pchronicle_lance_bytes={lance_bytes} "
    f"lance_over_json={lance_ratio:.4f} "
    f"space_saved_pct={space_saved:.2f} "
    f"json_over_lance={compression_ratio:.2f}"
)
PY
