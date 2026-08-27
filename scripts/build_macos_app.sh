#!/bin/zsh
set -euo pipefail

SCRIPT_DIR="${0:A:h}"
PROJECT_ROOT="${SCRIPT_DIR:h}"
VERSION="${ZENCLASH_VERSION:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' "${PROJECT_ROOT}/Cargo.toml" | head -n 1)}"
OUTPUT_DIR="${ZENCLASH_OUTPUT_DIR:-${PROJECT_ROOT}/target}"
TARGET_TRIPLE="aarch64-apple-darwin"
APP_DIR="${OUTPUT_DIR}/ZenClash.app"
CONTENTS_DIR="${APP_DIR}/Contents"
MACOS_DIR="${CONTENTS_DIR}/MacOS"
RESOURCES_DIR="${CONTENTS_DIR}/Resources"
PROFILE_PATH="${ZENCLASH_CONFIG:-${PROJECT_ROOT}/platforms/common/default.yaml}"
MIHOMO_PATH="${ZENCLASH_MIHOMO_BINARY:-}"
CARGO_OUTPUT_ROOT="${CARGO_TARGET_DIR:-${PROJECT_ROOT}/target}"
MIHOMO_WORK_DIR=""

cleanup() {
  if [[ -n "${MIHOMO_WORK_DIR}" ]]; then
    rm -rf "${MIHOMO_WORK_DIR}"
  fi
}
trap cleanup EXIT

if [[ "$(uname -m)" != "arm64" ]]; then
  echo "The macOS release bundle must be built on an Apple Silicon runner." >&2
  exit 1
fi

if [[ -n "${MIHOMO_PATH}" ]]; then
  if [[ ! -x "${MIHOMO_PATH}" ]]; then
    echo "ZENCLASH_MIHOMO_BINARY is not an executable file: ${MIHOMO_PATH}" >&2
    exit 1
  fi
else
  MIHOMO_WORK_DIR="$(mktemp -d)"
  MIHOMO_PATH="${MIHOMO_WORK_DIR}/mihomo"
  "${SCRIPT_DIR}/download_mihomo.sh" darwin arm64 "${MIHOMO_PATH}"
fi

if [[ ! -f "${PROFILE_PATH}" ]]; then
  echo "Mihomo profile not found: ${PROFILE_PATH}" >&2
  exit 1
fi

if ! file "${MIHOMO_PATH}" | grep -Eq 'arm64|universal binary'; then
  echo "Mihomo is not an Apple Silicon binary: ${MIHOMO_PATH}" >&2
  exit 1
fi

cd "${PROJECT_ROOT}"
rustup target add "${TARGET_TRIPLE}"
cargo build --release --locked -p zenclash-ui --bin zenclash --target "${TARGET_TRIPLE}"

rm -rf "${APP_DIR}"
mkdir -p "${MACOS_DIR}" "${RESOURCES_DIR}"
cp "${CARGO_OUTPUT_ROOT}/${TARGET_TRIPLE}/release/zenclash" "${MACOS_DIR}/zenclash"
cp "${PROJECT_ROOT}/platforms/macos/Info.plist" "${CONTENTS_DIR}/Info.plist"
cp "${MIHOMO_PATH}" "${RESOURCES_DIR}/mihomo"
cp "${PROFILE_PATH}" "${RESOURCES_DIR}/profile.yaml"
cp "${PROJECT_ROOT}/platforms/common/recovery.yaml" "${RESOURCES_DIR}/recovery.yaml"
cp "${PROJECT_ROOT}/LICENSE" "${RESOURCES_DIR}/LICENSE.txt"
chmod 755 "${MACOS_DIR}/zenclash" "${RESOURCES_DIR}/mihomo"
"${RESOURCES_DIR}/mihomo" -v

/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString ${VERSION}" "${CONTENTS_DIR}/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion ${VERSION%%-*}" "${CONTENTS_DIR}/Info.plist"

if [[ -f "${PROJECT_ROOT}/platforms/macos/ZenClash.icns" ]]; then
  cp "${PROJECT_ROOT}/platforms/macos/ZenClash.icns" "${RESOURCES_DIR}/ZenClash.icns"
elif [[ -f "${PROJECT_ROOT}/examples/clash-party/build/icon.icns" ]]; then
  cp "${PROJECT_ROOT}/examples/clash-party/build/icon.icns" "${RESOURCES_DIR}/ZenClash.icns"
fi

if [[ -n "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  codesign --force --options runtime --timestamp --sign "${APPLE_SIGNING_IDENTITY}" "${RESOURCES_DIR}/mihomo"
  codesign --force --options runtime --timestamp --sign "${APPLE_SIGNING_IDENTITY}" "${APP_DIR}"
else
  codesign --force --sign - "${RESOURCES_DIR}/mihomo"
  codesign --force --deep --sign - "${APP_DIR}"
fi

codesign --verify --deep --strict --verbose=2 "${APP_DIR}"
echo "Built ${APP_DIR}"
