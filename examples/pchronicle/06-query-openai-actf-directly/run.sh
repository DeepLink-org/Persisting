#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$example_dir/../../.." && pwd)"
cd "$example_dir"
pchronicle="${PCHRONICLE_BIN:-$repo_root/target/release/pchronicle}"
data="$repo_root/examples/data"

"$pchronicle" query "$data/openai-messages" \
  "SELECT session_id, COUNT(*) AS steps FROM dataset.steps GROUP BY session_id ORDER BY session_id" \
  --format jsonl

"$pchronicle" query "$data/actf" \
  "SELECT session_id, agent_id FROM dataset.runs ORDER BY session_id" \
  --format jsonl
