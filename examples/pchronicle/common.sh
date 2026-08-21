#!/usr/bin/env bash

pchronicle_example_init() {
  local example_dir="$1"

  mkdir -p "$example_dir/.work"
  PCHRONICLE_EXAMPLE_RUN_DIR="$(mktemp -d "$example_dir/.work/run.XXXXXX")"
}

pchronicle_capture() {
  local label="$1"
  shift

  local stdout_log="$PCHRONICLE_EXAMPLE_RUN_DIR/$label.stdout.log"
  local stderr_log="$PCHRONICLE_EXAMPLE_RUN_DIR/$label.stderr.log"
  local status

  if "$@" >"$stdout_log" 2>"$stderr_log"; then
    cat "$stdout_log"
    return 0
  else
    status=$?
  fi

  printf 'FAIL: command %s exited with status %s\n' "$label" "$status" >&2
  if [[ -s "$stderr_log" ]]; then
    cat "$stderr_log" >&2
  fi
  return "$status"
}

pchronicle_report_start() {
  printf '\n%s\n' "$1"
}

pchronicle_report_item() {
  printf '  %-12s %s\n' "$1" "$2"
}

pchronicle_human_bytes() {
  awk -v bytes="$1" 'BEGIN {
    if (bytes >= 1048576) {
      printf "%.1f MiB", bytes / 1048576
    } else if (bytes >= 1024) {
      printf "%.1f KiB", bytes / 1024
    } else {
      printf "%d B", bytes
    }
  }'
}

pchronicle_relative_performance() {
  local subject="$1"
  local baseline="$2"
  local baseline_over_subject="$3"

  awk \
    -v subject="$subject" \
    -v baseline="$baseline" \
    -v ratio="$baseline_over_subject" \
    'BEGIN {
      if (ratio > 1.01) {
        printf "%s is %.2fx faster than %s", subject, ratio, baseline
      } else if (ratio > 0 && ratio < 0.99) {
        printf "%s is %.2fx slower than %s", subject, 1 / ratio, baseline
      } else {
        printf "%s and %s have comparable elapsed time", subject, baseline
      }
    }'
}

pchronicle_storage_comparison() {
  local json_bytes="$1"
  local lance_bytes="$2"

  awk -v json_bytes="$json_bytes" -v lance_bytes="$lance_bytes" 'BEGIN {
    ratio = lance_bytes / json_bytes
    if (ratio <= 1) {
      printf "Lance uses %.2f%% of JSON space (%.2f%% smaller)", \
        ratio * 100, (1 - ratio) * 100
    } else {
      printf "Lance uses %.2f%% of JSON space (%.2f%% storage overhead)", \
        ratio * 100, (ratio - 1) * 100
    }
  }'
}

pchronicle_show_raw() {
  local stdout_log
  local stderr_log
  local label

  if [[ "${PCHRONICLE_EXAMPLE_VERBOSE:-0}" != "1" ]]; then
    return 0
  fi

  printf '\nRaw command output\n'
  for stdout_log in "$PCHRONICLE_EXAMPLE_RUN_DIR"/*.stdout.log; do
    [[ -e "$stdout_log" ]] || continue
    label="${stdout_log##*/}"
    label="${label%.stdout.log}"
    stderr_log="$PCHRONICLE_EXAMPLE_RUN_DIR/$label.stderr.log"

    printf '\n--- %s stdout ---\n' "$label"
    if [[ -s "$stdout_log" ]]; then
      cat "$stdout_log"
    else
      printf '(empty)\n'
    fi
    printf '%s\n' "--- $label stderr ---"
    if [[ -s "$stderr_log" ]]; then
      cat "$stderr_log"
    else
      printf '(empty)\n'
    fi
  done
}

pchronicle_report_finish() {
  local message="$1"

  pchronicle_report_item "Artifacts" "$PCHRONICLE_EXAMPLE_RUN_DIR"
  pchronicle_show_raw
  printf '\nPASS: %s\n' "$message"
}
