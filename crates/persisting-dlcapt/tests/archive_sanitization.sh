#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
validator="${ROOT}/scripts/validate-dlcapt-archive.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

make_archive() {
  local archive="$1"
  shift
  local stage="${tmp}/dlcapt-deploy"
  rm -rf "${stage}"
  mkdir -p "${stage}/config"
  cat > "${stage}/README.md" <<'EOF'
# dlcapt

Use https://example.invalid/v1 and synthetic S3 placeholders only.
EOF
  cat > "${stage}/config/proxy.example.toml" <<'EOF'
api_key = ""
bucket = "example-bucket"
endpoint = "https://s3.example.invalid"
EOF
  while [[ $# -gt 0 ]]; do
    local path="$1"
    local content="$2"
    mkdir -p "$(dirname "${stage}/${path}")"
    printf '%s\n' "${content}" > "${stage}/${path}"
    shift 2
  done
  tar -czf "${archive}" -C "${tmp}" dlcapt-deploy
}

clean_archive="${tmp}/clean.tar.gz"
make_archive "${clean_archive}" \
  config/empty.json '{"api_key":""}' \
  config/empty.yaml '"api_key": ""' \
  config/online-example/proxy.toml 'api_key = ""'
"${validator}" "${clean_archive}"

unsafe_content_archive="${tmp}/unsafe-content.tar.gz"
make_archive "${unsafe_content_archive}" \
  README.md $'bind = "0.0.0.0"\nsite = "wind-tunnel"\nproject = "ailab-pj"\nmount = "ssd1"\nlab = "pjlab"\nservice = "pjh-service"\napi_key = "not-empty"\ntoken = "not-empty"\npassword = "not-empty"\nsecret = "not-empty"\nAWS_ACCESS_KEY_ID = "AKIAIOSFODNN7EXAMPLE"\n-----BEGIN PRIVATE KEY-----'
if "${validator}" "${unsafe_content_archive}"; then
  echo "FAIL: archive with forbidden public text passed validation" >&2
  exit 1
fi

unsafe_name_archive="${tmp}/unsafe-name.tar.gz"
make_archive "${unsafe_name_archive}" \
  config/proxy-online.toml 'api_key = ""'
if "${validator}" "${unsafe_name_archive}"; then
  echo "FAIL: archive with private online config name passed validation" >&2
  exit 1
fi

unsafe_json_credential_archive="${tmp}/unsafe-json-credential.tar.gz"
make_archive "${unsafe_json_credential_archive}" \
  config/proxy.json '{"api_key":"leaked-secret"}'
if "${validator}" "${unsafe_json_credential_archive}"; then
  echo "FAIL: archive with JSON credential passed validation" >&2
  exit 1
fi

unsafe_yaml_credential_archive="${tmp}/unsafe-yaml-credential.tar.gz"
make_archive "${unsafe_yaml_credential_archive}" \
  config/proxy.yaml '"token": "leaked-secret"'
if "${validator}" "${unsafe_yaml_credential_archive}"; then
  echo "FAIL: archive with YAML credential passed validation" >&2
  exit 1
fi

unsafe_nested_name_archive="${tmp}/unsafe-nested-name.tar.gz"
make_archive "${unsafe_nested_name_archive}" \
  config/nested/proxy-online.toml 'api_key = ""'
if "${validator}" "${unsafe_nested_name_archive}"; then
  echo "FAIL: archive with nested private online config name passed validation" >&2
  exit 1
fi

unsafe_online_component_archive="${tmp}/unsafe-online-component.tar.gz"
make_archive "${unsafe_online_component_archive}" \
  config/online/proxy.toml 'api_key = ""'
if "${validator}" "${unsafe_online_component_archive}"; then
  echo "FAIL: archive with online directory component passed validation" >&2
  exit 1
fi

unsafe_beta_component_archive="${tmp}/unsafe-beta-component.tar.gz"
make_archive "${unsafe_beta_component_archive}" \
  config/beta/nested/proxy.toml 'api_key = ""'
if "${validator}" "${unsafe_beta_component_archive}"; then
  echo "FAIL: archive with beta directory component passed validation" >&2
  exit 1
fi

echo "PASS archive_sanitization.sh"
