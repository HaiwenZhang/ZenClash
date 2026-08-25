use super::{
    super::{
        load_page, Context, Page, PageTaskToken, PathBuf, PathPromptOptions, ProfileActivated,
        RuntimePage,
    },
    workflow,
};

mod catalog;

impl RuntimePage {
    pub(super) fn reload_profile(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.profile_path.clone() else {
            self.error = Some("未配置当前配置文件路径".into());
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(Page::Profiles) else {
            return;
        };
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            client
                .reload_config(&path, true)
                .await
                .map_err(|error| error.to_string())?;
            load_page(client, Page::Profiles).await
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("重载配置任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
                            this.notice = Some("真实配置已由 Mihomo 热重载".into());
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
            self.error = Some("配置仓库不可用".into());
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(Page::Profiles) else {
            return;
        };
        let client = self.client.clone();
        let task = self
            .runtime
            .spawn(workflow::import_local(store, client, path));
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("本地配置导入任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(outcome) => this.apply_profile_activation(
                        outcome,
                        |name| format!("已导入并启用本地配置“{name}”"),
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
            self.error = Some("配置仓库不可用".into());
            cx.notify();
            return;
        };
        if self.mutating {
            return;
        }
        let name = self.subscription_name.read(cx).value().to_string();
        let url = self.subscription_url.read(cx).value().to_string();
        let user_agent = self.subscription_user_agent.read(cx).value().to_string();
        if name.trim().is_empty() || url.trim().is_empty() {
            self.error = Some("请填写订阅名称和订阅 URL".into());
            cx.notify();
            return;
        }
        let Some(token) = self.begin_mutation(Page::Profiles) else {
            return;
        };
        let client = self.client.clone();
        let task = self
            .runtime
            .spawn(workflow::add_remote(store, client, name, url, user_agent));
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("在线订阅任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(outcome) => this.apply_profile_activation(
                        outcome,
                        |name| format!("在线订阅“{name}”已下载并启用"),
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

    pub(super) fn activate_managed_profile(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(store) = self.profile_store.clone() else {
            return;
        };
        let Some(token) = self.begin_mutation(Page::Profiles) else {
            return;
        };
        let client = self.client.clone();
        let task = self
            .runtime
            .spawn(workflow::activate_existing(store, client, id));
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("配置切换任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(outcome) => this.apply_profile_activation(
                        outcome,
                        |name| format!("已切换到“{name}”"),
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
            prompt: Some("选择 Mihomo YAML 配置".into()),
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
                    this.set_page_error(token, format!("无法打开配置选择器：{error}"));
                    cx.notify();
                }
                Err(error) => {
                    this.set_page_error(token, format!("配置选择器异常结束：{error}"));
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
                self.set_page_error(token, format!("配置已启用，但刷新页面失败：{error}"));
            }
        }
    }
}
