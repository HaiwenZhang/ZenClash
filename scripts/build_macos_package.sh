#!/bin/zsh
set -euo pipefail

SCRIPT_DIR="${0:A:h}"
PROJECT_ROOT="${SCRIPT_DIR:h}"
VERSION="${1:-${ZENCLASH_VERSION:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' "${PROJECT_ROOT}/Cargo.toml" | head -n 1)}}"
OUTPUT_DIR="${2:-${ZENCLASH_PACKAGE_DIR:-${PROJECT_ROOT}/dist}}"
WORK_DIR="$(mktemp -d)"
APP_OUTPUT_DIR="${WORK_DIR}/app"
DMG_ROOT="${WORK_DIR}/dmg"
DMG_PATH="${OUTPUT_DIR}/ZenClash-${VERSION}-macOS-arm64.dmg"

cleanup() {
  rm -rf "${WORK_DIR}"
}
trap cleanup EXIT

mkdir -p "${APP_OUTPUT_DIR}" "${DMG_ROOT}" "${OUTPUT_DIR}"
ZENCLASH_VERSION="${VERSION}" ZENCLASH_OUTPUT_DIR="${APP_OUTPUT_DIR}" \
  "${SCRIPT_DIR}/build_macos_app.sh"

ditto "${APP_OUTPUT_DIR}/ZenClash.app" "${DMG_ROOT}/ZenClash.app"
ln -s /Applications "${DMG_ROOT}/Applications"
rm -f "${DMG_PATH}"
hdiutil create \
  -volname "ZenClash ${VERSION}" \
  -srcfolder "${DMG_ROOT}" \
  -format UDZO \
  -ov \
  "${DMG_PATH}"
hdiutil imageinfo "${DMG_PATH}" >/dev/null

echo "Built ${DMG_PATH}"
