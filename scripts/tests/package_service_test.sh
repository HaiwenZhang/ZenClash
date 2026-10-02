#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd "${script_dir}/../.." && pwd)"
. "${project_root}/platforms/linux/package-service.sh"

removals=0
zenclash_remove_service() {
  removals=$((removals + 1))
}

zenclash_package_pre_remove upgrade
zenclash_package_pre_remove failed-upgrade
zenclash_package_pre_remove deconfigure
zenclash_package_pre_remove
[[ "${removals}" -eq 0 ]]

zenclash_package_pre_remove remove
[[ "${removals}" -eq 1 ]]
zenclash_package_pre_remove remove
[[ "${removals}" -eq 2 ]]

zenclash_remove_service() {
  return 17
}
if zenclash_package_pre_remove remove; then
  echo 'Package removal ignored the service removal failure' >&2
  exit 1
else
  [[ "$?" -eq 17 ]]
fi

echo 'Package removal policy behaviors passed.'
