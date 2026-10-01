use super::{
    super::{
        AppContext, Context, Page, PageTaskToken, PathBuf, PathPromptOptions, RemoteProfileOptions,
        RuntimePage, Window, load_page,
    },
    workflow,
};

mod catalog;

impl RuntimePage {
    pub(super) fn reload_profile(&mut self, cx: &mut Context<Self>) {
        if self.profile_path.is_none() {
            self.error = Some(zenclash_i18n::text("profiles.errors.profile_path_missing"));
            cx.notify();
            return;
        }
        let Some(token) = self.begin_mutation(Page::Profiles) else {
            return;
        };
        let client = self.client.clone();
        let controlled = self.controlled_config_store.clone();
        let core_runtime = self.profile_service.clone();
        let task = self.runtime.spawn(async move {
            let outcome = workflow::reload_effective(controlled, &core_runtime).await?;
            load_page(client, Page::Profiles)
                .await
                .map(|data| (outcome.generation, data))
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(zenclash_i18n::text_with(
                    "profiles.errors.reload_task",
                    &[("error", error.to_string())],
                )),
            };
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok((runtime_version, data)) => {
                        this.synchronize_committed_profile(cx);
                        if !this.profile_service.is_current(runtime_version) {
                            this.refresh(cx);
                            return;
                        }
                        if this.replace_page_data(token, data, cx) {
                            this.notice =
                                Some(if this.core_kind.capabilities().full_config_reload {
                                    zenclash_i18n::text_with(
                                        "profiles.notices.reloaded",
                                        &[("core", this.core_kind.display_name().to_owned())],
                                    )
                                } else {
                                    zenclash_i18n::text_with(
                                        "profiles.notices.restarted",
                                        &[("core", this.core_kind.display_name().to_owned())],
                                    )
                                });
                        }
                    }
                    Err(error) => this.set_profile_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(in super::super) fn reload_profile_catalog(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.profiles.store.clone() else {
            return;
        };
        let generation = self.profiles.begin_read();
        let task = self
            .runtime
            .spawn_blocking(move || store.load().map_err(|error| error.to_string()));
        self.profiles.read_task.replace(&task);
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                if this.profiles.generation != generation {
                    return;
                }
                match result {
                    Ok(catalog) => {
                        this.profiles.accept_catalog(generation, catalog);
                    }
                    Err(error) if this.page == Page::Profiles => this.error = Some(error),
                    Err(error) => {
                        tracing::warn!(%error, "failed to reload profile catalog");
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn import_local_profile_for_page(&mut self, path: PathBuf, page: Page, cx: &mut Context<Self>) {
        let Some(store) = self.profiles.store.clone() else {
            let error = zenclash_i18n::text("profiles.errors.store_unavailable");
            if page == Page::Home {
                self.home.action_error = Some(error);
            } else {
                self.error = Some(error);
            }
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(page) else {
            return;
        };
        if page == Page::Home {
            self.home.profile_switching = Some("local-import".into());
            self.home.action_error = None;
        }
        let controlled = self.controlled_config_store.clone();
        let core_runtime = self.profile_service.clone();
        let task = self.runtime.spawn(workflow::import_local(
            store,
            controlled,
            core_runtime,
            path,
            page,
        ));
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "profiles.errors.import_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                this.home.profile_switching = None;
                match result {
                    Ok(outcome) => this.apply_profile_activation(
                        outcome,
                        |name| {
                            zenclash_i18n::text_with(
                                "profiles.notices.imported",
                                &[("name", name.to_owned())],
                            )
                        },
                        token,
                        cx,
                    ),
                    Err(error) => this.set_profile_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn add_remote_profile(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.profiles.store.clone() else {
            self.profiles.forms.subscription_error =
                Some(zenclash_i18n::text("profiles.errors.store_unavailable"));
            cx.notify();
            return;
        };
        if self.core_busy() {
            return;
        }
        self.profiles.forms.subscription_error = None;
        let name = self
            .profiles
            .forms
            .subscription_name
            .read(cx)
            .value()
            .to_string();
        let url = self
            .profiles
            .forms
            .subscription_url
            .read(cx)
            .value()
            .to_string();
        let user_agent = self
            .profiles
            .forms
            .subscription_user_agent
            .read(cx)
            .value()
            .to_string();
        let authorization = self
            .profiles
            .forms
            .subscription_authorization
            .read(cx)
            .value()
            .to_string();
        if name.trim().is_empty() || url.trim().is_empty() {
            self.profiles.forms.subscription_error =
                Some(zenclash_i18n::text("profiles.errors.required_fields"));
            cx.notify();
            return;
        }
        let options = match RemoteProfileOptions::new(authorization, false) {
            Ok(options) => options.with_route(self.profiles.forms.subscription_route),
            Err(error) => {
                self.profiles.forms.subscription_error = Some(zenclash_i18n::text_with(
                    "profiles.errors.request_invalid",
                    &[("error", error)],
                ));
                cx.notify();
                return;
            }
        };
        let Some(token) = self.begin_mutation(Page::Profiles) else {
            return;
        };
        let controlled = self.controlled_config_store.clone();
        let core_runtime = self.profile_service.clone();
        let task = self.runtime.spawn(workflow::add_remote(
            store,
            controlled,
            core_runtime,
            name,
            url,
            user_agent,
            options,
        ));
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "profiles.errors.remote_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(outcome) => {
                        this.profiles.forms.subscription_error = None;
                        this.profiles.forms.adding_subscription = false;
                        this.apply_profile_activation(
                            outcome,
                            |name| {
                                zenclash_i18n::text_with(
                                    "profiles.notices.added",
                                    &[("name", name.to_owned())],
                                )
                            },
                            token,
                            cx,
                        );
                    }
                    Err(error) => this.profiles.forms.subscription_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn activate_managed_profile(&mut self, id: String, cx: &mut Context<Self>) {
        self.activate_managed_profile_for_page(id, Page::Profiles, cx);
    }

    pub(in crate::pages::runtime) fn activate_home_profile(
        &mut self,
        id: String,
        cx: &mut Context<Self>,
    ) {
        self.activate_managed_profile_for_page(id, Page::Home, cx);
    }

    fn activate_managed_profile_for_page(
        &mut self,
        id: String,
        page: Page,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.profiles.store.clone() else {
            return;
        };
        let Some(token) = self.begin_mutation(page) else {
            return;
        };
        if page == Page::Home {
            self.home.profile_switching = Some(id.clone());
            self.home.action_error = None;
        }
        let controlled = self.controlled_config_store.clone();
        let core_runtime = self.profile_service.clone();
        let task = self.runtime.spawn(workflow::activate_existing_for_page(
            store,
            controlled,
            core_runtime,
            id,
            page,
        ));
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "profiles.errors.activate_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                this.home.profile_switching = None;
                match result {
                    Ok(outcome) => this.apply_profile_activation(
                        outcome,
                        |name| {
                            zenclash_i18n::text_with(
                                "profiles.notices.activated",
                                &[("name", name.to_owned())],
                            )
                        },
                        token,
                        cx,
                    ),
                    Err(error) => this.set_profile_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn choose_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.choose_profile_for_page(Page::Profiles, window, cx);
    }

    fn choose_profile_for_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        let token = self.page_task_token_for(page);
        let restore_focus = window.focused(cx);
        let window_handle = window.window_handle();
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(zenclash_i18n::text("profiles.dialog.choose_yaml").into()),
        });
        cx.spawn(async move |this, cx| {
            let selection = receiver.await;
            let _ = this.update(cx, |this, cx| match selection {
                Ok(Ok(Some(paths))) => {
                    if this.is_page_task_current(token) {
                        if let Some(path) = paths.into_iter().next() {
                            this.import_local_profile_for_page(path, page, cx);
                        }
                    } else {
                        tracing::info!("discarded profile selection after leaving profile page");
                    }
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    this.set_page_error(
                        token,
                        zenclash_i18n::text_with(
                            "profiles.errors.chooser",
                            &[("error", error.to_string())],
                        ),
                    );
                    cx.notify();
                }
                Err(error) => {
                    this.set_page_error(
                        token,
                        zenclash_i18n::text_with(
                            "profiles.errors.chooser_task",
                            &[("error", error.to_string())],
                        ),
                    );
                    cx.notify();
                }
            });
            if let Some(focus) = restore_focus {
                let _ = cx.update_window(window_handle, |_, window, cx| focus.focus(window, cx));
            }
        })
        .detach();
    }

    fn apply_profile_activation(
        &mut self,
        outcome: workflow::ActivationOutcome,
        notice: impl FnOnce(&str) -> String,
        token: PageTaskToken,
        cx: &mut Context<Self>,
    ) {
        if !self.synchronize_profile_receipt(&outcome.receipt, cx) {
            return;
        }
        match outcome.refresh {
            Ok(data) => {
                if self.replace_page_data(token, data, cx) {
                    if token.page == Page::Home {
                        self.home.action_error = None;
                    }
                    self.notice = Some(notice(&outcome.name));
                }
            }
            Err(error) => {
                self.set_profile_page_error(
                    token,
                    zenclash_i18n::text_with(
                        "profiles.errors.enabled_refresh",
                        &[("error", error)],
                    ),
                );
            }
        }
    }

    fn set_profile_page_error(&mut self, token: PageTaskToken, error: String) {
        self.synchronize_profile_recovery();
        if token.page == Page::Home && self.is_page_task_current(token) {
            self.home.action_error = Some(error);
        } else {
            self.set_page_error(token, error);
        }
    }
}
