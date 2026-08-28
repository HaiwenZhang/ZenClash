#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "Usage: $0 <output-path> [release-tag]" >&2
  exit 2
fi

output_path="$1"
release_tag="${2:-${MIHOMO_GEODATA_VERSION:-latest}}"
asset_name="geoip.metadb"
api_url="https://api.github.com/repos/MetaCubeX/meta-rules-dat/releases/tags/${release_tag}"
work_dir="$(mktemp -d)"
release_json="${work_dir}/release.json"
download_path="${work_dir}/${asset_name}"

cleanup() {
  rm -rf "${work_dir}"
}
trap cleanup EXIT

for command_name in curl jq; do
  if ! command -v "${command_name}" >/dev/null 2>&1; then
    echo "Required command is missing: ${command_name}" >&2
    exit 1
  fi
done

curl_args=(--fail --silent --show-error --location --retry 3)
if [[ -n "${GH_TOKEN:-}" ]]; then
  curl_args+=(--header "Authorization: Bearer ${GH_TOKEN}")
fi

curl "${curl_args[@]}" \
  --header "Accept: application/vnd.github+json" \
  --header "X-GitHub-Api-Version: 2022-11-28" \
  "${api_url}" \
  --output "${release_json}"

download_url="$(jq -er --arg name "${asset_name}" '.assets[] | select(.name == $name) | .browser_download_url' "${release_json}")"
asset_digest="$(jq -er --arg name "${asset_name}" '.assets[] | select(.name == $name) | .digest // empty' "${release_json}" || true)"
if [[ -z "${download_url}" ]]; then
  echo "Mihomo GeoData release asset not found: ${asset_name}" >&2
  exit 1
fi
if [[ "${asset_digest}" != sha256:* ]]; then
  echo "Mihomo GeoData asset does not publish a SHA-256 digest: ${asset_name}" >&2
  exit 1
fi

curl "${curl_args[@]}" "${download_url}" --output "${download_path}"

expected_hash="${asset_digest#sha256:}"
if command -v sha256sum >/dev/null 2>&1; then
  actual_hash="$(sha256sum "${download_path}" | awk '{print $1}')"
else
  actual_hash="$(shasum -a 256 "${download_path}" | awk '{print $1}')"
fi
if [[ "${actual_hash}" != "${expected_hash}" ]]; then
  echo "Mihomo GeoData SHA-256 mismatch for ${asset_name}" >&2
  exit 1
fi

mkdir -p "$(dirname "${output_path}")"
cp "${download_path}" "${output_path}"
echo "Downloaded verified ${asset_name} to ${output_path}"
