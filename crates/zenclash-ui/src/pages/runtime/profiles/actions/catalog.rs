use super::{workflow, Context, Page, ProfileActivated, RuntimePage};

impl RuntimePage {
    pub(in super::super) fn update_managed_profile(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(store) = self.profile_store.clone() else {
            return;
        };
        let Some(token) = self.begin_mutation(Page::Profiles) else {
            return;
        };
        let client = self.client.clone();
        let task = self
            .runtime
            .spawn(workflow::update_remote(store, client, id));
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
