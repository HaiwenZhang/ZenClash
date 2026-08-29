use super::{
    App, FluentBuilder, Icon, IconName, Input, IntoElement, ParentElement, Styled, Switch, Window,
    div, h_flex, px, v_flex,
};
use gpui::SharedString;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ListPage {
    pub(super) index: usize,
    pub(super) count: usize,
    pub(super) start: usize,
    pub(super) end: usize,
}

pub(super) fn list_page(total: usize, requested_index: usize, page_size: usize) -> ListPage {
    let page_size = page_size.max(1);
    let count = total.div_ceil(page_size).max(1);
    let index = requested_index.min(count - 1);
    let start = index.saturating_mul(page_size).min(total);
    let end = start.saturating_add(page_size).min(total);
    ListPage {
        index,
        count,
        start,
        end,
    }
}

pub(super) fn contains_ascii_case_insensitive(value: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let query = query.as_bytes();
    value
        .as_bytes()
        .windows(query.len())
        .any(|candidate| candidate.eq_ignore_ascii_case(query))
}

pub(super) fn pagination_summary(page: ListPage, total: usize) -> String {
    zenclash_i18n::text_with(
        "common.pagination.summary",
        &[
            ("current", (page.index + 1).to_string()),
            ("total", page.count.to_string()),
            (
                "first",
                if total == 0 {
                    "0".into()
                } else {
                    (page.start + 1).to_string()
                },
            ),
            ("last", page.end.to_string()),
            ("count", total.to_string()),
        ],
    )
}

pub(super) fn setting_card(
    title: impl Into<gpui::SharedString>,
    theme: &gpui_component::Theme,
) -> gpui::Div {
    let title = title.into();
    v_flex()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .when(theme.shadow, |this| this.shadow_sm())
        .overflow_hidden()
        .child(
            h_flex()
                .px_4()
                .py_3()
                .gap_3()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.muted.opacity(0.34))
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(div().size(px(6.)).rounded_full().bg(theme.primary))
                .child(title),
        )
}

pub(super) fn config_input_row(
    label: impl Into<SharedString>,
    description: impl Into<SharedString>,
    input: Input,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    let label = label.into();
    let description = description.into();
    h_flex()
        .min_h(px(64.))
        .px_4()
        .py_3()
        .gap_5()
        .items_start()
        .justify_between()
        .border_b_1()
        .border_color(theme.border)
        .child(
            v_flex()
                .w(px(210.))
                .gap_1()
                .child(div().text_sm().child(label))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(description),
                ),
        )
        .child(div().flex_1().max_w(px(680.)).child(input.cleanable(true)))
        .into_any_element()
}

pub(super) fn info_row(
    label: impl ToString,
    value: impl ToString,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    let label = SharedString::from(label.to_string());
    let value = SharedString::from(value.to_string());
    h_flex()
        .min_h(px(50.))
        .px_4()
        .gap_4()
        .justify_between()
        .border_b_1()
        .border_color(theme.border)
        .child(div().text_sm().child(label))
        .child(
            div()
                .max_w(px(620.))
                .text_right()
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .text_color(theme.muted_foreground)
                .child(if value.is_empty() {
                    SharedString::from("—")
                } else {
                    value
                }),
        )
        .into_any_element()
}

pub(super) fn setting_switch<F>(
    label: impl Into<gpui::SharedString>,
    description: impl Into<gpui::SharedString>,
    checked: bool,
    id: &'static str,
    theme: &gpui_component::Theme,
    listener: F,
) -> gpui::AnyElement
where
    F: Fn(&bool, &mut Window, &mut App) + 'static,
{
    let label = label.into();
    let description = description.into();
    h_flex()
        .min_h(px(58.))
        .px_4()
        .gap_4()
        .justify_between()
        .border_b_1()
        .border_color(theme.border)
        .child(
            v_flex().gap_1().child(div().text_sm().child(label)).child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(description),
            ),
        )
        .child(Switch::new(id).checked(checked).on_click(listener))
        .into_any_element()
}

pub(super) fn metric(
    label: impl ToString,
    value: String,
    color: gpui::Hsla,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    let label = SharedString::from(label.to_string());
    v_flex()
        .relative()
        .min_w(px(190.))
        .min_h(px(104.))
        .flex_1()
        .justify_between()
        .gap_2()
        .p_4()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top_0()
                .h(px(3.))
                .bg(color),
        )
        .child(
            div()
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(
            div()
                .font_family(theme.mono_font_family.clone())
                .text_lg()
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(color)
                .child(value),
        )
        .into_any_element()
}

pub(super) fn message_banner(
    message: String,
    color: gpui::Hsla,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    h_flex()
        .gap_3()
        .p_3()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(color.opacity(0.55))
        .bg(color.opacity(0.1))
        .text_sm()
        .text_color(color)
        .child(
            div()
                .size(px(28.))
                .flex_shrink_0()
                .rounded(theme.radius)
                .bg(color.opacity(0.14))
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(IconName::Info).size_4()),
        )
        .child(div().flex_1().min_w_0().whitespace_normal().child(message))
        .into_any_element()
}

pub(super) fn empty_state(
    message: impl Into<gpui::SharedString>,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    let message = message.into();
    div()
        .p_5()
        .text_center()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(message)
        .into_any_element()
}

pub(super) fn format_port(port: u16) -> String {
    if port == 0 {
        zenclash_i18n::text("common.status.not_listening")
    } else {
        format!("127.0.0.1:{port}")
    }
}

pub(super) fn format_proxy(server: &str, port: u16, enabled: bool) -> String {
    if !enabled {
        zenclash_i18n::text("common.status.disabled")
    } else if server.trim().is_empty() || port == 0 {
        zenclash_i18n::text("common.status.misconfigured")
    } else {
        format!("{server}:{port}")
    }
}

pub(super) fn format_bytes(bytes: u64) -> String {
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format_decimal_bytes(bytes, 1_024, "KiB"),
        1_048_576..=1_073_741_823 => format_decimal_bytes(bytes, 1_048_576, "MiB"),
        _ => format_decimal_bytes(bytes, 1_073_741_824, "GiB"),
    }
}

fn format_decimal_bytes(bytes: u64, divisor: u64, unit: &str) -> String {
    let tenths = (u128::from(bytes) * 10 + u128::from(divisor / 2)) / u128::from(divisor);
    format!("{}.{:01} {unit}", tenths / 10, tenths % 10)
}

pub(super) fn normalized_fraction(value: u64, maximum: u64) -> f32 {
    let thousandths = (u128::from(value) * 1_000 / u128::from(maximum.max(1))).min(1_000);
    f32::from(u16::try_from(thousandths).unwrap_or(1_000)) / 1_000.0
}

pub(super) fn compact_text(value: &str, maximum: usize) -> String {
    let character_count = value.chars().count();
    if character_count <= maximum {
        return value.into();
    }
    if maximum == 0 {
        return String::new();
    }
    if maximum == 1 {
        return "…".into();
    }

    let content_characters = maximum - 1;
    let prefix_characters = content_characters.div_ceil(2);
    let suffix_characters = content_characters / 2;
    let prefix_end = value
        .char_indices()
        .nth(prefix_characters)
        .map_or(value.len(), |(index, _)| index);
    let suffix_start = value
        .char_indices()
        .rev()
        .nth(suffix_characters - 1)
        .map_or(0, |(index, _)| index);
    format!("{}…{}", &value[..prefix_end], &value[suffix_start..])
}

pub(super) fn format_profile_age(updated_at: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let elapsed = now.saturating_sub(updated_at);
    match elapsed {
        0..=59 => zenclash_i18n::text("common.time.just_now"),
        60..=3_599 => zenclash_i18n::text_with(
            "common.time.minutes_ago",
            &[("count", (elapsed / 60).to_string())],
        ),
        3_600..=86_399 => zenclash_i18n::text_with(
            "common.time.hours_ago",
            &[("count", (elapsed / 3_600).to_string())],
        ),
        _ => zenclash_i18n::text_with(
            "common.time.days_ago",
            &[("count", (elapsed / 86_400).to_string())],
        ),
    }
}

pub(super) fn empty_dash(value: &str) -> String {
    if value.trim().is_empty() {
        "—".into()
    } else {
        value.into()
    }
}

pub(super) fn yes_no(value: bool) -> String {
    if value {
        zenclash_i18n::text("common.status.enabled")
    } else {
        zenclash_i18n::text("common.status.disabled")
    }
}

#[cfg(test)]
mod tests {
    use super::{compact_text, contains_ascii_case_insensitive, list_page};

    #[test]
    fn list_page_clamps_a_stale_page_after_the_collection_shrinks() {
        assert_eq!(
            list_page(30, 20, 100),
            super::ListPage {
                index: 0,
                count: 1,
                start: 0,
                end: 30,
            }
        );
    }

    #[test]
    fn list_page_keeps_the_short_final_page_within_bounds() {
        assert_eq!(
            list_page(205, 2, 100),
            super::ListPage {
                index: 2,
                count: 3,
                start: 200,
                end: 205,
            }
        );
    }

    #[test]
    fn ascii_case_insensitive_search_does_not_require_lowercase_copies() {
        assert!(contains_ascii_case_insensitive("Example.COM", "example"));
        assert!(contains_ascii_case_insensitive("节点-A", "节点"));
        assert!(!contains_ascii_case_insensitive("DIRECT", "proxy"));
    }

    #[test]
    fn compact_text_preserves_a_value_within_the_limit() {
        assert_eq!(compact_text("节点-A", 4), "节点-A");
    }

    #[test]
    fn compact_text_returns_empty_for_a_zero_character_limit() {
        assert_eq!(compact_text("abcdef", 0), "");
    }

    #[test]
    fn compact_text_returns_an_ellipsis_for_a_one_character_limit() {
        assert_eq!(compact_text("abcdef", 1), "…");
    }

    #[test]
    fn compact_text_keeps_unicode_boundaries_and_exact_width() {
        assert_eq!(compact_text("订阅地址很长.example", 7), "订阅地…ple");
    }
}
