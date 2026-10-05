// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelIdentity {
    pub id: &'static str,
    pub service_slug: &'static str,
    pub windows_service_name: &'static str,
    pub service_display_name: &'static str,
    pub macos_app_bundle_id: &'static str,
    pub macos_service_id: &'static str,
}

#[cfg(not(feature = "development-channel"))]
pub const CHANNEL_IDENTITY: ChannelIdentity = ChannelIdentity {
    id: "production",
    service_slug: "zenclash-service",
    windows_service_name: "zenclash_service",
    service_display_name: "ZenClash Service",
    macos_app_bundle_id: "org.zenclash.app",
    macos_service_id: "org.zenclash.app.service",
};

#[cfg(feature = "development-channel")]
pub const CHANNEL_IDENTITY: ChannelIdentity = ChannelIdentity {
    id: "development",
    service_slug: "zenclash-service-dev",
    windows_service_name: "zenclash_service_dev",
    service_display_name: "ZenClash Development Service",
    macos_app_bundle_id: "org.zenclash.app.dev",
    macos_service_id: "org.zenclash.app.dev.service",
};

pub const SERVICE_SLUG: &str = CHANNEL_IDENTITY.service_slug;
pub const WINDOWS_SERVICE_NAME: &str = CHANNEL_IDENTITY.windows_service_name;
pub const SERVICE_DISPLAY_NAME: &str = CHANNEL_IDENTITY.service_display_name;
pub const MACOS_APP_BUNDLE_ID: &str = CHANNEL_IDENTITY.macos_app_bundle_id;
pub const MACOS_SERVICE_ID: &str = CHANNEL_IDENTITY.macos_service_id;

#[cfg(test)]
mod tests {
    use super::CHANNEL_IDENTITY;

    #[test]
    fn compiled_channel_has_a_self_consistent_identity() {
        assert!(!CHANNEL_IDENTITY.id.is_empty());
        assert!(CHANNEL_IDENTITY.service_slug.starts_with("zenclash-service"));
        assert!(CHANNEL_IDENTITY.windows_service_name.starts_with("zenclash_service"));
        assert!(
            CHANNEL_IDENTITY
                .macos_service_id
                .starts_with(CHANNEL_IDENTITY.macos_app_bundle_id)
        );
    }
}
