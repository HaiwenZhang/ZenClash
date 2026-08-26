use zenclash_core::ProfileSource;

use super::{workflow, Context, Page, ProfileActivated, RemoteProfileOptions, RuntimePage, Window};

impl RuntimePage {
    pub(in super::super) fn begin_edit_remote_profile(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = self
            .profile_catalog
            .profiles
            .iter()
            .find(|profile| profile.id == id)
        else {
            self.error = Some("找不到要编辑的在线订阅".into());
            cx.notify();
            return;
        };
        let ProfileSource::Remote {
            url,
            user_agent,
            options,
        } = &profile.source
        else {
            self.error = Some("本地配置没有订阅请求设置".into());
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
        self.profile_forms
            .request_name
            .update(cx, |input, cx| input.set_value(name, window, cx));
        self.profile_forms
            .request_url
            .update(cx, |input, cx| input.set_value(url, window, cx));
        self.profile_forms
            .request_user_agent
            .update(cx, |input, cx| input.set_value(user_agent, window, cx));
        self.profile_forms
            .request_authorization
            .update(cx, |input, cx| input.set_value(authorization, window, cx));
        self.profile_forms
            .request_timeout_seconds
            .update(cx, |input, cx| input.set_value(timeout_seconds, window, cx));
        self.profile_forms.update_cron.update(cx, |input, cx| {
            input.set_value(update_cron, window, cx);
        });
        self.profile_forms.editing_route = options.route();
        self.profile_forms.editing_fixed_update_interval = options.fixed_update_interval;
        self.profile_forms.editing_profile_id = Some(id);
        self.error = None;
        cx.notify();
    }

    pub(in super::super) fn cancel_edit_remote_profile(&mut self, cx: &mut Context<Self>) {
        self.profile_forms.editing_profile_id = None;
        cx.notify();
    }

    pub(in super::super) fn save_remote_profile_settings(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.profile_forms.editing_profile_id.clone() else {
            return;
        };
        let Some(store) = self.profile_store.clone() else {
            self.error = Some("配置仓库不可用".into());
            cx.notify();
            return;
        };
        let authorization = self
            .profile_forms
            .request_authorization
            .read(cx)
            .value()
            .to_string();
        let name = self.profile_forms.request_name.read(cx).value().to_string();
        let url = self.profile_forms.request_url.read(cx).value().to_string();
        let user_agent = self
            .profile_forms
            .request_user_agent
            .read(cx)
            .value()
            .to_string();
        let timeout_seconds = match self
            .profile_forms
            .request_timeout_seconds
            .read(cx)
            .value()
            .trim()
            .parse::<u32>()
        {
            Ok(value) => value,
            Err(error) => {
                self.error = Some(format!("订阅下载超时必须是整数秒数：{error}"));
                cx.notify();
                return;
            }
        };
        let options = RemoteProfileOptions::new(authorization, false)
            .map(|options| options.with_route(self.profile_forms.editing_route))
            .and_then(|options| {
                options.with_download_policy(
                    timeout_seconds,
                    self.profile_forms.editing_fixed_update_interval,
                )
            });
        let options = match options {
            Ok(options) => options,
            Err(error) => {
                self.error = Some(format!("订阅请求设置无效：{error}"));
                cx.notify();
                return;
            }
        };
        let cron = self.profile_forms.update_cron.read(cx).value().to_string();
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
                .map_err(|error| format!("订阅请求设置任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(()) if this.is_page_task_current(token) => {
                        if let Err(error) = this.reload_profile_catalog() {
                            this.set_page_error(token, error);
                        } else {
                            this.profile_forms.editing_profile_id = None;
                            this.notice = Some("订阅请求设置与更新计划已保存".into());
                        }
                    }
                    Ok(()) => {}
                    Err(error) => this.set_page_error(token, error),
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
        let Some(store) = self.profile_store.clone() else {
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
            .map_err(|error| format!("订阅更新策略任务异常结束：{error}"))?
            .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("订阅更新策略任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(()) => {
                        if let Err(error) = this.reload_profile_catalog() {
                            this.set_page_error(token, error);
                        } else if this.is_page_task_current(token) {
                            this.notice = Some(if enabled {
                                format!("已启用自动更新，每 {interval_minutes} 分钟检查一次")
                            } else {
                                "已关闭该订阅的自动更新".into()
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

    pub(in super::super) fn update_managed_profile(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(store) = self.profile_store.clone() else {
            return;
        };
        let Some(token) = self.begin_mutation(Page::Profiles) else {
            return;
        };
        let client = self.client.clone();
        let controlled = self.controlled_config_store.clone();
        let core_runtime =
            workflow::CoreProfileRuntime::new(self.core_kind, client, self.process.clone());
        let task = self
            .runtime
            .spawn(workflow::update_remote(store, controlled, core_runtime, id));
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("订阅更新任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(outcome) => {
                        let is_profile_page = match outcome.refresh {
                            Ok(data) => this.replace_page_data(token, data),
                            Err(error) => {
                                this.set_page_error(
                                    token,
                                    format!("订阅已更新，但刷新页面失败：{error}"),
                                );
                                false
                            }
                        };
                        if let Err(error) = this.reload_profile_catalog() {
                            this.set_page_error(token, error);
                        }
                        if is_profile_page {
                            this.notice = Some(format!("在线订阅“{}”已更新", outcome.name));
                        }
                        if outcome.active {
                            this.profile_path = Some(outcome.path.clone());
                            this.invalidate_config_inputs();
                            this.config_preview = None;
                            cx.emit(ProfileActivated { path: outcome.path });
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

    pub(in super::super) fn delete_managed_profile(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(store) = self.profile_store.clone() else {
            return;
        };
        let Some(token) = self.begin_mutation(Page::Profiles) else {
            return;
        };
        let task = self.runtime.spawn(workflow::delete(store, id));
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("配置删除任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(()) => {
                        if let Err(error) = this.reload_profile_catalog() {
                            this.set_page_error(token, error);
                        }
                        if this.is_page_task_current(token) {
                            this.notice = Some("配置已从 ZenClash 仓库删除".into());
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
}
