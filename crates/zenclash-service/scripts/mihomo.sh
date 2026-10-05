#!/usr/bin/env bash

set -e

PLATFORM="$(uname | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
FORCE=0
TARGET=""
RES_DIR="./temp"
META_URL_PREFIX="https://github.com/MetaCubeX/mihomo/releases/download"
META_ALPHA_URL_PREFIX="https://github.com/MetaCubeX/mihomo/releases/download/Prerelease-Alpha"

get_platform_arch() {
  case "$PLATFORM" in
    linux)
      case "$ARCH" in
        x86_64)      echo "linux-x64" ;;
        aarch64)     echo "linux-arm64" ;;
        armv7l)      echo "linux-arm" ;;
        i686)        echo "linux-ia32" ;;
        riscv64)     echo "linux-riscv64" ;;
        loongarch64) echo "linux-loong64" ;;
        *)           echo "" ;;
      esac
      ;;
    darwin)
      case "$ARCH" in
        x86_64)      echo "darwin-x64" ;;
        arm64)       echo "darwin-arm64" ;;
        *)           echo "" ;;
      esac
      ;;
    msys*|mingw*|cygwin*|win32)
      case "$ARCH" in
        x86_64)      echo "win32-x64" ;;
        i686)        echo "win32-ia32" ;;
        aarch64)     echo "win32-arm64" ;;
        *)           echo "" ;;
      esac
      ;;
    *)
      echo ""
      ;;
  esac
}

get_mihomo_bin_name() {
  local key="$1"
  case "$key" in
    win32-x64)      echo "mihomo-windows-amd64-v2" ;;
    win32-ia32)     echo "mihomo-windows-386" ;;
    win32-arm64)    echo "mihomo-windows-arm64" ;;
    darwin-x64)     echo "mihomo-darwin-amd64-v2" ;;
    darwin-arm64)   echo "mihomo-darwin-arm64" ;;
    linux-x64)      echo "mihomo-linux-amd64-v2" ;;
    linux-ia32)     echo "mihomo-linux-386" ;;
    linux-arm64)    echo "mihomo-linux-arm64" ;;
    linux-arm)      echo "mihomo-linux-armv7" ;;
    linux-riscv64)  echo "mihomo-linux-riscv64" ;;
    linux-loong64)  echo "mihomo-linux-loong64" ;;
    *)              echo "" ;;
  esac
}

download_file() {
  local url="$1"
  local path="$2"
  echo "[INFO] Downloading $url to $path"
  curl -L --retry 3 --fail -o "$path" "$url"
}

extract_archive() {
  local file="$1"
  local out="$2"
  if [[ "$file" == *.zip ]]; then
    unzip -o "$file" -d "$out"
  elif [[ "$file" == *.gz ]]; then
    gzip -dc "$file" > "$out/$(basename "${file%.gz}")"
  else
    echo "[ERROR] Unknown archive: $file"
    exit 1
  fi
}

get_latest_version() {
  local url="$1"
  curl -sL "$url" | tr -d ' \n'
}

fetch_mihomo() {
  local is_alpha="$1"
  local plat_arch
  plat_arch="$(get_platform_arch)"
  if [[ -z "$plat_arch" ]]; then
    echo "[ERROR] Unsupported platform/arch: $PLATFORM/$ARCH"
    exit 1
  fi

  local bin_name
  bin_name="$(get_mihomo_bin_name "$plat_arch")"
  if [[ -z "$bin_name" ]]; then
    echo "[ERROR] No binary matched for: $plat_arch"
    exit 1
  fi

  local url_prefix version url_ext ver_url
  if [[ "$is_alpha" == "alpha" ]]; then
    url_prefix="$META_ALPHA_URL_PREFIX"
    url_ext="zip"
    ver_url="https://github.com/MetaCubeX/mihomo/releases/download/Prerelease-Alpha/version.txt"
  else
    url_prefix="$META_URL_PREFIX"
    url_ext="$([[ $plat_arch == win32* ]] && echo 'zip' || echo 'gz')"
    ver_url="https://github.com/MetaCubeX/mihomo/releases/latest/download/version.txt"
  fi

  version="$(get_latest_version "$ver_url")"
  [[ -z "$version" ]] && { echo "[ERROR] Cannot fetch version!"; exit 1; }

  local archive_file="$RES_DIR/${bin_name}-${version}.${url_ext}"
  local extracted_file
  if [[ "$url_ext" == "gz" ]]; then
    extracted_file="$RES_DIR/${bin_name}-${version}"
  else
    extracted_file="$RES_DIR/${bin_name}$( [[ $plat_arch == win32* ]] && echo '.exe')"
  fi

  local final_bin
  if [[ $plat_arch == win32-* ]]; then
    final_bin="$RES_DIR/mihomo.exe"
  else
    final_bin="$RES_DIR/mihomo"
  fi

  local download_url
  if [[ "$is_alpha" == "alpha" ]]; then
    download_url="${url_prefix}/${bin_name}-${version}.${url_ext}"
  else
    download_url="${url_prefix}/${version}/${bin_name}-${version}.${url_ext}"
  fi

  mkdir -p "$RES_DIR"
  if [[ $FORCE -eq 1 || ! -f "$archive_file" ]]; then
    download_file "$download_url" "$archive_file"
  fi

  if [[ $FORCE -eq 1 || ! -f "$final_bin" ]]; then
    rm -f "$final_bin" "$extracted_file"
    extract_archive "$archive_file" "$RES_DIR"

    if [[ $plat_arch == win32-* ]]; then
      mv -f "$RES_DIR/${bin_name}.exe" "$final_bin"
    else
      mv -f "$extracted_file" "$final_bin"
    fi
    chmod +x "$final_bin"
    echo "[SUCCESS] Extracted $final_bin"
  else
    echo "[INFO] Binary already exists at $final_bin"
  fi
}

usage() {
  echo "Usage: $0 [--force|-f] [alpha|release]"
  echo "Pulls mihomo binary (alpha or release) into $RES_DIR as mihomo or mihomo.exe"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --force|-f)
      FORCE=1
      ;;
    alpha|release)
      TARGET="$1"
      ;;
    *)
      usage
      exit 1
      ;;
  esac
  shift
done

[[ -z "$TARGET" ]] && TARGET="release"

fetch_mihomo "$TARGET"
