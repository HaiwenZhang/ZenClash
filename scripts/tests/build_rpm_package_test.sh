#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd "${script_dir}/../.." && pwd)"
test_root="$(mktemp -d)"
trap '[[ -n "${test_root}" && -d "${test_root}/bin" ]] && rm -rf "${test_root}"' EXIT
mkdir -p "${test_root}/bin" "${test_root}/target/release" "${test_root}/captured"

cat >"${test_root}/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
binary=zenclash
if [[ " $* " == *" -p zenclash-service "* ]]; then
  [[ " $* " == *" --features server "* ]]
  [[ "${MOCK_MISSING_SERVICE:-0}" == 0 ]] || exit 0
  binary=zenclash-service
fi
if [[ "${binary}" == zenclash-service ]]; then
  case "${MOCK_SERVICE_VERSION:-valid}" in
    valid) printf '#!/usr/bin/env bash\nprintf "fixture\\n"\n' ;;
    empty) printf '#!/usr/bin/env bash\nexit 0\n' ;;
    failed) printf '#!/usr/bin/env bash\nexit 7\n' ;;
    *) exit 2 ;;
  esac >"${CARGO_TARGET_DIR}/release/${binary}"
else
  printf '#!/usr/bin/env bash\nprintf "fixture\\n"\n' >"${CARGO_TARGET_DIR}/release/${binary}"
fi
chmod +x "${CARGO_TARGET_DIR}/release/${binary}"
EOF

cat >"${test_root}/bin/rpmbuild" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
spec=""
topdir=""
payload=""
while (($#)); do
  case "$1" in
    -bb) spec="$2"; shift 2 ;;
    --define)
      case "$2" in
        "_topdir "*) topdir="${2#_topdir }" ;;
        "payload_dir "*) payload="${2#payload_dir }" ;;
      esac
      shift 2 ;;
    *) exit 2 ;;
  esac
done
[[ -n "${spec}" && -n "${topdir}" && -n "${payload}" ]]
buildroot="${topdir}/BUILDROOT"
mkdir -p "${buildroot}" "${topdir}/RPMS/x86_64"
in_install=0
while IFS= read -r line; do
  line="${line%$'\r'}"
  [[ "${line}" != '%files' ]] || break
  if [[ "${line}" == '%install' ]]; then in_install=1; continue; fi
  [[ "${in_install}" == 1 && -n "${line}" ]] || continue
  [[ "${line}" == 'install '* ]]
  line="${line//\%\{payload_dir\}/${payload}}"
  line="${line//\%\{buildroot\}/${buildroot}}"
  line="${line//\%\{_bindir\}/\/usr\/bin}"
  line="${line//\%\{_prefix\}/\/usr}"
  line="${line//\%\{_datadir\}/\/usr\/share}"
  line="${line//\%\{_licensedir\}/\/usr\/share\/licenses}"
  bash -c "${line}"
done <"${spec}"
# Execute the actual spec's install commands; this does not build a native RPM.
cp "${buildroot}/usr/lib/zenclash/zenclash-service" "${MOCK_CAPTURED}/zenclash-service"
cp "${buildroot}/usr/lib/systemd/system/zenclash-service.service" "${MOCK_CAPTURED}/unit"
cp "${buildroot}/usr/share/polkit-1/actions/org.zenclash.service.policy" "${MOCK_CAPTURED}/policy"
cp "${buildroot}/usr/lib/zenclash/package-service.sh" "${MOCK_CAPTURED}/package-service.sh"
find "${buildroot}" -type f | sed "s|^${buildroot}||" >"${topdir}/RPMS/x86_64/zenclash.rpm"
EOF

cat >"${test_root}/bin/rpm" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  -qip) [[ -s "$2" ]] ;;
  -qlp) cat "$2" ;;
  *) exit 2 ;;
esac
EOF

printf '#!/usr/bin/env bash\nprintf "Mihomo fixture\\n"\n' >"${test_root}/mihomo"
printf 'fixture\n' >"${test_root}/geoip.metadb"
chmod +x "${test_root}/bin/"* "${test_root}/mihomo"
export PATH="${test_root}/bin:${PATH}"
export CARGO_TARGET_DIR="${test_root}/target"
export ZENCLASH_MIHOMO_BINARY="${test_root}/mihomo"
export ZENCLASH_GEODATA_FILE="${test_root}/geoip.metadb"
export MOCK_CAPTURED="${test_root}/captured"

bash "${project_root}/scripts/build_rpm_package.sh" 9.8.7 "${test_root}/dist"
package="${test_root}/dist/ZenClash-9.8.7-linux-x86_64.rpm"
[[ -s "${package}" ]]
cmp "${CARGO_TARGET_DIR}/release/zenclash-service" "${MOCK_CAPTURED}/zenclash-service"
cmp "${project_root}/platforms/linux/zenclash-service.service" "${MOCK_CAPTURED}/unit"
cmp "${project_root}/platforms/linux/org.zenclash.service.policy" "${MOCK_CAPTURED}/policy"
cmp "${project_root}/platforms/linux/package-service.sh" "${MOCK_CAPTURED}/package-service.sh"

export MOCK_SCRIPT_LIBRARY="${test_root}/script-library.sh"
export MOCK_REMOVAL_LOG="${test_root}/removal.log"
cat >"${MOCK_SCRIPT_LIBRARY}" <<'EOF'
. "${MOCK_CAPTURED}/package-service.sh"
zenclash_remove_service() {
  printf 'remove\n' >>"${MOCK_REMOVAL_LOG}"
  return "${MOCK_REMOVAL_EXIT:-0}"
}
EOF
awk '/^%preun$/{collect=1; next} /^%postun$/{exit} collect' \
  "${project_root}/platforms/linux/zenclash.spec" \
  | sed 's|^  \. /usr/lib/zenclash/package-service.sh$|  . "${MOCK_SCRIPT_LIBRARY}"|' \
  >"${test_root}/fixture-preun"
sh "${test_root}/fixture-preun" 1
[[ ! -e "${MOCK_REMOVAL_LOG}" ]]
sh "${test_root}/fixture-preun" 0
[[ "$(wc -l <"${MOCK_REMOVAL_LOG}")" -eq 1 ]]
if MOCK_REMOVAL_EXIT=17 sh "${test_root}/fixture-preun" 0; then
  echo 'RPM preun ignored failed service removal' >&2
  exit 1
else
  [[ "$?" -eq 17 ]]
fi

rm -f "${package}" "${CARGO_TARGET_DIR}/release/zenclash-service"
if MOCK_MISSING_SERVICE=1 bash "${project_root}/scripts/build_rpm_package.sh" 9.8.7 "${test_root}/dist" >"${test_root}/missing.log" 2>&1; then
  echo 'RPM packaging unexpectedly succeeded without the service binary' >&2
  exit 1
fi
[[ ! -e "${package}" ]]
for version_failure in empty failed; do
  rm -f "${MOCK_CAPTURED}/zenclash-service"
  if MOCK_SERVICE_VERSION="${version_failure}" bash "${project_root}/scripts/build_rpm_package.sh" 9.8.7 "${test_root}/dist" >"${test_root}/${version_failure}.log" 2>&1; then
    echo "RPM packaging unexpectedly accepted ${version_failure} service version output" >&2
    exit 1
  fi
  [[ ! -e "${package}" && ! -e "${MOCK_CAPTURED}/zenclash-service" ]]
done
printf 'RPM payload and missing-service regression tests passed\n'
