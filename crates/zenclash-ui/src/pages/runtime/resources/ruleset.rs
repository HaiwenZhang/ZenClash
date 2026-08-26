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
        let source = self
            .ruleset
            .source
            .as_deref()
            .map_or_else(|| "尚未选择 MRS 文件".into(), compact_path);
        let conversion = self.ruleset.conversion.as_ref();
        let output_size = conversion.map_or_else(
            || "—".into(),
            |result| super::super::format_bytes(result.output_bytes),
        );
        let source_size = conversion.map_or_else(
            || "—".into(),
            |result| super::super::format_bytes(result.source_bytes),
        );

        setting_card("MRS 规则集检查器", theme)
            .when(!self.core_kind.capabilities().ruleset_conversion, |card| {
                card.child(super::super::message_banner(
                    format!(
                        "{} 不实现 Mihomo 的 MRS 转换命令；请选择 Mihomo 内核后使用此工具。",
                        self.core_kind.display_name()
                    ),
                    theme.warning,
                    theme,
                ))
            })
            .child(self.render_ruleset_controls(source, theme, cx))
            .child(info_row("源文件", &source_size, theme))
            .child(info_row("UTF-8 输出", &output_size, theme))
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
                    .child("用当前 Mihomo 内核解码二进制规则集；不经过 shell，也不会修改源文件。"),
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
                            .label("选择 MRS")
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
                            .label("转换")
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
                                "规则预览 · 已截断显示，复制与导出仍包含完整内容"
                            } else {
                                "规则预览"
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("copy-ruleset")
                                    .icon(IconName::Copy)
                                    .label("复制完整规则")
                                    .small()
                                    .ghost()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.copy_ruleset(cx);
                                    })),
                            )
                            .child(
                                Button::new("export-ruleset")
                                    .icon(IconName::File)
                                    .label("导出 TXT")
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
            prompt: Some("选择 Mihomo MRS 规则集".into()),
        });
        cx.spawn(async move |this, cx| {
            let selection = receiver.await;
            let _ = this.update(cx, |this, cx| match selection {
                Ok(Ok(Some(paths))) if this.is_page_task_current(token) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.ruleset.source = Some(path);
                        this.ruleset.conversion = None;
                        this.error = None;
                        this.notice = Some("已选择 MRS 文件；请选择匹配的行为类型后转换".into());
                        cx.notify();
                    }
                }
                Ok(Ok(Some(_))) => {
                    tracing::info!("discarded ruleset selection after leaving resources page");
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    this.set_page_error(token, format!("无法打开 MRS 选择器：{error}"));
                    cx.notify();
                }
                Err(error) => {
                    this.set_page_error(token, format!("MRS 选择器异常结束：{error}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn convert_ruleset(&mut self, cx: &mut Context<Self>) {
        if !self.core_kind.capabilities().ruleset_conversion {
            self.error = Some(format!(
                "{} 不支持 MRS 规则集转换",
                self.core_kind.display_name()
            ));
            cx.notify();
            return;
        }
        let Some(source) = self.ruleset.source.clone() else {
            return;
        };
        let Some(binary) = self.mihomo_binary() else {
            self.error = Some(
                "未找到 Mihomo 可执行文件；请先由 ZenClash 启动内核，或设置 ZENCLASH_MIHOMO_BINARY"
                    .into(),
            );
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
                .map_err(|error| format!("MRS 转换任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(conversion) if this.is_page_task_current(token) => {
                        this.notice = Some(format!(
                            "已用 {} 行为解码 MRS 规则集",
                            conversion.behavior.as_str()
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
                    this.set_page_error(token, format!("无法打开规则保存对话框：{error}"));
                    cx.notify();
                }
                Err(error) => {
                    this.set_page_error(token, format!("规则保存对话框异常结束：{error}"));
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
                .map_err(|error| format!("规则导出任务异常结束：{error}"))
                .and_then(|result| result.map_err(|error| format!("写入规则文件失败：{error}")));
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(()) if this.is_page_task_current(token) => {
                        this.notice = Some(format!("规则已导出到 {display_path}"));
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
