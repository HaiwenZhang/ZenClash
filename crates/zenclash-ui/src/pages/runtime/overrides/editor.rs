use gpui::{AppContext, Context, Entity, Window};
use gpui_component::input::InputState;

use super::super::{
    h_flex, px, setting_card, v_flex, Button, ButtonVariants, Disableable, IconName, Input,
    ParentElement, RuntimePage, Styled,
};

pub(crate) struct ProfileEditorState {
    pub(super) input: Entity<InputState>,
    pub(super) original: Option<String>,
    pub(super) profile_id: Option<String>,
}

impl ProfileEditorState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<'_, RuntimePage>) -> Self {
        Self {
            input: cx.new(|cx| {
                InputState::new(window, cx)
                    .code_editor("yaml")
                    .rows(24)
                    .placeholder("先生成配置预览，再打开原始 YAML 编辑器")
            }),
            original: None,
            profile_id: None,
        }
    }
}

impl RuntimePage {
    pub(super) fn open_profile_yaml_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preview) = &self.config_preview else {
            self.error = Some("请先生成原始配置预览".into());
            cx.notify();
            return;
        };
        let Some(profile_id) = self.profile_catalog.active.clone() else {
            self.error = Some("当前配置不属于 ZenClash 托管仓库，无法安全编辑".into());
            cx.notify();
            return;
        };
        let original = preview.source.clone();
        self.profile_editor.input.update(cx, |input, cx| {
            input.set_value(original.clone(), window, cx);
        });
        self.profile_editor.original = Some(original);
        self.profile_editor.profile_id = Some(profile_id);
        self.error = None;
        cx.notify();
    }

    pub(super) fn cancel_profile_yaml_editor(&mut self, cx: &mut Context<Self>) {
        self.profile_editor.original = None;
        self.profile_editor.profile_id = None;
        cx.notify();
    }

    pub(super) fn save_profile_yaml_editor(&mut self, cx: &mut Context<Self>) {
        let (Some(store), Some(id), Some(original)) = (
            self.profile_store.clone(),
            self.profile_editor.profile_id.clone(),
            self.profile_editor.original.clone(),
        ) else {
            self.error = Some("Profile 编辑事务已经失效，请重新打开".into());
            cx.notify();
            return;
        };
        let candidate = self.profile_editor.input.read(cx).value().to_string();
        let Some(token) = self.begin_mutation(super::super::Page::Override) else {
            return;
        };
        let client = self.client.clone();
        let controlled = self.controlled_config_store.clone();
        let core_runtime = super::super::profiles::workflow::CoreProfileRuntime::new(
            self.core_kind,
            client,
            self.process.clone(),
        );
        let core_name = self.core_kind.display_name();
        let task = self.runtime.spawn(async move {
            let edit_store = store.clone();
            let edit_id = id.clone();
            let update = tokio::task::spawn_blocking(move || {
                edit_store.replace_payload(&edit_id, &original, &candidate)
            })
            .await
            .map_err(|error| format!("保存 Profile 编辑任务异常结束：{error}"))?
            .map_err(|error| error.to_string())?;
            let path = store.profile_path(&update.record);
            if let Err(error) =
                super::super::profiles::workflow::reload_effective(controlled, &core_runtime, &path)
                    .await
            {
                let rollback_store = store.clone();
                return match tokio::task::spawn_blocking(move || {
                    rollback_store.rollback_update(update)
                })
                .await
                {
                    Ok(Ok(_)) => Err(format!(
                        "{core_name} 拒绝编辑后的 YAML，已恢复原文件：{error}"
                    )),
                    Ok(Err(rollback)) => Err(format!(
                        "{core_name} 拒绝编辑后的 YAML：{error}；恢复原文件失败：{rollback}"
                    )),
                    Err(rollback) => Err(format!(
                        "{core_name} 拒绝编辑后的 YAML：{error}；回滚任务异常结束：{rollback}"
                    )),
                };
            }
            Ok::<_, String>(path)
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("Profile 编辑工作流异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(path) if this.is_page_task_current(token) => {
                        this.profile_path = Some(path.clone());
                        this.profile_editor.original = None;
                        this.profile_editor.profile_id = None;
                        this.config_preview = None;
                        this.invalidate_config_inputs();
                        if let Err(error) = this.reload_profile_catalog() {
                            this.set_page_error(token, error);
                        } else {
                            this.notice = Some(format!(
                                "原始 Profile YAML 已保存并由 {} 验收",
                                this.core_kind.display_name()
                            ));
                            cx.emit(super::super::ProfileActivated { path });
                        }
                    }
                    Ok(_) => {}
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn render_profile_yaml_editor(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        setting_card("原始 Profile YAML 编辑器", theme)
            .child(
                v_flex()
                    .h(px(520.))
                    .p_3()
                    .child(Input::new(&self.profile_editor.input).h_full()),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .p_3()
                    .child(
                        Button::new("cancel-profile-yaml-edit")
                            .label("取消")
                            .ghost()
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cancel_profile_yaml_editor(cx);
                            })),
                    )
                    .child(
                        Button::new("save-profile-yaml-edit")
                            .icon(IconName::Check)
                            .label("校验、保存并热重载")
                            .primary()
                            .loading(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.save_profile_yaml_editor(cx);
                            })),
                    ),
            )
    }
}
