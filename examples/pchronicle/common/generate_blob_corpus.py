#!/usr/bin/env python3
"""Generate a deterministic ATIF corpus with large cross-trajectory blobs."""

import base64
import hashlib
import json
import os
import sys
from pathlib import Path


def positive_env(name: str, default: int) -> int:
    value = int(os.environ.get(name, default))
    if value <= 0:
        raise SystemExit(f"{name} must be greater than zero")
    return value


def payload(seed: int, identity: int, size: int) -> str:
    prefix = f"shared-blob-{identity:04d}:"
    output = bytearray()
    counter = 0
    while len(output) < size:
        output.extend(hashlib.sha256(f"{seed}:{identity}:{counter}".encode()).digest())
        counter += 1
    encoded = base64.urlsafe_b64encode(bytes(output)).decode()
    return (prefix + encoded)[:size]


if len(sys.argv) != 3:
    raise SystemExit("usage: generate_blob_corpus.py OUTPUT.ndjson STATS.json")

output_path = Path(sys.argv[1])
stats_path = Path(sys.argv[2])
trajectories = positive_env("PCHRONICLE_BLOB_TRAJECTORIES", 64)
steps_per_trajectory = positive_env("PCHRONICLE_BLOB_STEPS", 8)
unique_blobs = positive_env("PCHRONICLE_BLOB_UNIQUE", 8)
blob_bytes = positive_env("PCHRONICLE_BLOB_BYTES", 32 * 1024)
seed = int(os.environ.get("PCHRONICLE_BLOB_SEED", "20260810"))
blobs = [payload(seed, identity, blob_bytes) for identity in range(unique_blobs)]

logical_blob_bytes = 0
references = 0
output_path.parent.mkdir(parents=True, exist_ok=True)
with output_path.open("w", encoding="utf-8") as output:
    for trajectory_index in range(trajectories):
        steps = []
        for step_index in range(steps_per_trajectory):
            blob = blobs[(trajectory_index * steps_per_trajectory + step_index) % unique_blobs]
            serialized_blob_bytes = len(
                json.dumps(blob, ensure_ascii=False, separators=(",", ":")).encode()
            )
            # These four ATIF fields become four independent JSON content
            # columns in steps.lance. objects.lance can deduplicate the same
            # serialized value across columns; column-local Lance compression
            # cannot share bytes between those columns.
            logical_blob_bytes += serialized_blob_bytes * 4
            references += 4
            steps.append(
                {
                    "step_id": step_index + 1,
                    "source": "agent" if step_index % 2 else "user",
                    "message": blob,
                    "reasoning_effort": blob,
                    "metrics": blob,
                    "extra": blob,
                }
            )
        session_id = f"blob-session-{trajectory_index:06d}"
        document = {
            "schema_version": "1.0",
            "session_id": session_id,
            "trajectory_id": session_id,
            "agent": {"name": "blob-example", "version": "1.0"},
            "steps": steps,
        }
        output.write(json.dumps(document, ensure_ascii=False, separators=(",", ":")))
        output.write("\n")

unique_blob_json_bytes = sum(
    len(json.dumps(blob, ensure_ascii=False, separators=(",", ":")).encode()) for blob in blobs
)
stats = {
    "seed": seed,
    "trajectories": trajectories,
    "steps": trajectories * steps_per_trajectory,
    "steps_per_trajectory": steps_per_trajectory,
    "unique_blobs": unique_blobs,
    "blob_bytes": blob_bytes,
    "blob_references": references,
    "logical_blob_bytes": logical_blob_bytes,
    "unique_blob_json_bytes": unique_blob_json_bytes,
    "logical_dedup_ratio": logical_blob_bytes / unique_blob_json_bytes,
}
stats_path.write_text(json.dumps(stats, indent=2) + "\n", encoding="utf-8")
print(
    "generated_blob_corpus "
    f"trajectories={trajectories} steps={stats['steps']} "
    f"unique_blobs={unique_blobs} blob_bytes={blob_bytes} "
    f"logical_blob_bytes={logical_blob_bytes} "
    f"dedup_ratio={stats['logical_dedup_ratio']:.2f}x seed={seed}"
)
