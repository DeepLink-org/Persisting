# Shared helpers for capture integration scripts. Source from repo root context.
# shellcheck shell=bash

capture_repo_root() {
  (cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
}

capture_cargo_target_dir() {
  (cd "$REPO_ROOT" && cargo metadata --format-version 1 --no-deps \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
}

capture_engine_lib_name() {
  case "$(uname -s)" in
    Darwin) echo "libpersisting_engine.dylib" ;;
    MINGW*|MSYS*|CYGWIN*) echo "persisting_engine.dll" ;;
    *) echo "libpersisting_engine.so" ;;
  esac
}

capture_pick_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'
}

capture_parse_toml_row_count() {
  python3 -c "import re,sys; t=open(sys.argv[1]).read(); m=re.search(r'row_count\s*=\s*(\d+)', t); print(m.group(1) if m else '0')" "$1"
}

# Sets CLI and optionally PERSISTING_ENGINE_LIB. Args: skip_build need_engine
capture_resolve_binaries() {
  local skip_build="${1:-0}"
  local need_engine="${2:-0}"
  local profile="${PERSISTING_BUILD_PROFILE:-debug}"

  TARGET_DIR="${CARGO_TARGET_DIR:-$(capture_cargo_target_dir)}"
  ENGINE_NAME="$(capture_engine_lib_name)"

  if [[ "$skip_build" != "1" ]]; then
    if [[ "$need_engine" == "1" ]]; then
      echo "==> cargo build -p persisting-cli -p persisting-engine"
      (cd "$REPO_ROOT" && cargo build -p persisting-cli -p persisting-engine)
    else
      echo "==> cargo build -p persisting-cli"
      (cd "$REPO_ROOT" && cargo build -p persisting-cli)
    fi
  fi

  if [[ -n "${PERSISTING_CLI:-}" ]]; then
    CLI="$PERSISTING_CLI"
  else
    CLI="$TARGET_DIR/$profile/persisting"
  fi
  [[ -x "$CLI" ]] || capture_die "not executable: $CLI"

  if [[ -n "${PERSISTING_ENGINE_LIB:-}" ]]; then
    export PERSISTING_ENGINE_LIB
  elif [[ "$need_engine" == "1" ]]; then
    local lib="$TARGET_DIR/$profile/$ENGINE_NAME"
    [[ -f "$lib" ]] || capture_die "missing $lib (build persisting-engine or set PERSISTING_ENGINE_LIB)"
    export PERSISTING_ENGINE_LIB="$lib"
  fi
}

capture_wait_http() {
  local url="$1" tries="${2:-80}"
  local i=0
  while [[ $i -lt $tries ]]; do
    curl -sf "$url" >/dev/null 2>&1 && return 0
    sleep 0.15
    i=$((i + 1))
  done
  return 1
}

# Poll trajectory stats until row_count >= expected or plateau.
capture_drain_lance_rows() {
  local storage="$1" agent_id="$2" session_id="$3" expected="$4" drain_sec="$5"
  local stats_toml="$6"
  local best=0 prev=-1 stable=0
  local i max=$((drain_sec * 2))

  for ((i = 0; i < max; i++)); do
    if "$CLI" trajectory stats "$storage" \
      --agent-id "$agent_id" \
      --session-id "$session_id" \
      --storage-format lance >"$stats_toml" 2>/dev/null; then
      best="$(capture_parse_toml_row_count "$stats_toml")"
      if [[ "$best" -ge "$expected" ]]; then
        echo "$best"
        return 0
      fi
      if [[ "$best" -eq "$prev" && "$best" -gt 0 ]]; then
        stable=$((stable + 1))
        if [[ "$stable" -ge 6 ]]; then
          echo "$best"
          return 0
        fi
      else
        stable=0
      fi
      prev=$best
    fi
    sleep 0.5
  done
  echo "$best"
  return 1
}

capture_die() { echo "error: $*" >&2; exit 1; }
