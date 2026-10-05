#!/bin/sh

zenclash_remove_service() {
  /usr/lib/zenclash/zenclash-service-uninstall
}

zenclash_package_pre_remove() {
  case "${1:-}" in
    remove) zenclash_remove_service ;;
  esac
}

zenclash_package_reload_manager() {
  if [ -d /run/systemd/system ]; then
    /usr/bin/systemctl daemon-reload
  fi
}
