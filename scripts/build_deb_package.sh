#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd "${script_dir}/.." && pwd)"
version="${1:-${ZENCLASH_VERSION:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' "${project_root}/Cargo.toml" | head -n 1)}}"
output_dir="${2:-${ZENCLASH_PACKAGE_DIR:-${project_root}/dist}}"
profile_path="${ZENCLASH_CONFIG:-${project_root}/examples/19facdf022b.yaml}"
cargo_output_root="${CARGO_TARGET_DIR:-${project_root}/target}"
architecture="$(dpkg --print-architecture)"
work_dir="$(mktemp -d)"
package_root="${work_dir}/root"
mihomo_path="${ZENCLASH_MIHOMO_BINARY:-}"

cleanup() {
  rm -rf "${work_dir}"
}
trap cleanup EXIT

if [[ ! "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.+~][0-9A-Za-z.-]+)?$ ]]; then
  echo "Invalid Debian package version: ${version}" >&2
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

cd "${project_root}"
cargo build --release --locked -p zenclash-ui --bin zenclash
"${mihomo_path}" -v

install -Dm755 "${cargo_output_root}/release/zenclash" "${package_root}/usr/bin/zenclash"
install -Dm755 "${mihomo_path}" "${package_root}/usr/lib/zenclash/mihomo"
install -Dm644 "${profile_path}" "${package_root}/usr/lib/zenclash/profile.yaml"
install -Dm644 "${project_root}/platforms/macos/ZenClash.png" \
  "${package_root}/usr/share/icons/hicolor/1024x1024/apps/zenclash.png"
install -Dm644 "${project_root}/platforms/linux/zenclash.desktop" \
  "${package_root}/usr/share/applications/org.zenclash.ZenClash.desktop"
mkdir -p "${package_root}/DEBIAN"

cat >"${package_root}/DEBIAN/control" <<EOF
Package: zenclash
Version: ${version}
Section: net
Priority: optional
Architecture: ${architecture}
Maintainer: ZenClash contributors
Depends: libasound2, libfontconfig1, libgtk-3-0, libappindicator3-1 | libayatana-appindicator3-1, libvulkan1, libwayland-client0, libxdo3, libxkbcommon-x11-0
Description: Native Mihomo client built with Rust and GPUI
 ZenClash provides native proxy management, traffic monitoring, subscription
 management, runtime configuration and a bundled real Mihomo core.
EOF

mkdir -p "${output_dir}"
package_path="${output_dir}/ZenClash-${version}-Ubuntu-22.04+-${architecture}.deb"
dpkg-deb --build --root-owner-group "${package_root}" "${package_path}"
dpkg-deb --info "${package_path}" >/dev/null
dpkg-deb --contents "${package_path}" >/dev/null
dpkg-deb --contents "${package_path}" | grep -Eq '[[:space:]]\./usr/lib/zenclash/mihomo$'

echo "Built ${package_path}"
