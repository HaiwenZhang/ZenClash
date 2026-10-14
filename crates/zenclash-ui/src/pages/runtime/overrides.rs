use super::{
    Button, ButtonVariants, ClipboardItem, Context, Disableable, FluentBuilder, IconName,
    IntoElement, Page, ParentElement, PathPromptOptions, RuntimePage, ScrollableElement, Sizable,
    Styled, Switch, context_note, div, h_flex, info_row, load_page, setting_card, v_flex,
};

mod model;
pub(super) use model::OverridesState;
mod diff_view;
mod store_actions;
use diff_view::render_config_diff;

const MAX_PREVIEW_BYTES: usize = 64 * 1024;

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
        let token = self.page_task_token_for(Page::Override);
        let Some(read) = self.overrides.start_preview(
            &self.runtime,
            profile,
            self.controlled_config_store.clone(),
        ) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let completion = read.finish().await;
            let _ = this.update(cx, |this, cx| {
                if !this.is_page_task_current(token) {
                    return;
                }
                let Some(result) = this.overrides.complete_preview(completion) else {
                    return;
                };
                match result {
                    Ok(()) if this.is_page_task_current(token) => {
                        this.notice = Some(zenclash_i18n::text("overrides.notices.preview"));
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

    fn copy_config_preview(&self, effective: bool, cx: &mut Context<Self>) {
        let Some(preview) = self.overrides.preview() else {
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
        if self.profile_path.is_none() {
            self.error = Some(zenclash_i18n::text("overrides.errors.base_missing"));
            cx.notify();
            return;
        }
        let Some(store) = self.overrides.store().cloned() else {
            self.error = Some(zenclash_i18n::text("overrides.errors.store_unavailable"));
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(Page::Override) else {
            return;
        };
        let application = self
            .application_commands
            .override_application(store, self.controlled_config_store.clone());
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            let receipt = application.reapply().await?;
            let presentation = load_page(client, Page::Override).await;
            Ok::<_, crate::application_commands::OverrideApplicationFailure>((
                receipt,
                presentation,
            ))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok((receipt, presentation))) => {
                        let runtime_version =
                            receipt.runtime_outcome().map(|outcome| outcome.generation);
                        let enabled_count = receipt.enabled_count();
                        this.accept_override_receipt(receipt, cx);
                        match presentation {
                            Ok(data)
                                if runtime_version.is_some_and(|version| {
                                    this.application_commands.is_current(version)
                                }) =>
                            {
                                if this.replace_page_data(token, data, cx) {
                                    this.notice = Some(zenclash_i18n::text_with(
                                        "overrides.notices.applied",
                                        &[("count", enabled_count.to_string())],
                                    ));
                                }
                            }
                            Ok(_) => {}
                            Err(error) => this.set_page_error(token, error),
                        }
                    }
                    Ok(Err(failure)) => this.accept_override_failure(failure, token, cx),
                    Err(error) => this.set_page_error(token, error.to_string()),
                }
                this.finish_mutation(token);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn render_override(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        v_flex()
            .gap_4()
            .child(self.render_override_chain(theme, cx))
            .child(context_note(
                zenclash_i18n::text("overrides.chain.explanation"),
                theme,
            ))
            .children(self.overrides.preview().map(|preview| {
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
            .into_any_element()
    }

    fn render_override_chain(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let path = self.profile_path.as_ref().map_or_else(
            || zenclash_i18n::text("overrides.chain.unspecified"),
            |path| path.display().to_string(),
        );
        let count = self.overrides.catalog().items.len();
        let enabled = self
            .overrides
            .catalog()
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
                self.overrides
                    .catalog()
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
                            .loading(self.overrides.is_preview_loading())
                            .disabled(self.core_busy() || self.overrides.is_preview_loading())
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
                                self.core_busy()
                                    || self.overrides.preview().is_none()
                                    || self.profiles.catalog().active.is_none(),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                let Some(preview) = this.overrides.preview() else {
                                    this.error = Some(zenclash_i18n::text(
                                        "overrides.errors.preview_required",
                                    ));
                                    cx.notify();
                                    return;
                                };
                                let Some(profile_id) = this.profiles.catalog().active.clone()
                                else {
                                    this.error = Some(zenclash_i18n::text(
                                        "overrides.errors.unmanaged_profile",
                                    ));
                                    cx.notify();
                                    return;
                                };
                                this.open_profile_yaml_editor(
                                    profile_id,
                                    preview.source.clone(),
                                    window,
                                    cx,
                                );
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
                            .loading(self.core_busy())
                            .disabled(self.profile_path.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.apply_overrides(cx))),
                    ),
            )
    }

    fn render_override_record(
        &self,
        index: usize,
        record: &zenclash_core::YamlOverrideRecord,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let enabled_id = record.id.clone();
        let up_id = record.id.clone();
        let down_id = record.id.clone();
        let delete_id = record.id.clone();
        h_flex()
            .min_h(gpui_kit::rems(2.75))
            .px_4()
            .gap_3()
            .border_t_1()
            .border_color(theme.border)
            .child(
                Switch::new(format!("override-enabled:{}", record.id))
                    .checked(record.enabled)
                    .disabled(self.core_busy())
                    .on_click(cx.listener(move |this, checked, _, cx| {
                        this.set_override_enabled(&enabled_id, *checked, cx);
                    })),
            )
            .child(
                div()
                    .w(gpui_kit::rems(1.5))
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
                Button::new(format!("override-up:{}", record.id))
                    .icon(IconName::ArrowUp)
                    .xsmall()
                    .ghost()
                    .disabled(index == 0 || self.core_busy())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.move_override(&up_id, -1, cx);
                    })),
            )
            .child(
                Button::new(format!("override-down:{}", record.id))
                    .icon(IconName::ArrowDown)
                    .xsmall()
                    .ghost()
                    .disabled(index + 1 == self.overrides.catalog().items.len() || self.core_busy())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.move_override(&down_id, 1, cx);
                    })),
            )
            .child(
                Button::new(format!("override-delete:{}", record.id))
                    .icon(IconName::Delete)
                    .xsmall()
                    .ghost()
                    .danger()
                    .disabled(record.enabled || self.core_busy())
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
    theme: &gpui_kit::component::Theme,
    copy: F,
) -> gpui_kit::Div
where
    F: Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
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
                .max_h(gpui_kit::rems(22.5))
                .overflow_y_scrollbar()
                .p_4()
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(content),
        )
        .when(truncated, |panel| {
            panel.child(context_note(
                zenclash_i18n::text("overrides.preview.truncated"),
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

#[cfg(test)]
mod ui_tests;
