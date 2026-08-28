#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd "${script_dir}/.." && pwd)"
version="${1:-${ZENCLASH_VERSION:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' "${project_root}/Cargo.toml" | head -n 1)}}"
output_dir="${2:-${ZENCLASH_PACKAGE_DIR:-${project_root}/dist}}"
package_flavor="${ZENCLASH_PACKAGE_FLAVOR:-linux}"
profile_path="${ZENCLASH_CONFIG:-${project_root}/platforms/common/default.yaml}"
cargo_output_root="${CARGO_TARGET_DIR:-${project_root}/target}"
work_dir="$(mktemp -d)"
payload_dir="${work_dir}/payload"
rpmbuild_dir="${work_dir}/rpmbuild"
mihomo_path="${ZENCLASH_MIHOMO_BINARY:-}"
geodata_path="${ZENCLASH_GEODATA_FILE:-}"

cleanup() {
  rm -rf "${work_dir}"
}
trap cleanup EXIT

if [[ ! "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+([._+][0-9A-Za-z.]+)?$ ]]; then
  echo "Invalid RPM version: ${version}" >&2
  exit 2
fi
if [[ ! -f "${profile_path}" ]]; then
  echo "Mihomo profile not found: ${profile_path}" >&2
  exit 1
fi
if [[ -n "${mihomo_path}" ]]; then
  if [[ ! -x "${mihomo_path}" ]]; then
    echo "ZENCLASH_MIHOMO_BINARY is not an executable file: ${mihomo_path}" >&2
    exit 1
  fi
else
  mihomo_path="${work_dir}/mihomo"
  "${script_dir}/download_mihomo.sh" linux amd64 "${mihomo_path}"
fi
if [[ -n "${geodata_path}" ]]; then
  if [[ ! -f "${geodata_path}" ]]; then
    echo "ZENCLASH_GEODATA_FILE is not a regular file: ${geodata_path}" >&2
    exit 1
  fi
else
  geodata_path="${work_dir}/geoip.metadb"
  bash "${script_dir}/download_mihomo_geodata.sh" "${geodata_path}"
fi

cd "${project_root}"
cargo build --release --locked -p zenclash-ui --bin zenclash
"${mihomo_path}" -v

install -Dm755 "${cargo_output_root}/release/zenclash" "${payload_dir}/zenclash"
install -Dm755 "${mihomo_path}" "${payload_dir}/mihomo"
install -Dm644 "${geodata_path}" "${payload_dir}/geoip.metadb"
install -Dm644 "${profile_path}" "${payload_dir}/profile.yaml"
install -Dm644 "${project_root}/platforms/common/recovery.yaml" "${payload_dir}/recovery.yaml"
install -Dm644 "${project_root}/platforms/macos/ZenClash.png" "${payload_dir}/zenclash.png"
install -Dm644 "${project_root}/platforms/linux/zenclash.desktop" "${payload_dir}/zenclash.desktop"
install -Dm644 "${project_root}/LICENSE" "${payload_dir}/LICENSE"
mkdir -p "${rpmbuild_dir}" "${output_dir}"

rpmbuild -bb "${project_root}/platforms/linux/zenclash.spec" \
  --define "_topdir ${rpmbuild_dir}" \
  --define "app_version ${version}" \
  --define "payload_dir ${payload_dir}"

built_rpm="$(find "${rpmbuild_dir}/RPMS" -type f -name '*.rpm' -print -quit)"
if [[ -z "${built_rpm}" ]]; then
  echo "rpmbuild did not produce an RPM package" >&2
  exit 1
fi
package_path="${output_dir}/ZenClash-${version}-${package_flavor}-x86_64.rpm"
cp "${built_rpm}" "${package_path}"
rpm -qip "${package_path}" >/dev/null
rpm -qlp "${package_path}" >/dev/null
rpm -qlp "${package_path}" | grep -Eq '^/usr/lib/zenclash/mihomo$'
rpm -qlp "${package_path}" | grep -Eq '^/usr/lib/zenclash/geoip.metadb$'
rpm -qlp "${package_path}" | grep -Eq '^/usr/lib/zenclash/recovery.yaml$'
rpm -qlp "${package_path}" | grep -Eq '^/usr/share/licenses/zenclash/LICENSE$'

echo "Built ${package_path}"
