#!/usr/bin/env bash
set -euo pipefail

export PATH="../../../target/release:$PATH"

# Remove generated data and query results from the previous run.
rm -rf .work
mkdir .work

# Build equivalent, source-text-diversified ATIF and Lance inputs.
example_scale="${PCHRONICLE_EXAMPLE_SCALE:-64}"
python3 ../common/generate_atif.py ../../../crates/persisting-pchronicle/tests/fixtures/atif \
  .work/trajectories.ndjson "$example_scale"
ppilot chronicle import .work/trajectories.ndjson .work/lance

# Run and time the same cold CLI queries against both storage formats.
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
  .work/atif-group.jsonl .work/lance-group.jsonl \
  "$query_iterations" "$group_sql")
selective_metrics=$(python3 ../common/compare_ppilot_query.py \
  .work/trajectories.ndjson .work/lance \
  .work/atif-selective.jsonl .work/lance-selective.jsonl \
  "$query_iterations" "$selective_sql")

# Print both result sets and show that they are identical.
echo 'ATIF GROUP BY result:'
cat .work/atif-group.jsonl

echo 'Lance GROUP BY result:'
cat .work/lance-group.jsonl

echo "Selective result for ${target_session}:"
cat .work/lance-selective.jsonl

group_equal=$(jq -r .equal <<<"$group_metrics")
selective_equal=$(jq -r .equal <<<"$selective_metrics")
if [[ "$group_equal" == true && "$selective_equal" == true ]]; then
  result_rows=$(wc -l < .work/lance-group.jsonl | tr -d ' ')
  total_steps=$(jq -s 'map(.steps) | add // 0' .work/lance-group.jsonl)
  selective_rows=$(wc -l < .work/lance-selective.jsonl | tr -d ' ')
  group_atif_ms=$(jq -r .atif_ms <<<"$group_metrics")
  group_lance_ms=$(jq -r .lance_ms <<<"$group_metrics")
  group_winner=$(jq -r .winner <<<"$group_metrics")
  group_speedup=$(jq -r .speedup <<<"$group_metrics")
  selective_atif_ms=$(jq -r .atif_ms <<<"$selective_metrics")
  selective_lance_ms=$(jq -r .lance_ms <<<"$selective_metrics")
  selective_winner=$(jq -r .winner <<<"$selective_metrics")
  selective_speedup=$(jq -r .speedup <<<"$selective_metrics")
  echo "Cold pPilot query timing (${query_iterations} alternating runs, mean):"
  echo "  GROUP BY:  ATIF=${group_atif_ms} ms, Lance=${group_lance_ms} ms (${group_winner} ${group_speedup}x faster)"
  echo "  Selective: ATIF=${selective_atif_ms} ms, Lance=${selective_lance_ms} ms (${selective_winner} ${selective_speedup}x faster)"
  echo 'Diff: none for both queries'
  echo "Conclusion: PASS — both backends returned identical GROUP BY and selective results over ${total_steps} steps; Lance benefits are shown separately for full-scan aggregation and selective lookup."
  echo "RESULT benchmark=query_equivalence equal=true rows=${result_rows} steps=${total_steps} selective_rows=${selective_rows} iterations=${query_iterations} group_atif_ms=${group_atif_ms} group_lance_ms=${group_lance_ms} group_winner=${group_winner} group_speedup=${group_speedup} selective_atif_ms=${selective_atif_ms} selective_lance_ms=${selective_lance_ms} selective_winner=${selective_winner} selective_speedup=${selective_speedup}"
else
  diff -u .work/atif-group.jsonl .work/lance-group.jsonl || true
  diff -u .work/atif-selective.jsonl .work/lance-selective.jsonl || true
  echo 'Conclusion: FAIL — Lance and ATIF returned different results for at least one query.'
  echo 'RESULT benchmark=query_equivalence equal=false'
  exit 1
fi
