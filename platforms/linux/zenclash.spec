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
install -Dpm0755 %{payload_dir}/mihomo %{buildroot}%{_prefix}/lib/zenclash/mihomo
install -Dpm0644 %{payload_dir}/profile.yaml %{buildroot}%{_prefix}/lib/zenclash/profile.yaml
install -Dpm0644 %{payload_dir}/recovery.yaml %{buildroot}%{_prefix}/lib/zenclash/recovery.yaml
install -Dpm0644 %{payload_dir}/zenclash.png %{buildroot}%{_datadir}/icons/hicolor/1024x1024/apps/zenclash.png
install -Dpm0644 %{payload_dir}/zenclash.desktop %{buildroot}%{_datadir}/applications/org.zenclash.ZenClash.desktop
install -Dpm0644 %{payload_dir}/LICENSE %{buildroot}%{_licensedir}/zenclash/LICENSE

%files
%license %{_licensedir}/zenclash/LICENSE
%{_bindir}/zenclash
%{_prefix}/lib/zenclash/mihomo
%{_prefix}/lib/zenclash/profile.yaml
%{_prefix}/lib/zenclash/recovery.yaml
%{_datadir}/icons/hicolor/1024x1024/apps/zenclash.png
%{_datadir}/applications/org.zenclash.ZenClash.desktop

%changelog
* Tue Aug 25 2026 ZenClash contributors - %{app_version}-1
- Automated ZenClash release package
