#!/usr/bin/env bash
set -euo pipefail

export PATH="../../../target/release:$PATH"

if ! command -v ppilot >/dev/null 2>&1; then
    cargo build --release -q --manifest-path ../../../Cargo.toml \
        -p persisting-ppilot --features cli --bin ppilot
fi

rm -rf .work
mkdir -p .work/openai/batch .work/actf/bench

fixture_root="../../../crates/persisting-pchronicle/tests/fixtures/import_roundtrip"
cp "$fixture_root/cybergym_07270003_trimmed.json" .work/openai/root.json
cp "$fixture_root/cybergym_0729001_trimmed.json" .work/openai/batch/cybergym.json
printf '%s\n' 'not-json' > .work/openai/unmatched.json
cp "$fixture_root/make-doom-for-mips_trimmed.actf.json" .work/actf/bench/doom.actf.json
cp "$fixture_root/protein-assembly_trimmed.actf.json" .work/actf/bench/protein.actf.json

echo 'Querying the OpenAI directory with auto detection and a path wildcard...'
ppilot query sql .work/openai \
    --sql "SELECT _file_, COUNT(*) AS steps
           FROM steps
           WHERE _file_ LIKE 'batch/%'
           GROUP BY _file_
           ORDER BY _file_" \
    > .work/openai-query.jsonl
jq -e -s \
    'length == 1 and .[0]._file_ == "batch/cybergym.json" and .[0].steps == 3' \
    .work/openai-query.jsonl >/dev/null

echo 'Querying the ACTF directory with an explicit source and filename wildcard...'
ppilot query sql .work/actf --source actf \
    --sql "SELECT session_id, _file_
           FROM runs
           WHERE _file_ LIKE '%protein%'
           ORDER BY session_id" \
    > .work/actf-query.jsonl
jq -e -s \
    'length == 1 and .[0]._file_ == "bench/protein.actf.json"' \
    .work/actf-query.jsonl >/dev/null

ppilot query sql .work/openai --sql 'DESCRIBE runs' \
    > .work/direct-runs-schema.jsonl
jq -e -s 'any(.[]; .column_name == "_file_" and .data_type == "Utf8")' \
    .work/direct-runs-schema.jsonl >/dev/null

echo 'Converting one OpenAI file to Lance and checking its physical schema...'
ppilot convert .work/openai/root.json .work/lance --to lance
ppilot query sql .work/lance --sql 'DESCRIBE runs' \
    > .work/lance-runs-schema.jsonl
jq -e -s 'all(.[]; .column_name != "_file_")' \
    .work/lance-runs-schema.jsonl >/dev/null

echo
echo 'OpenAI wildcard result:'
sed 's/^/  /' .work/openai-query.jsonl
echo 'ACTF wildcard result:'
sed 's/^/  /' .work/actf-query.jsonl
echo
echo 'Conclusion: PASS — OpenAI/ACTF directories are directly queryable by relative _file_ path, while the Lance runs schema remains unchanged.'
echo 'RESULT example=direct-format-query openai_auto=true actf_query=true unmatched_pruned=true file_column=true lance_schema_unchanged=true'
