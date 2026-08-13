#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$example_dir/../../.." && pwd)"
cd "$example_dir"
pchronicle="${PCHRONICLE_BIN:-$repo_root/target/release/pchronicle}"
input="$repo_root/examples/data/atif/support-ticket.json"

rm -rf .work
mkdir -p .work

"$pchronicle" import --from "$input" --output .work/atif --format atif
"$pchronicle" export --from .work/atif --output .work/restored.json --format atif --strict

jq --sort-keys . "$input" > .work/input.normalized.json
jq --sort-keys . .work/restored.json > .work/restored.normalized.json
cmp .work/input.normalized.json .work/restored.normalized.json
echo "PASS: strict ATIF roundtrip is byte-identical after canonical JSON formatting"
