#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [ -z "${TMPDIR:-}" ]; then
  export TMPDIR="${ROOT}/.build-tmp"
  export TEMP="${TMPDIR}"
  export TMP="${TMPDIR}"
  mkdir -p "${TMPDIR}"
fi

if [ -z "${PROTOC:-}" ]; then
  if command -v protoc >/dev/null 2>&1; then
    export PROTOC="$(command -v protoc)"
  else
    PROTOC=$(
      find "${CARGO_HOME:-$HOME/.cargo}/registry/src" \
        -path '*/protoc-bin-vendored-*/bin/protoc' 2>/dev/null \
        | sort -V \
        | tail -1 \
        || true
    )
    if [ -n "${PROTOC:-}" ]; then
      export PROTOC
    else
      echo "PROTOC not found. Install protobuf-compiler or run: cargo fetch" >&2
      exit 1
    fi
  fi
fi

exec cargo "$@"
