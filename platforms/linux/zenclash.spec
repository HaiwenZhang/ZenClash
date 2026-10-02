Name:           zenclash
Version:        %{app_version}
Release:        1%{?dist}
Summary:        Native Mihomo client built with Rust and GPUI
License:        GPL-3.0-only
URL:            https://github.com/HaiwenZhang/zenclash
BuildArch:      x86_64
Requires:       alsa-lib
Requires:       fontconfig
Requires:       gtk3
Requires:       hicolor-icon-theme
Requires:       libappindicator-gtk3
Requires:       libxdo
Requires:       libxkbcommon-x11
Requires:       vulkan-loader
Requires:       wayland

%description
ZenClash provides native proxy management, traffic monitoring, subscription
management, runtime configuration and a bundled real Mihomo core.

%prep

%build

%install
install -Dpm0755 %{payload_dir}/zenclash %{buildroot}%{_bindir}/zenclash
install -Dpm0755 %{payload_dir}/zenclash-service %{buildroot}%{_prefix}/lib/zenclash/zenclash-service
install -Dpm0644 %{payload_dir}/package-service.sh %{buildroot}%{_prefix}/lib/zenclash/package-service.sh
install -Dpm0644 %{payload_dir}/zenclash-service.service %{buildroot}%{_prefix}/lib/systemd/system/zenclash-service.service
install -Dpm0644 %{payload_dir}/org.zenclash.service.policy %{buildroot}%{_datadir}/polkit-1/actions/org.zenclash.service.policy
install -Dpm0755 %{payload_dir}/mihomo %{buildroot}%{_prefix}/lib/zenclash/mihomo
install -Dpm0644 %{payload_dir}/geoip.metadb %{buildroot}%{_prefix}/lib/zenclash/geoip.metadb
install -Dpm0644 %{payload_dir}/profile.yaml %{buildroot}%{_prefix}/lib/zenclash/profile.yaml
install -Dpm0644 %{payload_dir}/recovery.yaml %{buildroot}%{_prefix}/lib/zenclash/recovery.yaml
install -Dpm0644 %{payload_dir}/zenclash.png %{buildroot}%{_datadir}/icons/hicolor/1024x1024/apps/zenclash.png
install -Dpm0644 %{payload_dir}/zenclash.desktop %{buildroot}%{_datadir}/applications/org.zenclash.ZenClash.desktop
install -Dpm0644 %{payload_dir}/LICENSE %{buildroot}%{_licensedir}/zenclash/LICENSE

%files
%license %{_licensedir}/zenclash/LICENSE
%{_bindir}/zenclash
%{_prefix}/lib/zenclash/zenclash-service
%{_prefix}/lib/zenclash/package-service.sh
%{_prefix}/lib/systemd/system/zenclash-service.service
%{_datadir}/polkit-1/actions/org.zenclash.service.policy
%{_prefix}/lib/zenclash/mihomo
%{_prefix}/lib/zenclash/geoip.metadb
%{_prefix}/lib/zenclash/profile.yaml
%{_prefix}/lib/zenclash/recovery.yaml
%{_datadir}/icons/hicolor/1024x1024/apps/zenclash.png
%{_datadir}/applications/org.zenclash.ZenClash.desktop

%post
set -e
. /usr/lib/zenclash/package-service.sh
zenclash_package_reload_manager

%preun
set -e
if [ "$1" -eq 0 ]; then
  . /usr/lib/zenclash/package-service.sh
  zenclash_package_pre_remove remove
fi

%postun
set -e
if [ -d /run/systemd/system ]; then
  /usr/bin/systemctl daemon-reload
fi

%changelog
* Tue Aug 25 2026 ZenClash contributors - %{app_version}-1
- Automated ZenClash release package
