use super::{
    super::{
        Context, Page, PageTaskToken, PathBuf, PathPromptOptions, ProfileActivated,
        RemoteProfileOptions, RuntimePage, Window, load_page,
    },
    workflow,
};

mod catalog;

impl RuntimePage {
    pub(super) fn reload_profile(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.profile_path.clone() else {
            self.error = Some(zenclash_i18n::text("profiles.errors.profile_path_missing"));
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(Page::Profiles) else {
            return;
        };
        let client = self.client.clone();
        let controlled = self.controlled_config_store.clone();
        let core_runtime = workflow::CoreProfileRuntime::new(self.core_session.clone());
        let task = self.runtime.spawn(async move {
            workflow::reload_effective(controlled, &core_runtime, &path).await?;
            load_page(client, Page::Profiles).await
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
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
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
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(in super::super) fn reload_profile_catalog(&mut self) -> Result<(), String> {
        let Some(store) = &self.profile_store else {
            return Ok(());
        };
        match store.load() {
            Ok(catalog) => {
                self.profile_catalog = catalog;
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn import_local_profile(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(store) = self.profile_store.clone() else {
            self.error = Some(zenclash_i18n::text("profiles.errors.store_unavailable"));
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(Page::Profiles) else {
            return;
        };
        let controlled = self.controlled_config_store.clone();
        let core_runtime = workflow::CoreProfileRuntime::new(self.core_session.clone());
        let task = self.runtime.spawn(workflow::import_local(
            store,
            controlled,
            core_runtime,
            path,
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
                this.mutating = false;
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
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn add_remote_profile(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.profile_store.clone() else {
            self.error = Some(zenclash_i18n::text("profiles.errors.store_unavailable"));
            cx.notify();
            return;
        };
        if self.mutating {
            return;
        }
        let name = self
            .profile_forms
            .subscription_name
            .read(cx)
            .value()
            .to_string();
        let url = self
            .profile_forms
            .subscription_url
            .read(cx)
            .value()
            .to_string();
        let user_agent = self
            .profile_forms
            .subscription_user_agent
            .read(cx)
            .value()
            .to_string();
        let authorization = self
            .profile_forms
            .subscription_authorization
            .read(cx)
            .value()
            .to_string();
        if name.trim().is_empty() || url.trim().is_empty() {
            self.error = Some(zenclash_i18n::text("profiles.errors.required_fields"));
            cx.notify();
            return;
        }
        let options = match RemoteProfileOptions::new(authorization, false) {
            Ok(options) => options.with_route(self.profile_forms.subscription_route),
            Err(error) => {
                self.error = Some(zenclash_i18n::text_with(
                    "profiles.errors.request_invalid",
                    &[("error", error.to_string())],
                ));
                cx.notify();
                return;
            }
        };
        let Some(token) = self.begin_mutation(Page::Profiles) else {
            return;
        };
        let controlled = self.controlled_config_store.clone();
        let core_runtime = workflow::CoreProfileRuntime::new(self.core_session.clone());
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
                this.mutating = false;
                match result {
                    Ok(outcome) => {
                        this.profile_forms.adding_subscription = false;
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
                    Err(error) => this.set_page_error(token, error),
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
        let Some(store) = self.profile_store.clone() else {
            return;
        };
        let Some(token) = self.begin_mutation(page) else {
            return;
        };
        if page == Page::Home {
            self.home_profile_switching = Some(id.clone());
        }
        let controlled = self.controlled_config_store.clone();
        let core_runtime = workflow::CoreProfileRuntime::new(self.core_session.clone());
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
                this.mutating = false;
                this.home_profile_switching = None;
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
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn choose_profile(&mut self, cx: &mut Context<Self>) {
        let token = self.page_task_token_for(Page::Profiles);
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
                            this.import_local_profile(path, cx);
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
        self.profile_path = Some(outcome.path.clone());
        self.invalidate_config_inputs();
        self.config_preview = None;
        if let Err(error) = self.reload_profile_catalog() {
            self.set_page_error(token, error);
        }
        cx.emit(ProfileActivated { path: outcome.path });
        match outcome.refresh {
            Ok(data) => {
                if self.replace_page_data(token, data) {
                    self.notice = Some(notice(&outcome.name));
                }
            }
            Err(error) => {
                self.set_page_error(
                    token,
                    zenclash_i18n::text_with(
                        "profiles.errors.enabled_refresh",
                        &[("error", error)],
                    ),
                );
            }
        }
    }
}
