#!/usr/bin/env bash
set -euo pipefail

if command -v apt-get >/dev/null 2>&1; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update
  apt-get install -y --no-install-recommends \
    build-essential clang cmake curl file git gzip jq pkg-config \
    libasound2-dev libfontconfig1-dev libglib2.0-dev libgtk-3-dev \
    libappindicator3-dev libssl-dev libvulkan1 libwayland-dev \
    libx11-xcb-dev libxdo-dev libxkbcommon-x11-dev \
    dpkg-dev desktop-file-utils
  exit 0
fi

if command -v dnf >/dev/null 2>&1; then
  dnf install -y dnf-plugins-core
  distro_id="$(. /etc/os-release && printf '%s' "${ID}")"
  distro_version="$(. /etc/os-release && printf '%s' "${VERSION_ID%%.*}")"

  if [[ "${distro_id}" == "rocky" ]]; then
    if [[ "${distro_version}" == "8" ]]; then
      dnf config-manager --set-enabled powertools
    elif [[ "${distro_version}" == "9" ]]; then
      dnf config-manager --set-enabled crb
    fi
    dnf install -y epel-release
  fi

  dnf install -y \
    alsa-lib-devel clang cmake curl file fontconfig-devel gcc gcc-c++ git \
    glib2-devel gtk3-devel gzip jq libappindicator-gtk3-devel libxcb-devel \
    libxdo-devel libxkbcommon-x11-devel openssl-devel pkgconf-pkg-config \
    rpm-build tar vulkan-loader wayland-devel
  exit 0
fi

echo "Unsupported Linux distribution; expected an APT or DNF based image." >&2
exit 1
