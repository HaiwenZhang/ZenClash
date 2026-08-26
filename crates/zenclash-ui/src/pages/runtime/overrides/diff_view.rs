use gpui::{AnyElement, FontWeight};
use zenclash_core::{ConfigDiffEntry, ConfigDiffKind, ConfigDiffReport};

use super::super::{
    div, empty_state, h_flex, px, setting_card, v_flex, FluentBuilder, IntoElement, ParentElement,
    Styled,
};

pub(super) fn render_config_diff(
    report: &ConfigDiffReport,
    theme: &gpui_component::Theme,
) -> gpui::Div {
    let added = count_kind(report, ConfigDiffKind::Added);
    let removed = count_kind(report, ConfigDiffKind::Removed);
    let changed = count_kind(report, ConfigDiffKind::Changed);
    setting_card("结构化配置差异", theme)
        .child(
            h_flex()
                .justify_between()
                .px_4()
                .py_3()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("新增 {added} · 删除 {removed} · 修改 {changed}")),
                )
                .child(div().text_xs().text_color(theme.muted_foreground).child(
                    if report.truncated {
                        "仅显示前 200 项"
                    } else {
                        "JSON Pointer 路径"
                    },
                )),
        )
        .when(report.entries.is_empty(), |this| {
            this.child(empty_state("原始 Profile 与最终运行配置结构一致", theme))
        })
        .children(
            report
                .entries
                .iter()
                .map(|entry| render_diff_entry(entry, theme)),
        )
}

fn render_diff_entry(entry: &ConfigDiffEntry, theme: &gpui_component::Theme) -> AnyElement {
    let (label, color) = match entry.kind {
        ConfigDiffKind::Added => ("新增", theme.success),
        ConfigDiffKind::Removed => ("删除", theme.danger),
        ConfigDiffKind::Changed => ("修改", theme.warning),
    };
    v_flex()
        .gap_2()
        .px_4()
        .py_3()
        .border_t_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .px_2()
                        .py_0p5()
                        .rounded_full()
                        .bg(color.opacity(0.12))
                        .text_size(px(10.))
                        .text_color(color)
                        .child(label),
                )
                .child(
                    div()
                        .font_family(theme.mono_font_family.clone())
                        .text_xs()
                        .child(entry.path.clone()),
                ),
        )
        .child(render_values(entry, theme))
        .into_any_element()
}

fn render_values(entry: &ConfigDiffEntry, theme: &gpui_component::Theme) -> gpui::Div {
    h_flex()
        .items_start()
        .gap_3()
        .child(value_column("原始", entry.source.as_deref(), theme))
        .child(div().pt_1().text_color(theme.muted_foreground).child("→"))
        .child(value_column("最终", entry.effective.as_deref(), theme))
}

fn value_column(
    label: &'static str,
    value: Option<&str>,
    theme: &gpui_component::Theme,
) -> gpui::Div {
    v_flex()
        .min_w_0()
        .flex_1()
        .gap_1()
        .child(
            div()
                .text_size(px(10.))
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(
            div()
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .child(value.unwrap_or("—").to_owned()),
        )
}

fn count_kind(report: &ConfigDiffReport, kind: ConfigDiffKind) -> usize {
    report
        .entries
        .iter()
        .filter(|entry| entry.kind == kind)
        .count()
}
