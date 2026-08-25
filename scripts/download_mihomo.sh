#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 || $# -gt 4 ]]; then
  echo "Usage: $0 <darwin|linux> <arm64|amd64> <output-path> [version]" >&2
  exit 2
fi

platform="$1"
arch="$2"
output_path="$3"
version="${4:-${MIHOMO_VERSION:-v1.19.30}}"

case "${platform}:${arch}" in
  darwin:arm64|linux:amd64) ;;
  *)
    echo "Unsupported Mihomo release target: ${platform}:${arch}" >&2
    exit 2
    ;;
esac

for command_name in curl jq gzip; do
  if ! command -v "${command_name}" >/dev/null 2>&1; then
    echo "Required command is missing: ${command_name}" >&2
    exit 1
  fi
done

asset_name="mihomo-${platform}-${arch}-${version}.gz"
api_url="https://api.github.com/repos/MetaCubeX/mihomo/releases/tags/${version}"
work_dir="$(mktemp -d)"
release_json="${work_dir}/release.json"
archive_path="${work_dir}/${asset_name}"

cleanup() {
  rm -rf "${work_dir}"
}
trap cleanup EXIT

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
  echo "Mihomo release asset not found: ${asset_name}" >&2
  exit 1
fi

curl "${curl_args[@]}" "${download_url}" --output "${archive_path}"

if [[ "${asset_digest}" == sha256:* ]]; then
  expected_hash="${asset_digest#sha256:}"
  if command -v sha256sum >/dev/null 2>&1; then
    actual_hash="$(sha256sum "${archive_path}" | awk '{print $1}')"
  else
    actual_hash="$(shasum -a 256 "${archive_path}" | awk '{print $1}')"
  fi
  if [[ "${actual_hash}" != "${expected_hash}" ]]; then
    echo "Mihomo SHA-256 mismatch for ${asset_name}" >&2
    exit 1
  fi
fi

mkdir -p "$(dirname "${output_path}")"
gzip -dc "${archive_path}" >"${output_path}"
chmod 755 "${output_path}"
"${output_path}" -v

echo "Downloaded verified ${asset_name} to ${output_path}"
