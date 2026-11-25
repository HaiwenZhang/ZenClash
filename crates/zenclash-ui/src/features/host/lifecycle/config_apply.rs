//! Controlled configuration application, profile receipt synchronization, and external callbacks.

use gpui_kit::{Context, Window};
use serde_json::Value;
use zenclash_core::{AppPreferences, CoreApplyKind, MihomoLogLevel};

use crate::features::host::{ProfileActivated, ProfileCleared, RuntimeConfigApplied, RuntimePage};
use crate::foundation::actions::Page;
use crate::foundation::primitives::preference_previews::PreferencePreviews;

impl RuntimePage {
    pub(crate) fn report_automatic_runtime(
        &mut self,
        notice: Option<&str>,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(notice) = notice {
            self.notice = Some(zenclash_i18n::text(notice));
            self.invalidate_config_inputs(cx);
            self.refresh(cx);
        }
        if let Some(error) = error {
            self.error = Some(zenclash_i18n::text_with(
                "automatic.failed",
                &[("error", error)],
            ));
        }
        cx.notify();
    }

    pub(crate) fn preferences_restored_from_app(
        &mut self,
        preferences: AppPreferences,
        previews: PreferencePreviews,
        cx: &mut Context<Self>,
    ) {
        self.preferences = preferences;
        self.settings.preference_previews = previews;
        self.settings
            .preference_previews
            .apply(&mut self.preferences);
        cx.notify();
    }

    /// Refreshes placeholders stored inside long-lived input entities after a
    /// process-wide locale change. Regular page labels update on the next render,
    /// while `InputState` requires an explicit update.
    pub(crate) fn refresh_localized_placeholders(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.connections.update(cx, |view, cx| {
            view.refresh_localized_placeholder(window, cx)
        });
        self.logs.update(cx, |view, cx| {
            view.refresh_localized_placeholder(window, cx)
        });
        self.rules.update(cx, |view, cx| {
            view.refresh_localized_placeholder(window, cx)
        });
        let localized_inputs = [
            (
                self.network_diagnostics.latency_name.clone(),
                "runtime.placeholders.network_target",
            ),
            (
                self.network_diagnostics.dns_name.clone(),
                "runtime.placeholders.dns_name",
            ),
            (
                self.config_inputs.core.mixed_port.clone(),
                "config_inputs.placeholders.mixed_port",
            ),
            (
                self.config_inputs.core.interface_name.clone(),
                "config_inputs.placeholders.automatic_interface",
            ),
            (
                self.config_inputs.tun.device.clone(),
                "config_inputs.placeholders.tun_device",
            ),
            (
                self.config_inputs.tun.mtu.clone(),
                "config_inputs.placeholders.default_mtu",
            ),
        ];
        for (input, key) in localized_inputs {
            input.update(cx, |input, cx| {
                input.set_placeholder(zenclash_i18n::text(key), window, cx);
            });
        }
        let localized_textareas = [
            (
                self.config_inputs.dns.fake_ip_filter.clone(),
                "config_inputs.placeholders.one_domain_or_rule",
            ),
            (
                self.config_inputs.dns.default_nameserver.clone(),
                "config_inputs.placeholders.one_ip_dns",
            ),
            (
                self.config_inputs.dns.nameserver.clone(),
                "config_inputs.placeholders.one_dns",
            ),
            (
                self.config_inputs.dns.proxy_server_nameserver.clone(),
                "config_inputs.placeholders.proxy_resolver",
            ),
            (
                self.config_inputs.dns.direct_nameserver.clone(),
                "config_inputs.placeholders.direct_resolver",
            ),
            (
                self.config_inputs.dns.fallback_ipcidr.clone(),
                "config_inputs.placeholders.one_cidr",
            ),
            (
                self.config_inputs.dns.fallback_domain.clone(),
                "config_inputs.placeholders.one_domain_rule",
            ),
            (
                self.config_inputs.dns.nameserver_policy.clone(),
                "config_inputs.placeholders.dns_mapping",
            ),
            (
                self.config_inputs.dns.hosts.clone(),
                "config_inputs.placeholders.address_mapping",
            ),
            (
                self.config_inputs.sniffer.skip_domain.clone(),
                "config_inputs.placeholders.one_domain",
            ),
            (
                self.config_inputs.sniffer.force_domain.clone(),
                "config_inputs.placeholders.one_domain",
            ),
            (
                self.config_inputs.sniffer.skip_dst_address.clone(),
                "config_inputs.placeholders.one_address",
            ),
            (
                self.config_inputs.sniffer.skip_src_address.clone(),
                "config_inputs.placeholders.one_address",
            ),
            (
                self.config_inputs.tun.route_include_address.clone(),
                "config_inputs.placeholders.one_cidr",
            ),
            (
                self.config_inputs.tun.route_exclude_address.clone(),
                "config_inputs.placeholders.one_cidr",
            ),
        ];
        for (input, key) in localized_textareas {
            input.update(cx, |input, cx| {
                input.set_placeholder(zenclash_i18n::text(key), window, cx);
            });
        }
        self.profiles
            .forms
            .refresh_localized_placeholders(window, cx);
        self.profiles
            .forms
            .yaml_editor
            .refresh_localized_placeholder(window, cx);
        cx.notify();
    }

    pub(crate) fn report_system_proxy_reconcile_error(
        &mut self,
        error: &str,
        cx: &mut Context<Self>,
    ) {
        self.error = Some(zenclash_i18n::text_with(
            "runtime.lifecycle.proxy_reconcile",
            &[("error", error.to_owned())],
        ));
        cx.notify();
    }

    pub(crate) fn profile_updated_in_background(
        &mut self,
        outcome: crate::application_commands::ProfileReceipt,
        cx: &mut Context<Self>,
    ) {
        let name = outcome.name().to_owned();
        self.synchronize_profile_receipt(&outcome, cx);
        self.notice = Some(outcome.warning().unwrap_or_else(|| {
            zenclash_i18n::text_with("runtime.lifecycle.profile_updated", &[("name", name)])
        }));
        cx.notify();
    }

    pub(in crate::features) fn synchronize_profile_receipt(
        &mut self,
        receipt: &crate::application_commands::ProfileReceipt,
        cx: &mut Context<Self>,
    ) -> bool {
        self.reload_profile_catalog(cx);
        self.synchronize_profile_recovery();
        let Some(runtime_version) = receipt.runtime_version() else {
            return false;
        };
        let accepted = self.application_commands.is_current(runtime_version);
        // A later mode/upgrade transition may invalidate the receipt without
        // changing its profile. Publish current core truth instead of losing
        // the only path notification from that completed profile transaction.
        self.synchronize_committed_profile(cx);
        accepted
    }

    pub(in crate::features) fn synchronize_committed_profile(&mut self, cx: &mut Context<Self>) {
        self.synchronize_committed_profile_state(false, cx);
    }

    pub(in crate::features) fn synchronize_cleared_or_fallback_profile(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        self.synchronize_committed_profile_state(true, cx);
    }

    fn synchronize_committed_profile_state(
        &mut self,
        emit_when_cleared: bool,
        cx: &mut Context<Self>,
    ) {
        self.synchronize_profile_recovery();
        let snapshot = self.application_commands.committed_profile();
        match snapshot.profile_path {
            Some(path) => {
                self.profile_path = Some(path.clone());
                self.invalidate_config_inputs(cx);
                self.library.overrides.invalidate_preview();
                cx.emit(ProfileActivated {
                    path,
                    runtime_version: snapshot.generation,
                });
            }
            None if emit_when_cleared => {
                self.profile_path = None;
                self.invalidate_config_inputs(cx);
                self.library.overrides.invalidate_preview();
                cx.emit(ProfileCleared {
                    runtime_version: snapshot.generation,
                });
            }
            None => {}
        }
    }

    pub(in crate::features) fn synchronize_profile_recovery(&mut self) {
        self.profiles
            .synchronize_recovery(&self.application_commands);
    }

    pub(crate) fn report_background_profile_error(&mut self, error: &str, cx: &mut Context<Self>) {
        self.synchronize_profile_recovery();
        tracing::warn!(%error, "automatic profile update failed");
        if self.page == Page::Profiles {
            self.error = Some(zenclash_i18n::text_with(
                "runtime.lifecycle.profile_update_failed",
                &[("error", error.to_owned())],
            ));
            cx.notify();
        }
    }

    pub(in crate::features) fn apply_controlled_config(
        &mut self,
        patch: Value,
        success: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        if !self
            .config_inputs
            .is_for_profile(self.profile_path.as_deref())
        {
            self.error = Some(zenclash_i18n::text("runtime.empty.loading"));
            cx.notify();
            return;
        }
        let success = success.into();
        let requested_log_level = patch
            .get("log-level")
            .and_then(Value::as_str)
            .and_then(MihomoLogLevel::from_api);
        let Some(profile) = self.profile_path.clone() else {
            self.error = Some(zenclash_i18n::text("runtime.lifecycle.profile_missing"));
            cx.notify();
            return;
        };
        let page = self.page;
        let Some(token) = self.begin_mutation(page) else {
            return;
        };
        let submitted_inputs = self.config_inputs.submitted(&patch, cx);
        let controlled = self.controlled_config_store.clone();
        let commands = self.application_commands.clone();
        let task = self.runtime.spawn(async move {
            let outcome = commands
                .apply_controlled_patch(&controlled, profile, patch)
                .await
                .map_err(|error| error.to_string())?;
            Ok::<_, String>(outcome)
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "runtime.lifecycle.config_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(outcome) => {
                        if !this.application_commands.is_current(outcome.generation) {
                            this.reload_controlled_config(cx);
                            this.invalidate_config_inputs(cx);
                            this.invalidate_page_load();
                            this.refresh(cx);
                            return;
                        }
                        this.config_inputs.accept_submitted(submitted_inputs, cx);
                        if let Some(level) = requested_log_level {
                            this.monitors.log_monitor.set_level(level);
                        }
                        this.reload_controlled_config(cx);
                        this.library.overrides.invalidate_preview();
                        cx.emit(RuntimeConfigApplied);
                        this.invalidate_page_load();
                        this.refresh(cx);
                        if this.is_page_task_current(token) {
                            this.notice = Some(if outcome.kind == CoreApplyKind::Restarted {
                                let saved = success.replace(
                                    &zenclash_i18n::text("runtime.lifecycle.hot_reload_term"),
                                    &zenclash_i18n::text("runtime.lifecycle.save_term"),
                                );
                                zenclash_i18n::text_with(
                                    "runtime.lifecycle.restarted_after_save",
                                    &[
                                        ("message", saved),
                                        ("core", this.core_kind.display_name().to_owned()),
                                    ],
                                )
                            } else {
                                success
                            });
                        }
                    }
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(in crate::features) fn controlled_bool(&self, pointer: &str, fallback: bool) -> bool {
        self.config_inputs
            .effective()
            .pointer(pointer)
            .and_then(Value::as_bool)
            .unwrap_or(fallback)
    }

    pub(crate) fn reload_controlled_config(&mut self, cx: &mut Context<Self>) {
        self.settings.controlled_config_generation =
            self.settings.controlled_config_generation.wrapping_add(1);
        let generation = self.settings.controlled_config_generation;
        let controlled = self.controlled_config_store.clone();
        let task = self
            .runtime
            .spawn_blocking(move || controlled.load_json().map_err(|error| error.to_string()));
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                if this.settings.controlled_config_generation != generation {
                    return;
                }
                match result {
                    Ok(controlled) => {
                        this.settings.controlled_config = controlled;
                        this.invalidate_config_inputs(cx);
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn profile_activated_from_tray(
        &mut self,
        receipt: crate::application_commands::ProfileReceipt,
        cx: &mut Context<Self>,
    ) {
        let name = receipt.name().to_owned();
        if self.synchronize_profile_receipt(&receipt, cx) {
            self.notice = Some(receipt.warning().unwrap_or_else(|| {
                zenclash_i18n::text_with(
                    "runtime.lifecycle.tray_profile_selected",
                    &[("name", name)],
                )
            }));
            self.refresh(cx);
        }
        cx.notify();
    }

    pub(crate) fn report_tray_profile_error(&mut self, error: &str, cx: &mut Context<Self>) {
        self.synchronize_profile_recovery();
        self.error = Some(zenclash_i18n::text_with(
            "runtime.lifecycle.tray_profile_failed",
            &[("error", error.to_owned())],
        ));
        cx.notify();
    }
}
