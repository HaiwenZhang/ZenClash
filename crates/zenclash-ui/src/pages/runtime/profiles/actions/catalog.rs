use zenclash_core::ProfileSource;

use super::{Context, Page, RemoteProfileOptions, RuntimePage, Window, workflow};

impl RuntimePage {
    pub(in super::super) fn begin_edit_remote_profile(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = self
            .profiles
            .catalog
            .profiles
            .iter()
            .find(|profile| profile.id == id)
        else {
            self.error = Some(zenclash_i18n::text("profiles.errors.remote_not_found"));
            cx.notify();
            return;
        };
        let ProfileSource::Remote {
            url,
            user_agent,
            options,
        } = &profile.source
        else {
            self.error = Some(zenclash_i18n::text(
                "profiles.errors.local_request_unavailable",
            ));
            cx.notify();
            return;
        };
        let authorization = options
            .authorization
            .as_ref()
            .map_or_else(String::new, |value| value.expose_secret().to_owned());
        let update_cron = profile.update_cron.clone().unwrap_or_default();
        let timeout_seconds = options.download_timeout_seconds.to_string();
        let name = profile.name.clone();
        let url = url.clone();
        let user_agent = user_agent.clone();
        self.profiles
            .forms
            .request_name
            .update(cx, |input, cx| input.set_value(name, window, cx));
        self.profiles
            .forms
            .request_url
            .update(cx, |input, cx| input.set_value(url, window, cx));
        self.profiles
            .forms
            .request_user_agent
            .update(cx, |input, cx| input.set_value(user_agent, window, cx));
        self.profiles
            .forms
            .request_authorization
            .update(cx, |input, cx| input.set_value(authorization, window, cx));
        self.profiles
            .forms
            .request_timeout_seconds
            .update(cx, |input, cx| input.set_value(timeout_seconds, window, cx));
        self.profiles.forms.update_cron.update(cx, |input, cx| {
            input.set_value(update_cron, window, cx);
        });
        self.profiles.forms.editing_route = options.route();
        self.profiles.forms.editing_fixed_update_interval = options.fixed_update_interval;
        self.profiles.forms.editing_profile_id = Some(id);
        self.error = None;
        cx.notify();
    }

    pub(in super::super) fn cancel_edit_remote_profile(&mut self, cx: &mut Context<Self>) {
        self.profiles.forms.editing_profile_id = None;
        cx.notify();
    }

    pub(in super::super) fn save_remote_profile_settings(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.profiles.forms.editing_profile_id.clone() else {
            return;
        };
        let Some(store) = self.profiles.store.clone() else {
            self.error = Some(zenclash_i18n::text("profiles.errors.store_unavailable"));
            cx.notify();
            return;
        };
        let authorization = self
            .profiles
            .forms
            .request_authorization
            .read(cx)
            .value()
            .to_string();
        let name = self
            .profiles
            .forms
            .request_name
            .read(cx)
            .value()
            .to_string();
        let url = self.profiles.forms.request_url.read(cx).value().to_string();
        let user_agent = self
            .profiles
            .forms
            .request_user_agent
            .read(cx)
            .value()
            .to_string();
        let timeout_seconds = match self
            .profiles
            .forms
            .request_timeout_seconds
            .read(cx)
            .value()
            .trim()
            .parse::<u32>()
        {
            Ok(value) => value,
            Err(error) => {
                self.error = Some(zenclash_i18n::text_with(
                    "profiles.errors.timeout_integer",
                    &[("error", error.to_string())],
                ));
                cx.notify();
                return;
            }
        };
        let options = RemoteProfileOptions::new(authorization, false)
            .map(|options| options.with_route(self.profiles.forms.editing_route))
            .and_then(|options| {
                options.with_download_policy(
                    timeout_seconds,
                    self.profiles.forms.editing_fixed_update_interval,
                )
            });
        let options = match options {
            Ok(options) => options,
            Err(error) => {
                self.error = Some(zenclash_i18n::text_with(
                    "profiles.errors.request_invalid",
                    &[("error", error)],
                ));
                cx.notify();
                return;
            }
        };
        let cron = self.profiles.forms.update_cron.read(cx).value().to_string();
        let cron = (!cron.trim().is_empty()).then_some(cron);
        let Some(token) = self.begin_mutation(Page::Profiles) else {
            return;
        };
        let task = self.runtime.spawn_blocking(move || {
            store
                .set_remote_request_settings(&id, &name, &url, &user_agent, options, cron)
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "profiles.errors.request_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(()) => {
                        this.reload_profile_catalog(cx);
                        this.profiles.forms.editing_profile_id = None;
                        if this.is_page_task_current(token) {
                            this.notice =
                                Some(zenclash_i18n::text("profiles.notices.request_saved"));
                        }
                    }
                    Err(error) => {
                        this.synchronize_profile_recovery();
                        this.set_page_error(token, error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(in super::super) fn set_profile_update_policy(
        &mut self,
        id: String,
        enabled: bool,
        interval_minutes: u32,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.profiles.store.clone() else {
            return;
        };
        let Some(token) = self.begin_mutation(Page::Profiles) else {
            return;
        };
        let task = self.runtime.spawn(async move {
            tokio::task::spawn_blocking(move || {
                store.set_update_policy(&id, enabled, interval_minutes)
            })
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "profiles.errors.policy_task",
                    &[("error", error.to_string())],
                )
            })?
            .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "profiles.errors.policy_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(()) => {
                        this.reload_profile_catalog(cx);
                        if this.is_page_task_current(token) {
                            this.notice = Some(if enabled {
                                zenclash_i18n::text_with(
                                    "profiles.notices.auto_update_enabled",
                                    &[("minutes", interval_minutes.to_string())],
                                )
                            } else {
                                zenclash_i18n::text("profiles.notices.auto_update_disabled")
                            });
                        }
                    }
                    Err(error) => {
                        this.synchronize_profile_recovery();
                        this.set_page_error(token, error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(in super::super) fn update_managed_profile(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(store) = self.profiles.store.clone() else {
            return;
        };
        let Some(token) = self.begin_mutation(Page::Profiles) else {
            return;
        };
        let controlled = self.controlled_config_store.clone();
        let core_runtime = self.profile_service.clone();
        let task = self
            .runtime
            .spawn(workflow::update_remote(store, controlled, core_runtime, id));
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "profiles.errors.update_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(outcome) => {
                        let accepted = this.synchronize_profile_receipt(&outcome.receipt, cx);
                        if !accepted && outcome.receipt.runtime_version().is_some() {
                            this.refresh(cx);
                            cx.notify();
                            return;
                        }
                        if !this.profile_service.is_current(outcome.refresh_version) {
                            this.refresh(cx);
                            cx.notify();
                            return;
                        }
                        let is_profile_page = match outcome.refresh {
                            Ok(data) => this.replace_page_data(token, data, cx),
                            Err(error) => {
                                this.set_page_error(
                                    token,
                                    zenclash_i18n::text_with(
                                        "profiles.errors.updated_refresh",
                                        &[("error", error)],
                                    ),
                                );
                                false
                            }
                        };
                        this.reload_profile_catalog(cx);
                        if is_profile_page {
                            this.notice = Some(outcome.receipt.warning().unwrap_or_else(|| {
                                zenclash_i18n::text_with(
                                    "profiles.notices.updated",
                                    &[("name", outcome.receipt.name().to_owned())],
                                )
                            }));
                        }
                    }
                    Err(error) => {
                        this.synchronize_profile_recovery();
                        this.set_page_error(token, error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(in super::super) fn delete_managed_profile(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(store) = self.profiles.store.clone() else {
            return;
        };
        let Some(token) = self.begin_mutation(Page::Profiles) else {
            return;
        };
        let task = self.runtime.spawn(workflow::delete(store, id));
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "profiles.errors.delete_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(()) => {
                        this.reload_profile_catalog(cx);
                        if this.is_page_task_current(token) {
                            this.notice = Some(zenclash_i18n::text("profiles.notices.deleted"));
                        }
                    }
                    Err(error) => {
                        this.synchronize_profile_recovery();
                        this.set_page_error(token, error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
