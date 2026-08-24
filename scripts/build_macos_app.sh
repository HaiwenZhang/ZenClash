#!/bin/zsh
set -euo pipefail

SCRIPT_DIR="${0:A:h}"
PROJECT_ROOT="${SCRIPT_DIR:h}"
APP_DIR="${PROJECT_ROOT}/target/ZenClash.app"
CONTENTS_DIR="${APP_DIR}/Contents"
MACOS_DIR="${CONTENTS_DIR}/MacOS"
RESOURCES_DIR="${CONTENTS_DIR}/Resources"
PROFILE_PATH="${ZENCLASH_CONFIG:-${PROJECT_ROOT}/examples/19facdf022b.yaml}"
MIHOMO_PATH="${ZENCLASH_MIHOMO_BINARY:-}"

if [[ -z "${MIHOMO_PATH}" ]]; then
  MIHOMO_PATH="$(command -v mihomo || true)"
fi

if [[ ! -x "${MIHOMO_PATH}" ]]; then
  echo "Set ZENCLASH_MIHOMO_BINARY to a real executable Mihomo binary." >&2
  exit 1
fi

if [[ ! -f "${PROFILE_PATH}" ]]; then
  echo "Mihomo profile not found: ${PROFILE_PATH}" >&2
  exit 1
fi

cd "${PROJECT_ROOT}"
cargo build --release -p zenclash-ui --bin zenclash

mkdir -p "${MACOS_DIR}" "${RESOURCES_DIR}"
cp "${PROJECT_ROOT}/target/release/zenclash" "${MACOS_DIR}/zenclash"
cp "${PROJECT_ROOT}/platforms/macos/Info.plist" "${CONTENTS_DIR}/Info.plist"
cp "${MIHOMO_PATH}" "${RESOURCES_DIR}/mihomo"
cp "${PROFILE_PATH}" "${RESOURCES_DIR}/profile.yaml"
chmod 755 "${MACOS_DIR}/zenclash" "${RESOURCES_DIR}/mihomo"

if [[ -f "${PROJECT_ROOT}/examples/clash-party/build/icon.icns" ]]; then
  cp "${PROJECT_ROOT}/examples/clash-party/build/icon.icns" "${RESOURCES_DIR}/ZenClash.icns"
fi

# Ad-hoc signing keeps the local bundle internally consistent without requiring
# an Apple Developer identity. Distribution builds can re-sign the same bundle.
if command -v codesign >/dev/null 2>&1; then
  codesign --force --deep --sign - "${APP_DIR}"
fi

echo "Built ${APP_DIR}"
