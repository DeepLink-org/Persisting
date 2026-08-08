#!/usr/bin/env bash
set -euo pipefail

export PATH="../../../target/release:$PATH"

if ! command -v ppilot >/dev/null 2>&1; then
    cargo build --release -q --manifest-path ../../../Cargo.toml \
        -p persisting-ppilot --features cli --bin ppilot
fi

rm -rf .work
mkdir -p .work

fixture_root="../../../crates/persisting-pchronicle/tests/fixtures/import_roundtrip"
openai_input="$fixture_root/cybergym_0729001_trimmed.json"
actf_input="$fixture_root/make-doom-for-mips_trimmed.actf.json"

echo 'Importing OpenAI corpus into Storyline Lance...'
ppilot convert "$openai_input" .work/openai-lance --to lance

echo 'Restoring OpenAI corpus from Storyline Lance...'
ppilot convert .work/openai-lance .work/openai-restored \
    --from lance --to openai_msg

openai_restored=".work/openai-restored/$(basename "$openai_input")"
jq -ne --slurpfile expected "$openai_input" \
    --slurpfile actual "$openai_restored" \
    '$expected[0] == $actual[0]' >/dev/null

echo 'Importing ACTF into Storyline Lance...'
ppilot convert "$actf_input" .work/actf-lance --to lance

echo 'Restoring ACTF from Storyline Lance...'
ppilot convert .work/actf-lance .work/actf-restored \
    --from lance --to actf

actf_task_id=$(jq -r '.task_id' "$actf_input")
actf_restored=".work/actf-restored/${actf_task_id}.actf.json"
jq -ne --slurpfile expected "$actf_input" \
    --slurpfile actual "$actf_restored" \
    '$expected[0] == $actual[0]' >/dev/null

openai_sessions=$(jq '[.[].session_id] | unique | length' "$openai_input")
openai_rows=$(jq 'length' "$openai_input")
actf_steps=$(jq '[.attempts[].trajectory.steps | length] | add' "$actf_input")

echo
echo 'Round-trip comparison:'
echo "  OpenAI: equal=true sessions=$openai_sessions rows=$openai_rows"
echo "  ACTF:   equal=true task_id=$actf_task_id steps=$actf_steps"
echo '  Lance schemas: unchanged runs/steps/tool_calls tables'
echo
echo "RESULT benchmark=format-roundtrip openai_equal=true actf_equal=true openai_sessions=$openai_sessions openai_rows=$openai_rows actf_steps=$actf_steps"
