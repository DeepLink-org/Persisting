#!/usr/bin/env bash
set -euo pipefail

# Keep shell xtrace separate from captured curl stdout/stderr so assertions see
# only the real HTTP result while users still see the command being executed.
exec 9>&2

trace_exec() {
  BASH_XTRACEFD=9 bash -x -c 'exec "$@"' bash "$@"
}

case_name=${1:?usage: curl-checks.sh CASE URL [URL]}
shift

: "${HTTP_PROXY:?pVisor did not inject HTTP_PROXY}"

curl_success() {
  url=$1
  expected_body=$2
  body=$(trace_exec curl --fail --silent --show-error \
    --proxy "$HTTP_PROXY" --noproxy "" "$url")
  printf '%s\n' "$body"
  test "$body" = "$expected_body"
  printf 'CURL TEST RESULT: PASS (exit=0, expected body=%s)\n' "$expected_body"
}

curl_denied() {
  url=$1
  expected_reason=$2
  set +e
  output=$(trace_exec curl --fail-with-body --silent --show-error \
    --write-out "\nHTTP %{http_code}\n" \
    --proxy "$HTTP_PROXY" --noproxy "" "$url" 2>&1)
  curl_status=$?
  set -e
  printf '%s\n' "$output"
  printf 'curl exit: %s\n' "$curl_status"
  test "$curl_status" = "22"
  case "$output" in *"($expected_reason)"*) ;; *) return 1 ;; esac
  case "$output" in *"HTTP 403"*) ;; *) return 1 ;; esac
  printf 'CURL TEST RESULT: PASS (expected deny, exit=22, HTTP=403, reason=%s)\n' \
    "$expected_reason"
}

case "$case_name" in
  allowlist)
    allowed_url=${1:?missing allowed URL}
    denied_url=${2:?missing denied URL}
    curl_success "$allowed_url" allowed
    echo
    curl_denied "$denied_url" port-not-allowed
    ;;
  explicit-deny)
    url=${1:?missing URL}
    curl_denied "$url" explicit-deny
    ;;
  structured-policy)
    url=${1:?missing payload URL}
    set -- $(trace_exec curl --silent --show-error --output /dev/null \
      --write-out "%{time_total} %{size_download}" \
      --proxy "$HTTP_PROXY" --noproxy "" "$url")
    seconds=$1
    bytes=$2
    printf 'downloaded %s bytes in %ss\n' "$bytes" "$seconds"
    test "$bytes" = "4096"
    awk -v seconds="$seconds" 'BEGIN { exit !(seconds >= 0.8) }'
    printf 'CURL TEST RESULT: PASS (exit=0, bytes=%s, elapsed=%ss, limit=4000 B/s)\n' \
      "$bytes" "$seconds"
    ;;
  deny-all)
    url=${1:?missing URL}
    curl_denied "$url" no-network
    echo
    body=$(trace_exec curl --fail --silent --show-error --noproxy "*" "$url")
    printf '%s\n' "$body"
    test "$body" = "allowed"
    printf 'CURL TEST RESULT: PASS (direct socket bypassed the cooperative proxy)\n'
    ;;
  *)
    printf 'unknown curl check case: %s\n' "$case_name" >&2
    exit 2
    ;;
esac
