#!/usr/bin/env bash
set -euo pipefail

export PATH="../../../target/release:$PATH"

rm -rf .work
mkdir .work

bench_iters="${PCHRONICLE_BLOB_BENCH_ITERS:-10}"
python3 ../common/generate_blob_corpus.py .work/corpus.ndjson .work/corpus-stats.json

# A threshold larger than any generated value keeps the comparison store inline.
ppilot chronicle import .work/corpus.ndjson .work/inline \
  --content-offload-threshold 1000000000

# The production default is 64 KiB. This example uses an explicit 4 KiB threshold
# so its 32 KiB fixtures exercise objects.lance without creating a huge corpus.
ppilot chronicle import .work/corpus.ndjson .work/offloaded \
  --content-offload-threshold 4096 \
  --content-preview-bytes 256 \
  --content-zstd-level 3

python3 ../common/benchmark_blob_offload.py \
  .work/inline .work/offloaded .work/corpus-stats.json "$bench_iters" \
  | tee .work/output.txt
