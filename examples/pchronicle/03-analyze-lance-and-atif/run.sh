#!/usr/bin/env bash
set -euo pipefail

export PATH="../../../target/release:$PATH"

# Remove generated data and query results from the previous run.
rm -rf .work
mkdir .work

# Build equivalent, source-text-diversified raw JSON and Lance inputs.
example_scale="${PCHRONICLE_EXAMPLE_SCALE:-64}"
python3 ../common/generate_atif.py ../../../crates/persisting-pchronicle/tests/fixtures/atif \
  .work/trajectories.ndjson "$example_scale"
ppilot chronicle import .work/trajectories.ndjson .work/lance

# Run the same logical queries through the raw Python baseline and both
# pChronicle paths. Each measured invocation starts a fresh process.
query_iterations="${PCHRONICLE_QUERY_ITERS:-10}"
group_sql='SELECT source, COUNT(*) AS steps FROM steps GROUP BY source ORDER BY source'
target_session=$(python3 - .work/trajectories.ndjson <<'PY'
import json
import sys

documents = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
_, target = max(enumerate(documents), key=lambda item: (len(item[1]["steps"]), item[0]))
print(target["session_id"])
PY
)
selective_sql="SELECT step_id, source FROM steps WHERE session_id = '${target_session}' AND step_id BETWEEN 5 AND 15 ORDER BY step_id"

group_metrics=$(python3 ../common/compare_ppilot_query.py \
  .work/trajectories.ndjson .work/lance \
  .work/python-group.jsonl .work/pchronicle-json-group.jsonl \
  .work/pchronicle-lance-group.jsonl \
  "$query_iterations" group "$group_sql")
selective_metrics=$(python3 ../common/compare_ppilot_query.py \
  .work/trajectories.ndjson .work/lance \
  .work/python-selective.jsonl .work/pchronicle-json-selective.jsonl \
  .work/pchronicle-lance-selective.jsonl \
  "$query_iterations" selective "$selective_sql" "$target_session")

# Print all result sets and show that they are semantically identical.
echo 'Python json.loads GROUP BY baseline:'
cat .work/python-group.jsonl

echo 'pChronicle direct-JSON GROUP BY result:'
cat .work/pchronicle-json-group.jsonl

echo 'pChronicle Lance GROUP BY result:'
cat .work/pchronicle-lance-group.jsonl

echo "Selective result for ${target_session}:"
cat .work/pchronicle-lance-selective.jsonl

group_equal=$(jq -r .equal <<<"$group_metrics")
selective_equal=$(jq -r .equal <<<"$selective_metrics")
if [[ "$group_equal" == true && "$selective_equal" == true ]]; then
  result_rows=$(wc -l < .work/pchronicle-lance-group.jsonl | tr -d ' ')
  total_steps=$(jq -s 'map(.steps) | add // 0' .work/pchronicle-lance-group.jsonl)
  selective_rows=$(wc -l < .work/pchronicle-lance-selective.jsonl | tr -d ' ')
  group_python_ms=$(jq -r .python_json.median_ms <<<"$group_metrics")
  group_python_p95_ms=$(jq -r .python_json.p95_ms <<<"$group_metrics")
  group_json_ms=$(jq -r .pchronicle_json.median_ms <<<"$group_metrics")
  group_json_p95_ms=$(jq -r .pchronicle_json.p95_ms <<<"$group_metrics")
  group_lance_ms=$(jq -r .pchronicle_lance.median_ms <<<"$group_metrics")
  group_lance_p95_ms=$(jq -r .pchronicle_lance.p95_ms <<<"$group_metrics")
  group_json_relative=$(jq -r .pchronicle_json.speedup_vs_python <<<"$group_metrics")
  group_lance_relative=$(jq -r .pchronicle_lance.speedup_vs_python <<<"$group_metrics")
  selective_python_ms=$(jq -r .python_json.median_ms <<<"$selective_metrics")
  selective_python_p95_ms=$(jq -r .python_json.p95_ms <<<"$selective_metrics")
  selective_json_ms=$(jq -r .pchronicle_json.median_ms <<<"$selective_metrics")
  selective_json_p95_ms=$(jq -r .pchronicle_json.p95_ms <<<"$selective_metrics")
  selective_lance_ms=$(jq -r .pchronicle_lance.median_ms <<<"$selective_metrics")
  selective_lance_p95_ms=$(jq -r .pchronicle_lance.p95_ms <<<"$selective_metrics")
  selective_json_relative=$(jq -r .pchronicle_json.speedup_vs_python <<<"$selective_metrics")
  selective_lance_relative=$(jq -r .pchronicle_lance.speedup_vs_python <<<"$selective_metrics")
  echo "Cold-process timing (${query_iterations} rotated runs, median):"
  echo "  GROUP BY:  Python=${group_python_ms}/${group_python_p95_ms} ms; pChronicle JSON=${group_json_ms}/${group_json_p95_ms} ms (${group_json_relative}x vs baseline); pChronicle Lance=${group_lance_ms}/${group_lance_p95_ms} ms (${group_lance_relative}x vs baseline)"
  echo "  Selective: Python=${selective_python_ms}/${selective_python_p95_ms} ms; pChronicle JSON=${selective_json_ms}/${selective_json_p95_ms} ms (${selective_json_relative}x vs baseline); pChronicle Lance=${selective_lance_ms}/${selective_lance_p95_ms} ms (${selective_lance_relative}x vs baseline)"
  echo '  Values are median/p95; relative throughput uses medians.'
  echo 'Semantic diff: none for both queries'
  echo "Conclusion: PASS — raw Python parsing and both pChronicle paths returned the same results over ${total_steps} steps. Ratios are always Python median / measured-path median; values above 1 mean the pChronicle path is faster."
  echo "RESULT benchmark=query_equivalence baseline=python_json equal=true rows=${result_rows} steps=${total_steps} selective_rows=${selective_rows} iterations=${query_iterations} group_python_ms=${group_python_ms} group_python_p95_ms=${group_python_p95_ms} group_pchronicle_json_ms=${group_json_ms} group_pchronicle_json_p95_ms=${group_json_p95_ms} group_pchronicle_lance_ms=${group_lance_ms} group_pchronicle_lance_p95_ms=${group_lance_p95_ms} group_json_vs_python=${group_json_relative} group_lance_vs_python=${group_lance_relative} selective_python_ms=${selective_python_ms} selective_python_p95_ms=${selective_python_p95_ms} selective_pchronicle_json_ms=${selective_json_ms} selective_pchronicle_json_p95_ms=${selective_json_p95_ms} selective_pchronicle_lance_ms=${selective_lance_ms} selective_pchronicle_lance_p95_ms=${selective_lance_p95_ms} selective_json_vs_python=${selective_json_relative} selective_lance_vs_python=${selective_lance_relative}"
else
  diff -u .work/python-group.jsonl .work/pchronicle-json-group.jsonl || true
  diff -u .work/python-group.jsonl .work/pchronicle-lance-group.jsonl || true
  diff -u .work/python-selective.jsonl .work/pchronicle-json-selective.jsonl || true
  diff -u .work/python-selective.jsonl .work/pchronicle-lance-selective.jsonl || true
  echo 'Conclusion: FAIL — at least one pChronicle result differs from the Python JSON baseline.'
  echo 'RESULT benchmark=query_equivalence baseline=python_json equal=false'
  exit 1
fi
