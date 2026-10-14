use super::{Context, Page, PageSnapshotData, RuntimePage, Window};
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
                    self.application_commands.service_tun_warning(),
                );
                if self.application_commands.pending_finalization().is_some()
                    || self
                        .application_commands
                        .service_state()
                        .is_some_and(|state| {
                            state.phase() == zenclash_core::ServicePhase::Unconfirmed
                        })
                {
                    add(
                        "service-pending",
                        FeedbackKind::Warning,
                        Some(zenclash_i18n::text("core_page.service.pending")),
                    );
                }
                if self.page == Page::Settings
                    && let PageSnapshotData::Settings {
                        autostart: Err(error),
                        ..
                    } = self.page_observation.data()
                {
                    add("autostart", FeedbackKind::Warning, Some(error.clone()));
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
                    self.network_diagnostics
                        .query
                        .snapshot()
                        .and_then(|snapshot| snapshot.public_ip_error.clone()),
                );
                if let PageSnapshotData::Network { system, .. } = self.page_observation.data() {
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
                    self.core_releases.error().clone(),
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
                if !self.loading && matches!(self.page_observation.data(), PageSnapshotData::Empty)
                {
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
                    self.application_commands.service_tun_warning(),
                );
                if let PageSnapshotData::Tun {
                    permissions:
                        zenclash_core::Observation::Failed { failure, .. }
                        | zenclash_core::Observation::Stale { failure, .. },
                    ..
                } = self.page_observation.data()
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
                .preview()
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
