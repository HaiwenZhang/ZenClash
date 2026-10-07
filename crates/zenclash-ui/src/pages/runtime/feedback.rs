use super::{Context, Page, RuntimeData, RuntimePage, Window};
use crate::components::feedback::{Feedback, FeedbackKind};

impl RuntimePage {
    pub(super) fn publish_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut messages = Vec::new();
        let mut add = |source, kind, message: Option<String>| {
            if let Some(message) = message {
                messages.push(Feedback::new(source, kind, message));
            }
        };
        add(
            "runtime-startup",
            FeedbackKind::Error,
            self.startup_error.clone(),
        );
        add("runtime-error", FeedbackKind::Error, self.error.clone());
        add("runtime-notice", FeedbackKind::Success, self.notice.clone());
        match self.page {
            Page::Home | Page::Settings => {
                add(
                    "home-action",
                    FeedbackKind::Error,
                    self.home.action_error.clone(),
                );
                add(
                    "home-proxy",
                    FeedbackKind::Error,
                    self.home.proxy_error.clone(),
                );
                add(
                    "service-tun",
                    FeedbackKind::Error,
                    self.profile_service.service_tun_warning(),
                );
                if self.profile_service.pending_finalization().is_some()
                    || self.profile_service.service_state().is_some_and(|state| {
                        state.phase() == zenclash_core::ServicePhase::Unconfirmed
                    })
                {
                    add(
                        "service-pending",
                        FeedbackKind::Warning,
                        Some(zenclash_i18n::text("core_page.service.pending")),
                    );
                }
                if self.page == Page::Settings {
                    add(
                        "app-update",
                        FeedbackKind::Warning,
                        self.app_update.error.clone(),
                    );
                    if let Some(zenclash_core::AppUpdateStatus::Available { release, .. }) =
                        &self.app_update.status
                    {
                        add(
                            "app-update-available",
                            FeedbackKind::Success,
                            Some(zenclash_i18n::text_with(
                                "settings.app_update.available",
                                &[("version", release.tag.clone())],
                            )),
                        );
                    }
                    if let RuntimeData::Settings {
                        autostart: Err(error),
                        ..
                    } = &self.data
                    {
                        add("autostart", FeedbackKind::Warning, Some(error.clone()));
                    }
                }
            }
            Page::Profiles => add(
                "subscription",
                FeedbackKind::Error,
                self.profiles.forms.subscription_error.clone(),
            ),
            Page::Network => {
                add(
                    "public-ip",
                    FeedbackKind::Error,
                    self.network_probe
                        .snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.public_ip_error.clone()),
                );
                if let RuntimeData::Network { system, .. } = &self.data {
                    add(
                        "system-network",
                        FeedbackKind::Warning,
                        system.error.clone(),
                    );
                }
            }
            Page::Mihomo => {
                add(
                    "core-release",
                    FeedbackKind::Error,
                    self.core_releases.error.clone(),
                );
                if self.core_session.runtime_descriptor().backend()
                    == zenclash_core::CoreRuntimeBackend::Service
                {
                    add(
                        "service-upgrade",
                        FeedbackKind::Warning,
                        Some(zenclash_i18n::text("core_page.maintenance.service_upgrade")),
                    );
                }
                if !self.loading && matches!(self.data, RuntimeData::Empty) {
                    add(
                        "core-unavailable",
                        FeedbackKind::Error,
                        Some(zenclash_i18n::text("core_page.status.no_placeholder")),
                    );
                }
            }
            Page::Tun => {
                add(
                    "service-tun",
                    FeedbackKind::Error,
                    self.profile_service.service_tun_warning(),
                );
                if let RuntimeData::Tun {
                    permissions:
                        zenclash_core::Observation::Failed { failure, .. }
                        | zenclash_core::Observation::Stale { failure, .. },
                    ..
                } = &self.data
                {
                    add(
                        "tun-permissions",
                        FeedbackKind::Warning,
                        Some(failure.message.clone()),
                    );
                }
            }
            _ => {}
        }
        let capabilities = self.core_kind.capabilities();
        let warning = match self.page {
            Page::Connections if !capabilities.udp_connection_tracking => {
                Some("connections.warnings.udp_tracking")
            }
            Page::Rules if !capabilities.rule_toggle => Some("rules.warnings.stats_unavailable"),
            Page::Resources if !capabilities.geodata_update || !capabilities.external_ui_update => {
                Some("resources.builtin.unsupported")
            }
            _ => None,
        };
        if let Some(key) = warning {
            add(
                "core-capability",
                FeedbackKind::Warning,
                Some(zenclash_i18n::text_with(
                    key,
                    &[("core", self.core_kind.display_name().to_owned())],
                )),
            );
        }
        if self.page == Page::Resources && !capabilities.ruleset_conversion {
            add(
                "ruleset-capability",
                FeedbackKind::Warning,
                Some(zenclash_i18n::text_with(
                    "resources.ruleset.unsupported",
                    &[("core", self.core_kind.display_name().to_owned())],
                )),
            );
        }
        if self.page == Page::Override
            && self
                .overrides
                .preview
                .as_ref()
                .is_some_and(|preview| preview.is_truncated())
        {
            add(
                "config-preview",
                FeedbackKind::Warning,
                Some(zenclash_i18n::text("overrides.preview.truncated")),
            );
        }
        self.feedback_notifications.publish(messages, window, cx);
    }
}
