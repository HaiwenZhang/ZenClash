#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd "${script_dir}/../.." && pwd)"
test_root="$(mktemp -d)"
mock_bin="${test_root}/bin"
export MIHOMO_VERSION=v1.19.30
export ZENCLASH_DEPENDENCY_LICENSE_DIR="${test_root}/dependency-licenses"
mock_target="${test_root}/target"
output_dir="${test_root}/dist"
dpkg_log="${test_root}/dpkg-deb.log"
control_copy="${test_root}/control"
service_copy="${test_root}/zenclash-service"
unit_copy="${test_root}/zenclash-service.service"
policy_copy="${test_root}/org.zenclash.service.policy"
library_copy="${test_root}/package-service.sh"
prerm_copy="${test_root}/prerm"

cleanup() {
  rm -rf "${test_root}"
}
trap cleanup EXIT

mkdir -p "${mock_bin}" "${mock_target}/release"

cat >"${mock_bin}/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
mkdir -p "${MOCK_TARGET_DIR}/release"
case " $* " in
  *" -p zenclash-service "*)
    [[ " $* " == *" --features standalone,client "* ]]
    if [[ "${MOCK_MISSING_SERVICE:-0}" == 0 ]]; then
      case "${MOCK_SERVICE_VERSION:-valid}" in
        valid) printf '#!/usr/bin/env bash\nprintf "zenclash-service fixture\\n"\n' ;;
        empty) printf '#!/usr/bin/env bash\nexit 0\n' ;;
        failed) printf '#!/usr/bin/env bash\nexit 7\n' ;;
        *) exit 2 ;;
      esac >"${MOCK_TARGET_DIR}/release/zenclash-service"
      chmod +x "${MOCK_TARGET_DIR}/release/zenclash-service"
      for tool in zenclash-service-install zenclash-service-uninstall; do
        [[ " $* " == *" --bin ${tool} "* ]]
        cp "${MOCK_TARGET_DIR}/release/zenclash-service" "${MOCK_TARGET_DIR}/release/${tool}"
      done
    fi
    ;;
  *)
    printf '#!/usr/bin/env bash\nexit 0\n' >"${MOCK_TARGET_DIR}/release/zenclash"
    chmod +x "${MOCK_TARGET_DIR}/release/zenclash"
    ;;
esac
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
    [[ -x "${3}/usr/lib/zenclash/zenclash-service" ]]
    cp "${3}/usr/lib/zenclash/zenclash-service" "${MOCK_SERVICE_COPY}"
    for tool in zenclash-service-install zenclash-service-uninstall; do
      [[ -s "${3}/usr/lib/zenclash/${tool}" && -x "${3}/usr/lib/zenclash/${tool}" ]]
    done
    [[ ! -e "${3}/usr/lib/systemd/system/zenclash-service.service" ]]
    cp "${3}/usr/share/polkit-1/actions/org.zenclash.service.policy" "${MOCK_POLICY_COPY}"
    cp "${3}/usr/lib/zenclash/package-service.sh" "${MOCK_LIBRARY_COPY}"
    for name in NOTICE.md CORRESPONDING-SOURCE.md licenses/zenclash-service/LICENSE licenses/zenclash-service-integration/LICENSE licenses/dependencies/MANIFEST.json; do
      [[ -s "${3}/usr/share/doc/zenclash/${name}" ]]
    done
    [[ -x "${3}/DEBIAN/prerm" && -x "${3}/DEBIAN/postinst" && -x "${3}/DEBIAN/postrm" ]]
    cp "${3}/DEBIAN/prerm" "${MOCK_PRERM_COPY}"
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
    for name in NOTICE.md CORRESPONDING-SOURCE.md licenses/zenclash-service/LICENSE licenses/zenclash-service-integration/LICENSE licenses/dependencies/MANIFEST.json; do
      printf '%s\n' "-rw-r--r-- root/root 1 ./usr/share/doc/zenclash/${name}"
    done
    printf '%s\n' '-rwxr-xr-x root/root 1 ./usr/lib/zenclash/zenclash-service'
    printf '%s\n' '-rw-r--r-- root/root 1 ./usr/lib/zenclash/package-service.sh'
    for tool in zenclash-service-install zenclash-service-uninstall; do
      printf '%s\n' "-rwxr-xr-x root/root 1 ./usr/lib/zenclash/${tool}"
    done
    printf '%s\n' '-rw-r--r-- root/root 1 ./usr/share/polkit-1/actions/org.zenclash.service.policy'
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
python3 "${script_dir}/license_fixture.py" "${project_root}" "${ZENCLASH_DEPENDENCY_LICENSE_DIR}" "${test_root}/geoip.metadb"

PATH="${mock_bin}:${PATH}" \
  MOCK_CONTROL_COPY="${control_copy}" \
  MOCK_DPKG_LOG="${dpkg_log}" \
  MOCK_SERVICE_COPY="${service_copy}" \
  MOCK_UNIT_COPY="${unit_copy}" \
  MOCK_POLICY_COPY="${policy_copy}" \
  MOCK_LIBRARY_COPY="${library_copy}" \
  MOCK_PRERM_COPY="${prerm_copy}" \
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
cmp "${mock_target}/release/zenclash-service" "${service_copy}"
cmp "${project_root}/platforms/linux/org.zenclash.service.policy" "${policy_copy}"
cmp "${project_root}/platforms/linux/package-service.sh" "${library_copy}"

export MOCK_SCRIPT_LIBRARY="${test_root}/script-library.sh"
export MOCK_LIBRARY_COPY="${library_copy}"
export MOCK_REMOVAL_LOG="${test_root}/removal.log"
cat >"${MOCK_SCRIPT_LIBRARY}" <<'EOF'
. "${MOCK_LIBRARY_COPY}"
zenclash_remove_service() {
  printf 'remove\n' >>"${MOCK_REMOVAL_LOG}"
  return "${MOCK_REMOVAL_EXIT:-0}"
}
EOF
sed 's|^\. /usr/lib/zenclash/package-service.sh$|. "${MOCK_SCRIPT_LIBRARY}"|' \
  "${prerm_copy}" >"${test_root}/fixture-prerm"
sh "${test_root}/fixture-prerm" upgrade 9.8.8
[[ ! -e "${MOCK_REMOVAL_LOG}" ]]
sh "${test_root}/fixture-prerm" remove
[[ "$(wc -l <"${MOCK_REMOVAL_LOG}")" -eq 1 ]]
if MOCK_REMOVAL_EXIT=17 sh "${test_root}/fixture-prerm" remove; then
  echo 'Debian prerm ignored failed service removal' >&2
  exit 1
else
  [[ "$?" -eq 17 ]]
fi

rm -f "${mock_target}/release/zenclash-service" "${package_path}"
if PATH="${mock_bin}:${PATH}" \
  MOCK_TARGET_DIR="${mock_target}" \
  MOCK_MISSING_SERVICE=1 \
  CARGO_TARGET_DIR="${mock_target}" \
  ZENCLASH_MIHOMO_BINARY="${test_root}/mihomo" \
  ZENCLASH_GEODATA_FILE="${test_root}/geoip.metadb" \
  bash "${project_root}/scripts/build_deb_package.sh" 9.8.7 "${output_dir}" >"${test_root}/missing.log" 2>&1; then
  echo 'Packaging unexpectedly succeeded without the service binary' >&2
  exit 1
fi
[[ ! -f "${package_path}" ]]

for version_failure in empty failed; do
  : >"${dpkg_log}"
  if PATH="${mock_bin}:${PATH}" \
    MOCK_TARGET_DIR="${mock_target}" \
    MOCK_SERVICE_VERSION="${version_failure}" \
    MOCK_DPKG_LOG="${dpkg_log}" \
    MOCK_CONTROL_COPY="${control_copy}" \
    MOCK_SERVICE_COPY="${service_copy}" \
    MOCK_UNIT_COPY="${unit_copy}" \
    MOCK_POLICY_COPY="${policy_copy}" \
    CARGO_TARGET_DIR="${mock_target}" \
    ZENCLASH_MIHOMO_BINARY="${test_root}/mihomo" \
    ZENCLASH_GEODATA_FILE="${test_root}/geoip.metadb" \
    bash "${project_root}/scripts/build_deb_package.sh" 9.8.7 "${output_dir}" >"${test_root}/${version_failure}.log" 2>&1; then
    echo "DEB packaging unexpectedly accepted ${version_failure} service version output" >&2
    exit 1
  fi
  [[ ! -f "${package_path}" && ! -s "${dpkg_log}" ]]
done

printf 'DEB packaging regression test passed\n'
