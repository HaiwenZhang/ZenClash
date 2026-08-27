use std::path::{Path, PathBuf};

use super::super::{
    div, h_flex, info_row, px, setting_card, v_flex, Button, ButtonVariants, ClipboardItem,
    Context, Disableable, FluentBuilder, IconName, IntoElement, Page, ParentElement,
    PathPromptOptions, RuntimePage, ScrollableElement, Selectable, Sizable, Styled,
};
use zenclash_core::{RulesetBehavior, RulesetConversion, RulesetConverter};

const MAX_PREVIEW_BYTES: usize = 16 * 1024;

#[derive(Default)]
pub(crate) struct RulesetUiState {
    source: Option<PathBuf>,
    behavior: RulesetBehavior,
    conversion: Option<RulesetConversion>,
}

impl RuntimePage {
    pub(super) fn render_ruleset_converter(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let source = self.ruleset.source.as_deref().map_or_else(
            || zenclash_i18n::text("resources.ruleset.none"),
            compact_path,
        );
        let conversion = self.ruleset.conversion.as_ref();
        let output_size = conversion.map_or_else(
            || "—".into(),
            |result| super::super::format_bytes(result.output_bytes),
        );
        let source_size = conversion.map_or_else(
            || "—".into(),
            |result| super::super::format_bytes(result.source_bytes),
        );

        setting_card(zenclash_i18n::text("resources.ruleset.title"), theme)
            .when(!self.core_kind.capabilities().ruleset_conversion, |card| {
                card.child(super::super::message_banner(
                    zenclash_i18n::text_with(
                        "resources.ruleset.unsupported",
                        &[("core", self.core_kind.display_name().to_owned())],
                    ),
                    theme.warning,
                    theme,
                ))
            })
            .child(self.render_ruleset_controls(source, theme, cx))
            .child(info_row(
                zenclash_i18n::text("resources.ruleset.source"),
                &source_size,
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("resources.ruleset.output"),
                &output_size,
                theme,
            ))
            .when_some(conversion, |card, result| {
                card.child(self.render_ruleset_result(result, theme, cx))
            })
            .into_any_element()
    }

    fn render_ruleset_controls(
        &self,
        source: String,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        v_flex()
            .gap_3()
            .p_4()
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(zenclash_i18n::text("resources.ruleset.description")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(behavior_button(
                        "ruleset-domain",
                        "DOMAIN",
                        RulesetBehavior::Domain,
                        self.ruleset.behavior,
                        self.mutating,
                        cx,
                    ))
                    .child(behavior_button(
                        "ruleset-ipcidr",
                        "IPCIDR",
                        RulesetBehavior::IpCidr,
                        self.ruleset.behavior,
                        self.mutating,
                        cx,
                    ))
                    .child(behavior_button(
                        "ruleset-classical",
                        "CLASSICAL",
                        RulesetBehavior::Classical,
                        self.ruleset.behavior,
                        self.mutating,
                        cx,
                    )),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .px_3()
                            .py_2()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .overflow_hidden()
                            .child(source),
                    )
                    .child(
                        Button::new("choose-ruleset")
                            .icon(IconName::FolderOpen)
                            .label(zenclash_i18n::text("resources.ruleset.choose"))
                            .small()
                            .outline()
                            .disabled(
                                self.mutating || !self.core_kind.capabilities().ruleset_conversion,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.choose_ruleset(cx);
                            })),
                    )
                    .child(
                        Button::new("convert-ruleset")
                            .icon(IconName::ArrowRight)
                            .label(zenclash_i18n::text("resources.ruleset.convert"))
                            .small()
                            .primary()
                            .loading(self.mutating)
                            .disabled(
                                self.mutating
                                    || self.ruleset.source.is_none()
                                    || !self.core_kind.capabilities().ruleset_conversion,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.convert_ruleset(cx);
                            })),
                    ),
            )
    }

    fn render_ruleset_result(
        &self,
        result: &RulesetConversion,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let (preview, truncated) = ruleset_preview(&result.content);
        v_flex()
            .gap_2()
            .p_4()
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(if truncated {
                                zenclash_i18n::text("resources.ruleset.preview_truncated")
                            } else {
                                zenclash_i18n::text("resources.ruleset.preview")
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("copy-ruleset")
                                    .icon(IconName::Copy)
                                    .label(zenclash_i18n::text("resources.ruleset.copy"))
                                    .small()
                                    .ghost()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.copy_ruleset(cx);
                                    })),
                            )
                            .child(
                                Button::new("export-ruleset")
                                    .icon(IconName::File)
                                    .label(zenclash_i18n::text("resources.ruleset.export"))
                                    .small()
                                    .outline()
                                    .disabled(self.mutating)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.choose_ruleset_export(cx);
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .max_h(px(280.))
                    .overflow_y_scrollbar()
                    .p_3()
                    .rounded(theme.radius)
                    .bg(theme.secondary)
                    .border_1()
                    .border_color(theme.border)
                    .text_xs()
                    .child(preview.to_owned()),
            )
    }

    fn choose_ruleset(&mut self, cx: &mut Context<Self>) {
        let token = self.page_task_token_for(Page::Resources);
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(zenclash_i18n::text("resources.ruleset.choose_prompt").into()),
        });
        cx.spawn(async move |this, cx| {
            let selection = receiver.await;
            let _ = this.update(cx, |this, cx| match selection {
                Ok(Ok(Some(paths))) if this.is_page_task_current(token) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.ruleset.source = Some(path);
                        this.ruleset.conversion = None;
                        this.error = None;
                        this.notice = Some(zenclash_i18n::text("resources.ruleset.selected"));
                        cx.notify();
                    }
                }
                Ok(Ok(Some(_))) => {
                    tracing::info!("discarded ruleset selection after leaving resources page");
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    this.set_page_error(
                        token,
                        zenclash_i18n::text_with(
                            "resources.ruleset.errors.picker",
                            &[("error", error.to_string())],
                        ),
                    );
                    cx.notify();
                }
                Err(error) => {
                    this.set_page_error(
                        token,
                        zenclash_i18n::text_with(
                            "resources.ruleset.errors.picker_task",
                            &[("error", error.to_string())],
                        ),
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn convert_ruleset(&mut self, cx: &mut Context<Self>) {
        if !self.core_kind.capabilities().ruleset_conversion {
            self.error = Some(zenclash_i18n::text_with(
                "resources.ruleset.errors.unsupported",
                &[("core", self.core_kind.display_name().to_owned())],
            ));
            cx.notify();
            return;
        }
        let Some(source) = self.ruleset.source.clone() else {
            return;
        };
        let Some(binary) = self.mihomo_binary() else {
            self.error = Some(zenclash_i18n::text(
                "resources.ruleset.errors.binary_missing",
            ));
            cx.notify();
            return;
        };
        let behavior = self.ruleset.behavior;
        let Some(token) = self.begin_mutation(Page::Resources) else {
            return;
        };
        let task = self.runtime.spawn_blocking(move || {
            RulesetConverter::new(binary)
                .convert_mrs_to_text(source, behavior)
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "resources.ruleset.errors.conversion_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(conversion) if this.is_page_task_current(token) => {
                        this.notice = Some(zenclash_i18n::text_with(
                            "resources.ruleset.converted",
                            &[("behavior", conversion.behavior.as_str().to_owned())],
                        ));
                        this.ruleset.conversion = Some(conversion);
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

    fn copy_ruleset(&self, cx: &mut Context<Self>) {
        if let Some(result) = &self.ruleset.conversion {
            cx.write_to_clipboard(ClipboardItem::new_string(result.content.clone()));
        }
    }

    fn choose_ruleset_export(&mut self, cx: &mut Context<Self>) {
        let Some(result) = &self.ruleset.conversion else {
            return;
        };
        let token = self.page_task_token_for(Page::Resources);
        let directory = self
            .ruleset
            .source
            .as_deref()
            .and_then(Path::parent)
            .map_or_else(std::env::temp_dir, Path::to_path_buf);
        let receiver = cx.prompt_for_new_path(&directory, Some("zenclash-ruleset.txt"));
        let payload = result.content.clone();
        cx.spawn(async move |this, cx| {
            let selection = receiver.await;
            let _ = this.update(cx, |this, cx| match selection {
                Ok(Ok(Some(path))) if this.is_page_task_current(token) => {
                    this.write_ruleset_export(path, payload, token, cx);
                }
                Ok(Ok(Some(_))) => {
                    tracing::info!("discarded ruleset export after leaving resources page");
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    this.set_page_error(
                        token,
                        zenclash_i18n::text_with(
                            "resources.ruleset.errors.save_dialog",
                            &[("error", error.to_string())],
                        ),
                    );
                    cx.notify();
                }
                Err(error) => {
                    this.set_page_error(
                        token,
                        zenclash_i18n::text_with(
                            "resources.ruleset.errors.save_dialog_task",
                            &[("error", error.to_string())],
                        ),
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn write_ruleset_export(
        &mut self,
        path: PathBuf,
        payload: String,
        token: super::super::PageTaskToken,
        cx: &mut Context<Self>,
    ) {
        let Some(_) = self.begin_mutation(Page::Resources) else {
            return;
        };
        let display_path = path.display().to_string();
        let task = self.runtime.spawn(tokio::fs::write(path, payload));
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "resources.ruleset.errors.export_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| {
                    result.map_err(|error| {
                        zenclash_i18n::text_with(
                            "resources.ruleset.errors.write",
                            &[("error", error.to_string())],
                        )
                    })
                });
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(()) if this.is_page_task_current(token) => {
                        this.notice = Some(zenclash_i18n::text_with(
                            "resources.ruleset.exported",
                            &[("path", display_path)],
                        ));
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
}

fn behavior_button(
    id: &'static str,
    label: &'static str,
    behavior: RulesetBehavior,
    selected: RulesetBehavior,
    disabled: bool,
    cx: &mut Context<RuntimePage>,
) -> Button {
    Button::new(id)
        .label(label)
        .small()
        .outline()
        .selected(behavior == selected)
        .disabled(disabled)
        .on_click(cx.listener(move |this, _, _, cx| {
            this.ruleset.behavior = behavior;
            this.ruleset.conversion = None;
            this.error = None;
            cx.notify();
        }))
}

fn compact_path(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn ruleset_preview(content: &str) -> (&str, bool) {
    if content.len() <= MAX_PREVIEW_BYTES {
        return (content, false);
    }
    let mut end = MAX_PREVIEW_BYTES;
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    (&content[..end], true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_preserves_utf8_boundaries() {
        let content = "界".repeat(MAX_PREVIEW_BYTES);
        let (preview, truncated) = ruleset_preview(&content);

        assert!(truncated);
        assert!(preview.is_char_boundary(preview.len()));
        assert!(preview.len() <= MAX_PREVIEW_BYTES);
    }

    #[test]
    fn preview_keeps_short_content_intact() {
        let content = "+.example.com\n";

        assert_eq!(ruleset_preview(content), (content, false));
    }
}
