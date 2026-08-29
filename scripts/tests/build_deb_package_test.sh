#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd "${script_dir}/../.." && pwd)"
test_root="$(mktemp -d)"
mock_bin="${test_root}/bin"
mock_target="${test_root}/target"
output_dir="${test_root}/dist"
dpkg_log="${test_root}/dpkg-deb.log"
control_copy="${test_root}/control"

cleanup() {
  rm -rf "${test_root}"
}
trap cleanup EXIT

mkdir -p "${mock_bin}" "${mock_target}/release"

cat >"${mock_bin}/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
mkdir -p "${MOCK_TARGET_DIR}/release"
printf '#!/usr/bin/env bash\nexit 0\n' >"${MOCK_TARGET_DIR}/release/zenclash"
chmod +x "${MOCK_TARGET_DIR}/release/zenclash"
EOF

cat >"${mock_bin}/dpkg" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "${1:-}" == "--print-architecture" ]]
printf 'amd64\n'
EOF

cat >"${mock_bin}/dpkg-deb" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "${1:-}" >>"${MOCK_DPKG_LOG}"

case "${1:-}" in
  --build)
    package_path="${4:?missing package path}"
    mkdir -p "$(dirname "${package_path}")"
    cp "${3:?missing package root}/DEBIAN/control" "${MOCK_CONTROL_COPY}"
    : >"${package_path}"
    ;;
  --info)
    ;;
  --contents)
    trap 'exit 2' PIPE
    printf '%s\n' '-rwxr-xr-x root/root 1 ./usr/lib/zenclash/mihomo'
    for ((index = 0; index < 20000; index += 1)); do
      printf '%s\n' '-rw-r--r-- root/root 1 ./usr/share/doc/zenclash/filler'
    done
    printf '%s\n' '-rw-r--r-- root/root 1 ./usr/lib/zenclash/geoip.metadb'
    printf '%s\n' '-rw-r--r-- root/root 1 ./usr/lib/zenclash/recovery.yaml'
    printf '%s\n' '-rw-r--r-- root/root 1 ./usr/share/doc/zenclash/LICENSE'
    ;;
  *)
    printf 'Unexpected dpkg-deb invocation: %s\n' "$*" >&2
    exit 1
    ;;
esac
EOF

cat >"${mock_bin}/install" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
source_path=""
destination_path=""
for argument in "$@"; do
  source_path="${destination_path}"
  destination_path="${argument}"
done
[[ -n "${source_path}" && -n "${destination_path}" ]]
mkdir -p "$(dirname "${destination_path}")"
cp "${source_path}" "${destination_path}"
EOF

cat >"${test_root}/mihomo" <<'EOF'
#!/usr/bin/env bash
printf 'Mihomo test fixture\n'
EOF

chmod +x \
  "${mock_bin}/cargo" \
  "${mock_bin}/dpkg" \
  "${mock_bin}/dpkg-deb" \
  "${mock_bin}/install" \
  "${test_root}/mihomo"
printf 'fixture\n' >"${test_root}/geoip.metadb"

PATH="${mock_bin}:${PATH}" \
  MOCK_CONTROL_COPY="${control_copy}" \
  MOCK_DPKG_LOG="${dpkg_log}" \
  MOCK_TARGET_DIR="${mock_target}" \
  CARGO_TARGET_DIR="${mock_target}" \
  ZENCLASH_MIHOMO_BINARY="${test_root}/mihomo" \
  ZENCLASH_GEODATA_FILE="${test_root}/geoip.metadb" \
  ZENCLASH_CONFIG="${project_root}/platforms/common/default.yaml" \
  bash "${project_root}/scripts/build_deb_package.sh" 9.8.7 "${output_dir}"

package_path="${output_dir}/ZenClash-9.8.7-Ubuntu-24.04+-amd64.deb"
[[ -f "${package_path}" ]]
[[ "$(grep -c '^--contents$' "${dpkg_log}")" -eq 1 ]]
grep -Fxq \
  'Depends: libasound2t64, libfontconfig1, libgtk-3-0t64, libayatana-appindicator3-1, libvulkan1, libwayland-client0, libxdo3, libxkbcommon-x11-0' \
  "${control_copy}"

printf 'DEB packaging regression test passed\n'
