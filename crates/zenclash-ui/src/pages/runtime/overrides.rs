use super::{
    Button, ButtonVariants, ClipboardItem, ConfigDiffReport, Context, Disableable, FluentBuilder,
    IconName, IntoElement, Page, ParentElement, PathPromptOptions, RuntimePage, ScrollableElement,
    Sizable, Styled, Switch, diff_yaml_configs, div, h_flex, info_row, load_page, message_banner,
    px, setting_card, v_flex,
};

mod diff_view;
mod editor;
mod store_actions;
use diff_view::render_config_diff;
pub(crate) use editor::ProfileEditorState;

const MAX_PREVIEW_BYTES: usize = 64 * 1024;
const MAX_CONFIG_DIFF_ENTRIES: usize = 200;

pub(super) struct ConfigPreview {
    source: String,
    effective: String,
    diff: ConfigDiffReport,
}

impl RuntimePage {
    fn choose_overrides(&mut self, cx: &mut Context<Self>) {
        let token = self.page_task_token_for(Page::Override);
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some(zenclash_i18n::text("overrides.dialog.import").into()),
        });
        cx.spawn(async move |this, cx| {
            let selection = receiver.await;
            let _ = this.update(cx, |this, cx| match selection {
                Ok(Ok(Some(paths))) => {
                    if this.is_page_task_current(token) {
                        this.import_override_paths(paths, cx);
                    }
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    this.set_page_error(
                        token,
                        zenclash_i18n::text_with(
                            "overrides.errors.picker",
                            &[("error", error.to_string())],
                        ),
                    );
                    cx.notify();
                }
                Err(error) => {
                    this.set_page_error(
                        token,
                        zenclash_i18n::text_with(
                            "overrides.errors.picker_task",
                            &[("error", error.to_string())],
                        ),
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn load_config_preview(&mut self, cx: &mut Context<Self>) {
        let Some(profile) = self.profile_path.clone() else {
            self.error = Some(zenclash_i18n::text("overrides.errors.base_missing"));
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(Page::Override) else {
            return;
        };
        let controlled = self.controlled_config_store.clone();
        let overrides = self.enabled_override_paths();
        let task = self.runtime.spawn(async move {
            tokio::task::spawn_blocking(move || {
                let source = controlled
                    .source_payload(&profile)
                    .map_err(|error| error.to_string())?;
                let effective = controlled
                    .effective_with_overrides(profile, &overrides)
                    .map_err(|error| error.to_string())?;
                let diff = diff_yaml_configs(&source, &effective, MAX_CONFIG_DIFF_ENTRIES)
                    .map_err(|error| error.to_string())?;
                Ok::<_, String>(ConfigPreview {
                    source,
                    effective,
                    diff,
                })
            })
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "overrides.errors.preview_task",
                    &[("error", error.to_string())],
                )
            })?
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "overrides.errors.preview_worker",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(preview) if this.is_page_task_current(token) => {
                        this.config_preview = Some(preview);
                        this.notice = Some(zenclash_i18n::text("overrides.notices.preview"));
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

    fn copy_config_preview(&self, effective: bool, cx: &mut Context<Self>) {
        let Some(preview) = &self.config_preview else {
            return;
        };
        let payload = if effective {
            &preview.effective
        } else {
            &preview.source
        };
        cx.write_to_clipboard(ClipboardItem::new_string(payload.clone()));
    }

    fn apply_overrides(&mut self, cx: &mut Context<Self>) {
        let Some(profile) = self.profile_path.clone() else {
            self.error = Some(zenclash_i18n::text("overrides.errors.base_missing"));
            cx.notify();
            return;
        };
        let overrides = self.enabled_override_paths();
        let Some(token) = self.begin_mutation(Page::Override) else {
            return;
        };
        let count = overrides.len();
        let client = self.client.clone();
        let controlled = self.controlled_config_store.clone();
        let core_runtime =
            super::profiles::workflow::CoreProfileRuntime::new(self.core_session.clone());
        let task = self.runtime.spawn(async move {
            super::profiles::workflow::reload_effective(controlled, &core_runtime, &profile)
                .await?;
            load_page(client, Page::Override).await
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(zenclash_i18n::text_with(
                    "overrides.errors.reload_task",
                    &[("error", error.to_string())],
                )),
            };
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
                            this.notice = Some(zenclash_i18n::text_with(
                                "overrides.notices.applied",
                                &[("count", count.to_string())],
                            ));
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
        v_flex()
            .gap_4()
            .child(self.render_override_chain(theme, cx))
            .child(message_banner(
                zenclash_i18n::text("overrides.chain.explanation"),
                theme.primary,
                theme,
            ))
            .children(self.config_preview.as_ref().map(|preview| {
                v_flex()
                    .gap_4()
                    .child(render_config_diff(&preview.diff, theme))
                    .child(preview_panel(
                        zenclash_i18n::text("overrides.preview.source"),
                        &preview.source,
                        "copy-source-config",
                        theme,
                        cx.listener(|this, _, _, cx| this.copy_config_preview(false, cx)),
                    ))
                    .child(preview_panel(
                        zenclash_i18n::text("overrides.preview.effective"),
                        &preview.effective,
                        "copy-effective-config",
                        theme,
                        cx.listener(|this, _, _, cx| this.copy_config_preview(true, cx)),
                    ))
            }))
            .when(self.profile_editor.original.is_some(), |this| {
                this.child(self.render_profile_yaml_editor(theme, cx))
            })
            .into_any_element()
    }

    fn render_override_chain(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let path = self.profile_path.as_ref().map_or_else(
            || zenclash_i18n::text("overrides.chain.unspecified"),
            |path| path.display().to_string(),
        );
        let count = self.override_catalog.items.len();
        let enabled = self
            .override_catalog
            .items
            .iter()
            .filter(|record| record.enabled)
            .count();
        setting_card(zenclash_i18n::text("overrides.chain.title"), theme)
            .child(info_row(
                zenclash_i18n::text("overrides.chain.base"),
                &path,
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("overrides.chain.yaml"),
                zenclash_i18n::text_with(
                    "overrides.chain.count",
                    &[
                        ("enabled", enabled.to_string()),
                        ("total", count.to_string()),
                    ],
                ),
                theme,
            ))
            .children(
                self.override_catalog
                    .items
                    .iter()
                    .enumerate()
                    .map(|(index, record)| self.render_override_record(index, record, theme, cx)),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .p_3()
                    .child(
                        Button::new("preview-overrides")
                            .icon(IconName::Eye)
                            .label(zenclash_i18n::text("overrides.actions.preview"))
                            .loading(self.mutating)
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.load_config_preview(cx);
                            })),
                    )
                    .child(
                        Button::new("edit-source-profile")
                            .icon(IconName::File)
                            .label(zenclash_i18n::text("overrides.actions.edit"))
                            .outline()
                            .disabled(
                                self.mutating
                                    || self.config_preview.is_none()
                                    || self.profile_catalog.active.is_none(),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_profile_yaml_editor(window, cx);
                            })),
                    )
                    .child(
                        Button::new("choose-overrides")
                            .icon(IconName::FolderOpen)
                            .label(zenclash_i18n::text("overrides.actions.import"))
                            .on_click(cx.listener(|this, _, _, cx| this.choose_overrides(cx))),
                    )
                    .child(
                        Button::new("apply-overrides")
                            .icon(crate::assets::AppIcon::RefreshCw)
                            .label(zenclash_i18n::text("overrides.actions.apply"))
                            .primary()
                            .loading(self.mutating)
                            .disabled(self.profile_path.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.apply_overrides(cx))),
                    ),
            )
    }

    fn render_override_record(
        &self,
        index: usize,
        record: &zenclash_core::YamlOverrideRecord,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let enabled_id = record.id.clone();
        let up_id = record.id.clone();
        let down_id = record.id.clone();
        let delete_id = record.id.clone();
        h_flex()
            .min_h(px(44.))
            .px_4()
            .gap_3()
            .border_t_1()
            .border_color(theme.border)
            .child(
                Switch::new(("override-enabled", index))
                    .checked(record.enabled)
                    .disabled(self.mutating)
                    .on_click(cx.listener(move |this, checked, _, cx| {
                        this.set_override_enabled(&enabled_id, *checked, cx);
                    })),
            )
            .child(
                div()
                    .w(px(24.))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{}.", index + 1)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .child(record.name.clone()),
            )
            .child(
                Button::new(("override-up", index))
                    .icon(IconName::ArrowUp)
                    .xsmall()
                    .ghost()
                    .disabled(index == 0 || self.mutating)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.move_override(&up_id, -1, cx);
                    })),
            )
            .child(
                Button::new(("override-down", index))
                    .icon(IconName::ArrowDown)
                    .xsmall()
                    .ghost()
                    .disabled(index + 1 == self.override_catalog.items.len() || self.mutating)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.move_override(&down_id, 1, cx);
                    })),
            )
            .child(
                Button::new(("override-delete", index))
                    .icon(IconName::Delete)
                    .xsmall()
                    .ghost()
                    .danger()
                    .disabled(record.enabled || self.mutating)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.delete_disabled_override(delete_id.clone(), cx);
                    })),
            )
    }
}

fn preview_panel<F>(
    title: String,
    payload: &str,
    button_id: &'static str,
    theme: &gpui_component::Theme,
    copy: F,
) -> gpui::Div
where
    F: Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
{
    let (content, truncated) = preview_text(payload);
    setting_card(title, theme)
        .child(
            h_flex()
                .justify_between()
                .px_4()
                .py_3()
                .child(format!("{} bytes", payload.len()))
                .child(
                    Button::new(button_id)
                        .icon(IconName::Copy)
                        .label(zenclash_i18n::text("overrides.actions.copy"))
                        .on_click(copy),
                ),
        )
        .child(
            div()
                .max_h(px(360.))
                .overflow_y_scrollbar()
                .p_4()
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(content),
        )
        .when(truncated, |panel| {
            panel.child(message_banner(
                zenclash_i18n::text("overrides.preview.truncated"),
                theme.warning,
                theme,
            ))
        })
}

fn preview_text(payload: &str) -> (String, bool) {
    if payload.len() <= MAX_PREVIEW_BYTES {
        return (payload.to_owned(), false);
    }
    let mut end = MAX_PREVIEW_BYTES;
    while !payload.is_char_boundary(end) {
        end -= 1;
    }
    (payload[..end].to_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_truncation_preserves_utf8_boundaries() {
        let payload = "配".repeat(MAX_PREVIEW_BYTES);
        let (preview, truncated) = preview_text(&payload);

        assert!(truncated);
        assert!(preview.len() <= MAX_PREVIEW_BYTES);
        assert!(preview.is_char_boundary(preview.len()));
    }
}
