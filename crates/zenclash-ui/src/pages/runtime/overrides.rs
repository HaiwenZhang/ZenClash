use super::{
    h_flex, info_row, load_page, merge_profile_overrides, message_banner, setting_card, v_flex,
    Button, ButtonVariants, Context, Disableable, IconName, IntoElement, Page, ParentElement,
    PathPromptOptions, RuntimePage, Styled,
};

impl RuntimePage {
    fn choose_overrides(&mut self, cx: &mut Context<Self>) {
        let token = self.page_task_token_for(Page::Override);
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("按应用顺序选择 YAML 覆写".into()),
        });
        cx.spawn(async move |this, cx| {
            let selection = receiver.await;
            let _ = this.update(cx, |this, cx| match selection {
                Ok(Ok(Some(paths))) => {
                    this.override_paths = paths;
                    if this.is_page_task_current(token) {
                        this.notice = Some(format!(
                            "已选择 {} 份覆写；点击“合并并热重载”应用",
                            this.override_paths.len()
                        ));
                        this.error = None;
                    }
                    cx.notify();
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    this.set_page_error(token, format!("无法打开覆写选择器：{error}"));
                    cx.notify();
                }
                Err(error) => {
                    this.set_page_error(token, format!("覆写选择器异常结束：{error}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn apply_overrides(&mut self, cx: &mut Context<Self>) {
        let Some(profile) = self.profile_path.clone() else {
            self.error = Some("未配置基础配置文件路径".into());
            cx.notify();
            return;
        };
        if self.override_paths.is_empty() {
            return;
        }
        let Some(token) = self.begin_mutation(Page::Override) else {
            return;
        };
        let overrides = self.override_paths.clone();
        let count = overrides.len();
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            let payload =
                tokio::task::spawn_blocking(move || merge_profile_overrides(profile, &overrides))
                    .await
                    .map_err(|error| format!("覆写合并任务异常结束：{error}"))?
                    .map_err(|error| error.to_string())?;
            client
                .reload_payload(payload, true)
                .await
                .map_err(|error| error.to_string())?;
            load_page(client, Page::Override).await
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("覆写热重载任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
                            this.notice = Some(format!("{count} 份 YAML 覆写已合并并热重载"));
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

    pub(super) fn render_override(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let path = self
            .profile_path
            .as_ref()
            .map_or_else(|| "未指定".into(), |path| path.display().to_string());
        let count = self.override_paths.len();
        v_flex()
            .gap_4()
            .child(
                setting_card("配置覆写链", theme)
                    .child(info_row("基础配置", &path, theme))
                    .child(info_row("YAML 覆写", &format!("{count} 份"), theme))
                    .children(self.override_paths.iter().enumerate().map(|(index, path)| {
                        info_row(
                            "应用顺序",
                            &format!("{}. {}", index + 1, path.display()),
                            theme,
                        )
                    }))
                    .child(
                        h_flex()
                            .justify_end()
                            .gap_2()
                            .p_3()
                            .child(
                                Button::new("choose-overrides")
                                    .icon(IconName::FolderOpen)
                                    .label("选择 YAML 覆写")
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.choose_overrides(cx)),
                                    ),
                            )
                            .child(
                                Button::new("clear-overrides")
                                    .label("清空")
                                    .disabled(count == 0 || self.mutating)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.override_paths.clear();
                                        this.notice =
                                            Some("覆写选择已清空；运行中配置未改变".into());
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("apply-overrides")
                                    .icon(IconName::Redo2)
                                    .label("合并并热重载")
                                    .primary()
                                    .loading(self.mutating)
                                    .disabled(count == 0)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.apply_overrides(cx)),
                                    ),
                            ),
                    ),
            )
            .child(message_banner(
                "基础文件保持不变；映射递归合并，后选覆写优先，数组与标量整体替换。".into(),
                theme.primary,
                theme,
            ))
            .into_any_element()
    }
}
