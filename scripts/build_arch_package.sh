#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == --help ]]; then
  cat <<'EOF'
Usage: scripts/build_arch_package.sh [version] [output-directory]

Build an x86_64 Arch Linux / Omarchy package from the current checkout.
Run as a regular user. The package is written to dist/ by default.

Build dependencies: base-devel rust clang cmake pkgconf curl jq
Runtime dependencies must also be installed (reported before building).

Optional environment variables:
  CARGO_TARGET_DIR                Cargo build cache directory
  ZENCLASH_VERSION                Application version
  ZENCLASH_PACKAGE_DIR            Package output directory
  ZENCLASH_CONFIG                 Bundled Mihomo profile
  ZENCLASH_MIHOMO_BINARY          Existing Mihomo executable
  ZENCLASH_GEODATA_FILE           Existing geoip.metadb
  MIHOMO_VERSION                 Mihomo release tag for the download script
  MIHOMO_GEODATA_VERSION         GeoData release tag for the download script
EOF
  exit 0
fi
if (( $# > 2 )); then
  echo "Usage: $0 [version] [output-directory]" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd "${script_dir}/.." && pwd)"
version="${1:-${ZENCLASH_VERSION:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' "${project_root}/Cargo.toml" | head -n 1)}}"
output_dir="${2:-${ZENCLASH_PACKAGE_DIR:-${project_root}/dist}}"
profile_path="${ZENCLASH_CONFIG:-${project_root}/platforms/common/default.yaml}"
mihomo_path="${ZENCLASH_MIHOMO_BINARY:-}"
geodata_path="${ZENCLASH_GEODATA_FILE:-}"

if [[ ! "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+([._+][0-9A-Za-z.]+)?$ ]]; then
  echo "Invalid Arch package version: ${version}" >&2
  exit 2
fi
if [[ "$(uname -s):$(uname -m)" != Linux:x86_64 ]]; then
  echo "This package requires an x86_64 Arch Linux / Omarchy build host." >&2
  exit 1
fi
if (( EUID == 0 )); then
  echo "Run this script as a regular user; makepkg must not run as root." >&2
  exit 1
fi
for command_name in cargo makepkg pacman fakeroot bsdtar zstd pkg-config sha256sum; do
  if ! command -v "${command_name}" >/dev/null 2>&1; then
    echo "Required command is missing: ${command_name}" >&2
    exit 1
  fi
done

runtime_dependencies=(
  alsa-lib fontconfig gcc-libs glibc gtk3 hicolor-icon-theme
  libayatana-appindicator libxcb libxkbcommon libxkbcommon-x11
  openssl polkit vulkan-icd-loader wayland xdotool
)
if ! missing_dependencies="$(pacman -T "${runtime_dependencies[@]}")"; then
  printf 'Install the missing dependencies before building:\n%s\n' "${missing_dependencies}" >&2
  exit 1
fi
pkg-config --exists alsa fontconfig gtk+-3.0 wayland-client xkbcommon xkbcommon-x11 openssl

# Resolve caller-supplied paths before changing into the checkout/build directory.
profile_path="$(realpath -e "${profile_path}")"
if [[ -n "${mihomo_path}" ]]; then
  mihomo_path="$(realpath -e "${mihomo_path}")"
fi
if [[ -n "${geodata_path}" ]]; then
  geodata_path="$(realpath -e "${geodata_path}")"
fi
mkdir -p "${output_dir}"
output_dir="$(cd "${output_dir}" && pwd)"
cd "${project_root}"
cargo_output_root="$(realpath -m "${CARGO_TARGET_DIR:-${project_root}/target}")"
export CARGO_TARGET_DIR="${cargo_output_root}"
work_dir="$(mktemp -d)"
trap 'rm -rf "${work_dir}"' EXIT
payload_dir="${work_dir}/payload"

if [[ ! -f "${profile_path}" ]]; then
  echo "Mihomo profile not found: ${profile_path}" >&2
  exit 1
fi
if [[ -z "${mihomo_path}" ]]; then
  mihomo_path="${work_dir}/mihomo"
  bash "${script_dir}/download_mihomo.sh" linux amd64 "${mihomo_path}"
fi
if [[ ! -s "${mihomo_path}" || ! -x "${mihomo_path}" ]]; then
  echo "Mihomo binary is missing, empty or not executable: ${mihomo_path}" >&2
  exit 1
fi
if [[ -z "${geodata_path}" ]]; then
  geodata_path="${work_dir}/geoip.metadb"
  bash "${script_dir}/download_mihomo_geodata.sh" "${geodata_path}"
fi
if [[ ! -s "${geodata_path}" || ! -f "${geodata_path}" ]]; then
  echo "Mihomo GeoData is missing or empty: ${geodata_path}" >&2
  exit 1
fi

bundled_mihomo_version="$("${mihomo_path}" -v)"
ZENCLASH_VERSION="${version}" \
ZENCLASH_BUNDLED_MIHOMO_VERSION="${bundled_mihomo_version}" \
cargo build --release --locked -p zenclash-ui --bin zenclash
ZENCLASH_VERSION="${version}" \
cargo build --release --locked -p zenclash-service --features standalone,client \
  --bin zenclash-service --bin zenclash-service-install --bin zenclash-service-uninstall

for binary in zenclash zenclash-service zenclash-service-install zenclash-service-uninstall; do
  binary_path="${cargo_output_root}/release/${binary}"
  if [[ ! -s "${binary_path}" || ! -x "${binary_path}" ]]; then
    echo "Build executable is missing, empty or not executable: ${binary_path}" >&2
    exit 1
  fi
done
if ! service_version="$("${cargo_output_root}/release/zenclash-service" --version)" || \
   [[ -z "${service_version//[[:space:]]/}" ]]; then
  echo "Service version check failed." >&2
  exit 1
fi

install -Dm755 "${cargo_output_root}/release/zenclash" "${payload_dir}/usr/bin/zenclash"
for binary in zenclash-service zenclash-service-install zenclash-service-uninstall; do
  install -Dm755 "${cargo_output_root}/release/${binary}" "${payload_dir}/usr/lib/zenclash/${binary}"
done
install -Dm755 "${mihomo_path}" "${payload_dir}/usr/lib/zenclash/mihomo"
install -Dm644 "${geodata_path}" "${payload_dir}/usr/lib/zenclash/geoip.metadb"
install -Dm644 "${profile_path}" "${payload_dir}/usr/lib/zenclash/profile.yaml"
install -Dm644 "${project_root}/platforms/common/recovery.yaml" "${payload_dir}/usr/lib/zenclash/recovery.yaml"
install -Dm644 "${project_root}/platforms/linux/package-service.sh" "${payload_dir}/usr/lib/zenclash/package-service.sh"
install -Dm644 "${project_root}/platforms/linux/org.zenclash.service.policy" \
  "${payload_dir}/usr/share/polkit-1/actions/org.zenclash.service.policy"
install -Dm644 "${project_root}/platforms/linux/zenclash.desktop" \
  "${payload_dir}/usr/share/applications/org.zenclash.ZenClash.desktop"
install -Dm644 "${project_root}/platforms/macos/ZenClash.png" \
  "${payload_dir}/usr/share/icons/hicolor/1024x1024/apps/zenclash.png"
install -Dm644 "${project_root}/LICENSE" "${payload_dir}/usr/share/licenses/zenclash/LICENSE"

cat >"${work_dir}/zenclash.install" <<'EOF'
post_install() {
  . /usr/lib/zenclash/package-service.sh
  zenclash_package_reload_manager
}

post_upgrade() {
  post_install
}

pre_remove() {
  . /usr/lib/zenclash/package-service.sh
  zenclash_package_pre_remove remove
}

post_remove() {
  if [ -d /run/systemd/system ]; then
    /usr/bin/systemctl daemon-reload
  fi
}
EOF

{
  printf 'pkgname=zenclash\npkgver=%q\npkgrel=1\n' "${version}"
  printf 'depends=('
  printf '%q ' "${runtime_dependencies[@]}"
  printf ')\n'
  cat <<'EOF'
pkgdesc='Native Mihomo client built with Rust and GPUI'
arch=('x86_64')
url='https://github.com/HaiwenZhang/ZenClash'
license=('GPL-3.0-only')
install=zenclash.install
# Cargo has already applied the workspace's release/LTO/strip settings.
options=('!strip' '!debug' '!lto')

package() {
  cp -a "${startdir}/payload/." "${pkgdir}/"
}
EOF
} >"${work_dir}/PKGBUILD"

cd "${work_dir}"
PKGDEST="${output_dir}" PKGEXT=.pkg.tar.zst makepkg --force --noconfirm
package_path="${output_dir}/zenclash-${version}-1-x86_64.pkg.tar.zst"
pacman -Qip "${package_path}"
bsdtar -tf "${package_path}" >"${work_dir}/package-contents.txt"
for required_path in \
  .PKGINFO .BUILDINFO .MTREE .INSTALL \
  usr/bin/zenclash \
  usr/lib/zenclash/zenclash-service \
  usr/lib/zenclash/zenclash-service-install \
  usr/lib/zenclash/zenclash-service-uninstall \
  usr/lib/zenclash/package-service.sh \
  usr/lib/zenclash/mihomo \
  usr/lib/zenclash/geoip.metadb \
  usr/lib/zenclash/profile.yaml \
  usr/lib/zenclash/recovery.yaml \
  usr/share/polkit-1/actions/org.zenclash.service.policy \
  usr/share/applications/org.zenclash.ZenClash.desktop \
  usr/share/icons/hicolor/1024x1024/apps/zenclash.png \
  usr/share/licenses/zenclash/LICENSE; do
  grep -Fxq "${required_path}" "${work_dir}/package-contents.txt"
done
cd "${output_dir}"
sha256sum "$(basename "${package_path}")" >"${package_path}.sha256"
echo "Built ${package_path}"
