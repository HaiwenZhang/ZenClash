#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd "${script_dir}/../.." && pwd)"
test_root="$(mktemp -d)"
[[ "${test_root}" == /* && "${test_root}" != / && -d "${test_root}" && ! -L "${test_root}" ]]
mock_bin="${test_root}/bin"
export MIHOMO_VERSION=v1.19.30
mock_target="${test_root}/target"
app_dir="${test_root}/output/ZenClash.app"
sign_log="${test_root}/codesign.log"
trap 'rm -rf "${test_root}"' EXIT
mkdir -p "${mock_bin}"

cat >"${mock_bin}/uname" <<'EOF'
#!/usr/bin/env bash
[[ "${1:-}" == -m ]]
printf 'arm64\n'
EOF
cat >"${mock_bin}/file" <<'EOF'
#!/usr/bin/env bash
printf '%s: Mach-O 64-bit executable arm64\n' "${1:?}"
EOF
cat >"${mock_bin}/rustup" <<'EOF'
#!/usr/bin/env bash
[[ "$*" == 'target add aarch64-apple-darwin' ]]
EOF
cat >"${mock_bin}/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
output="${CARGO_TARGET_DIR}/aarch64-apple-darwin/release"
mkdir -p "${output}"
case " $* " in
  *' -p zenclash-service '*)
    [[ " $* " == *' --features standalone,client '* ]]
    [[ " $* " == *' --bin zenclash-service '* ]]
    [[ " $* " == *' --target aarch64-apple-darwin '* ]]
    case "${MOCK_SERVICE_FAILURE:-}" in
      missing) exit 0 ;;
      empty) : >"${output}/zenclash-service" ;;
      version) printf '#!/usr/bin/env bash\nexit 1\n' >"${output}/zenclash-service" ;;
      empty_version) printf '#!/usr/bin/env bash\nexit 0\n' >"${output}/zenclash-service" ;;
      whitespace_version) printf '#!/usr/bin/env bash\nprintf " \\t\\n"\n' >"${output}/zenclash-service" ;;
      *) printf '#!/usr/bin/env bash\n[[ "${1:-}" == --version ]]\nprintf "zenclash-service fixture\\n"\n' >"${output}/zenclash-service" ;;
    esac
    chmod +x "${output}/zenclash-service"
    for tool in zenclash-service-install zenclash-service-uninstall; do
      [[ " $* " == *" --bin ${tool} "* ]]
      cp "${output}/zenclash-service" "${output}/${tool}"
    done
    ;;
  *)
    [[ " $* " == *' -p zenclash-ui '* ]]
    printf '#!/usr/bin/env bash\nexit 0\n' >"${output}/zenclash"
    chmod +x "${output}/zenclash"
    ;;
esac
EOF
cat >"${mock_bin}/codesign" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${MOCK_SIGN_LOG}"
# Require actual staged payload even when the bundle itself is the sign target.
if [[ ! -s "${MOCK_APP_DIR}/Contents/MacOS/zenclash-service" || ! -x "${MOCK_APP_DIR}/Contents/MacOS/zenclash-service" ]]; then
  echo 'The staged App is missing an executable service payload' >&2
  exit 1
fi
for tool in zenclash-service-install zenclash-service-uninstall; do
  [[ -s "${MOCK_APP_DIR}/Contents/MacOS/${tool}" && -x "${MOCK_APP_DIR}/Contents/MacOS/${tool}" ]]
done
EOF
cat >"${test_root}/mihomo" <<'EOF'
#!/usr/bin/env bash
[[ "${1:-}" == -v ]]
printf 'Mihomo fixture\n'
EOF
chmod +x "${mock_bin}/"* "${test_root}/mihomo"
printf 'geodata fixture\n' >"${test_root}/geoip.metadb"

run_build() {
  # The fixed native plist utility is replaced only in this shell; the production
  # script is sourced unchanged and all copies/sign targets are real test files.
  PATH="${mock_bin}:${PATH}" \
    CARGO_TARGET_DIR="${mock_target}" \
    ZENCLASH_VERSION=9.8.7 \
    ZENCLASH_OUTPUT_DIR="${test_root}/output" \
    ZENCLASH_MIHOMO_BINARY="${test_root}/mihomo" \
    ZENCLASH_GEODATA_FILE="${test_root}/geoip.metadb" \
    MOCK_SIGN_LOG="${sign_log}" \
    MOCK_APP_DIR="${app_dir}" \
    APPLE_SIGNING_IDENTITY="${1:-}" \
    MOCK_SERVICE_FAILURE="${2:-}" \
    bash -c 'function /usr/libexec/PlistBuddy { [[ "$1" == -c && -f "$3" ]]; }; source "$1"' \
      macos-payload-test "${project_root}/scripts/build_macos_app.sh"
}

assert_payload_and_order() {
  cmp "${mock_target}/aarch64-apple-darwin/release/zenclash-service" \
    "${app_dir}/Contents/MacOS/zenclash-service"
  for tool in zenclash-service-install zenclash-service-uninstall; do
    cmp "${mock_target}/aarch64-apple-darwin/release/${tool}" "${app_dir}/Contents/MacOS/${tool}"
    grep -Fq -- "--verify --strict --verbose=2 ${app_dir}/Contents/MacOS/${tool}" "${sign_log}"
  done
  local helper_sign helper_verify bundle_sign bundle_verify
  helper_sign="$(grep -n -- '--force.*--sign.*Contents/MacOS/zenclash-service$' "${sign_log}" | cut -d: -f1)"
  helper_verify="$(grep -n -- '--verify.*Contents/MacOS/zenclash-service$' "${sign_log}" | cut -d: -f1)"
  bundle_sign="$(grep -n -- '--force.*--sign.*ZenClash.app$' "${sign_log}" | cut -d: -f1)"
  bundle_verify="$(grep -n -- '--verify.*ZenClash.app$' "${sign_log}" | cut -d: -f1)"
  [[ -n "${helper_sign}" && -n "${helper_verify}" && -n "${bundle_sign}" && -n "${bundle_verify}" ]]
  [[ "${helper_sign}" -lt "${helper_verify}" && "${helper_verify}" -lt "${bundle_sign}" && "${bundle_sign}" -lt "${bundle_verify}" ]]
}

run_build
assert_payload_and_order
grep -Fq -- '--sign - ' "${sign_log}"
: >"${sign_log}"
run_build 'Fixture Developer ID'
assert_payload_and_order
grep -Fq -- '--options runtime --timestamp --sign Fixture Developer ID ' "${sign_log}"
cp "${app_dir}/Contents/MacOS/zenclash-service" "${test_root}/accepted-helper"
printf 'preserve existing bundle\n' >"${app_dir}/preserved-marker"

for failure in missing empty version empty_version whitespace_version; do
  rm -f "${mock_target}/aarch64-apple-darwin/release/zenclash-service"
  : >"${sign_log}"
  if run_build '' "${failure}" >"${test_root}/${failure}.log" 2>&1; then
    printf 'macOS packaging unexpectedly accepted %s service payload\n' "${failure}" >&2
    exit 1
  fi
  [[ ! -s "${sign_log}" ]]
  cmp "${test_root}/accepted-helper" "${app_dir}/Contents/MacOS/zenclash-service"
  [[ "$(cat "${app_dir}/preserved-marker")" == 'preserve existing bundle' ]]
done
printf 'macOS helper payload regression test passed\n'
