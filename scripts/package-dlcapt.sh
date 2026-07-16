#!/usr/bin/env bash
# Build persisting-dlcapt release binary and pack a deploy tarball.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
CARGO_SH="${SCRIPT_DIR}/cargo-with-protoc.sh"
CRATE_DIR="${ROOT}/crates/persisting-dlcapt"
OUT_DIR="${ROOT}/target/dlcapt"
INCREMENTAL=0
NO_BUILD=0
TARGET=""

usage() {
  cat <<'EOF'
Usage: package-dlcapt.sh [OPTIONS]

Options:
  -i, --incremental     Skip cargo clean
  --no-build            Repackage existing release binary
  --target <triple>     Pass --target to cargo build (optional)
  -h, --help            Show help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -i|--incremental)
      INCREMENTAL=1
      shift
      ;;
    --no-build)
      NO_BUILD=1
      shift
      ;;
    --target)
      if [[ $# -lt 2 || -z "$2" ]]; then
        echo "FAIL: --target requires a target triple" >&2
        usage >&2
        exit 2
      fi
      TARGET="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

cd "${ROOT}"
mkdir -p "${OUT_DIR}"

host_target="$(rustc -vV | awk '/^host:/{print $2}')"
build_target="${TARGET:-${host_target}}"
metadata="$(cargo metadata --no-deps --format-version 1 --locked)"
version="$(jq -r '.packages[] | select(.name=="persisting-dlcapt") | .version' <<<"${metadata}")"
cargo_target_dir="$(jq -r '.target_directory' <<<"${metadata}")"
archive="${OUT_DIR}/dlcapt-${version}-${build_target}.tar.gz"
archive_file="$(basename "${archive}")"
stage="${OUT_DIR}/dlcapt-deploy"

if [[ "${NO_BUILD}" -eq 0 ]]; then
  if [[ "${INCREMENTAL}" -eq 0 ]]; then
    "${CARGO_SH}" clean -p persisting-dlcapt
  fi
  build_args=(-p persisting-dlcapt --release --locked)
  if [[ -n "${TARGET}" ]]; then
    build_args+=(--target "${TARGET}")
  fi
  "${CARGO_SH}" build "${build_args[@]}"
fi

if [[ -n "${TARGET}" ]]; then
  binary="${cargo_target_dir}/${TARGET}/release/dlcapt"
else
  binary="${cargo_target_dir}/release/dlcapt"
fi
if [[ ! -x "${binary}" ]]; then
  echo "FAIL: missing binary ${binary}" >&2
  exit 1
fi

rm -rf "${stage}" "${archive}" "${archive}.sha256"
mkdir -p "${stage}/bin" "${stage}/config" "${stage}/var/store"
cp "${binary}" "${stage}/bin/dlcapt"
chmod +x "${stage}/bin/dlcapt"
cp "${CRATE_DIR}"/config/*.example.toml "${stage}/config/"
cp "${CRATE_DIR}/config/proxy.lance-s3.deploy.example.toml" \
  "${stage}/config/proxy.lance-s3.deploy.toml"
cp "${CRATE_DIR}/README.md" "${stage}/README.md"
cp "${ROOT}/LICENSE" "${stage}/LICENSE"
cp "${ROOT}/NOTICE" "${stage}/NOTICE"

# Refuse private online/beta configs if someone drops them into config/
if compgen -G "${stage}/config/*online*" >/dev/null \
  || compgen -G "${stage}/config/*beta*" >/dev/null; then
  echo "FAIL: online/beta config leaked into archive staging" >&2
  exit 1
fi

tar -czf "${archive}" -C "${OUT_DIR}" dlcapt-deploy
"${SCRIPT_DIR}/validate-dlcapt-archive.sh" "${archive}"
(
  cd "${OUT_DIR}"
  sha256sum "${archive_file}" > "${archive_file}.sha256"
)

echo "OK archive:  ${archive}"
echo "OK checksum: ${archive}.sha256"
echo "OK binary:   ${binary}"
