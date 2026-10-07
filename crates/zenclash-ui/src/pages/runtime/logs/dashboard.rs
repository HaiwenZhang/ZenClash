use super::*;
use gpui_kit::component::button::{ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, WindowExt};

use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::{InteractiveElement, TestSupportExt};

impl RuntimePage {
    pub(in crate::pages::runtime) fn render_log_actions(
        &self,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let presentation = &self.logs.presentation;
        h_flex()
            .gap_2()
            .flex_wrap()
            .child(
                Button::new("pause-logs-display")
                    .label(zenclash_i18n::text(if self.logs.paused {
                        "common.actions.resume_display"
                    } else {
                        "common.actions.pause_display"
                    }))
                    .tooltip(zenclash_i18n::text("logs.display_pause_description"))
                    .small()
                    .h_10()
                    .outline()
                    .disabled(self.logs.presentation.revision.is_none())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.logs.paused = !this.logs.paused;
                        this.logs.cancel_refresh();
                        this.logs.last_refresh = None;
                        this.update_log_presentation(cx);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("export-logs")
                    .icon(gpui_kit::assets::IconName::Upload)
                    .label(zenclash_i18n::text("logs.actions.export"))
                    .small()
                    .h_10()
                    .outline()
                    .loading(self.logs.exporting)
                    .disabled(presentation.entries.is_empty() || self.logs.exporting)
                    .on_click(cx.listener(|this, _, _, cx| this.choose_log_export(cx))),
            )
            .child(
                Button::new("log-settings")
                    .outline()
                    .h_10()
                    .label(zenclash_i18n::text("redesign.log_settings"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_log_settings(window, cx)),
                    ),
            )
    }

    pub(in crate::pages::runtime) fn log_stream_status(
        &self,
        theme: &gpui_kit::component::Theme,
    ) -> gpui_kit::Div {
        let color = if self.logs.connected {
            theme.success
        } else {
            theme.muted_foreground
        };
        h_flex()
            .gap_2()
            .text_sm()
            .text_color(color)
            .child(div().size_2().rounded_full().bg(color))
            .child(zenclash_i18n::text(if self.logs.paused {
                "redesign.display_paused"
            } else if self.logs.connected {
                "redesign.receiving"
            } else {
                "logs.stream.reconnecting"
            }))
    }

    pub(super) fn log_dashboard(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let presentation = &self.logs.presentation;
        let ready = presentation.revision.is_some();
        let mut table = panel(theme)
            .id("logs-table")
            .test_support()
            .max_w_full()
            .gap_0()
            .flex_1()
            .min_h_0()
            .w_full()
            .min_w_0()
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .pb_3()
                    .flex_wrap()
                    .child(
                        div().flex_1().min_w(gpui_kit::rems(12.)).child(
                            Input::new(&self.logs.filter)
                                .prefix(gpui_kit::component::Icon::new(IconName::Search))
                                .small()
                                .h_10(),
                        ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("redesign.core_logs")),
                    )
                    .child(self.log_level_filters(cx))
                    .child(
                        Button::new("clear-logs")
                            .icon(gpui_kit::assets::IconName::Trash)
                            .label(zenclash_i18n::text("redesign.clear_log_list"))
                            .small()
                            .h_10()
                            .outline()
                            .disabled(presentation.entries.is_empty())
                            .on_click(cx.listener(|this, _, window, cx| {
                                let owner = cx.entity().downgrade();
                                if window.focused(cx).is_none() {
                                    window.focus(&this.focus_handle, cx);
                                }
                                window.open_alert_dialog(cx, move |dialog, _, _| {
                                    let owner = owner.clone();
                                    dialog
                                        .confirm()
                                        .title(zenclash_i18n::text("logs.ui.clear_title"))
                                        .description(zenclash_i18n::text(
                                            "logs.ui.clear_description",
                                        ))
                                        .ok_text(zenclash_i18n::text("logs.ui.clear_confirm"))
                                        .ok_variant(
                                            gpui_kit::component::button::ButtonVariant::Danger,
                                        )
                                        .on_ok(move |_, _, cx| {
                                            let _ = owner.update(cx, |this, cx| {
                                                this.log_monitor.clear();
                                                this.logs.paused = false;
                                                this.logs.selected = None;
                                                this.logs.page = 0;
                                                this.logs.cancel_refresh();
                                                this.update_log_presentation(cx);
                                                cx.notify();
                                            });
                                            true
                                        })
                                });
                            })),
                    ),
            )
            .child(log_columns(theme));
        let owner = cx.entity().downgrade();
        table = table.child(
            div()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .when(presentation.matches.is_empty(), |view| {
                    view.child(empty_state(
                        zenclash_i18n::text(if !ready {
                            "runtime.empty.loading"
                        } else if presentation.entries.is_empty() {
                            "logs.empty.waiting"
                        } else {
                            "logs.empty.filtered"
                        }),
                        theme,
                    ))
                })
                .when(!presentation.matches.is_empty(), |view| {
                    view.child(
                        gpui_kit::uniform_list(
                            "log-table-viewport",
                            presentation.matches.len(),
                            move |range, _, cx| {
                                owner
                                    .update(cx, |page, cx| {
                                        let theme = cx.theme().clone();
                                        range
                                            .filter_map(|position| {
                                                page.logs
                                                    .presentation
                                                    .matches
                                                    .get(position)
                                                    .copied()
                                            })
                                            .map(|index| page.render_log_row(index, &theme, cx))
                                            .collect::<Vec<_>>()
                                    })
                                    .unwrap_or_default()
                            },
                        )
                        .track_scroll(&self.logs.scroll)
                        .h_full()
                        .w_full(),
                    )
                })
                .vertical_scrollbar(&self.logs.scroll),
        );
        table = table.child(
            h_flex()
                .justify_between()
                .flex_wrap()
                .gap_2()
                .pt_3()
                .child(div().text_sm().text_color(theme.muted_foreground).child(
                    zenclash_i18n::text_with(
                        "redesign.log_count",
                        &[("count", presentation.matches.len().to_string())],
                    ),
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .child(self.render_log_order(cx))
                        .child(
                            div()
                                .text_sm()
                                .child(zenclash_i18n::text("redesign.auto_scroll")),
                        )
                        .child(
                            crate::components::mint_switch::MintSwitch::new("log-auto-scroll")
                                .accessibility_label(zenclash_i18n::text("redesign.auto_scroll"))
                                .checked(self.logs.auto_scroll)
                                .on_click(cx.listener(|page, checked, _, cx| {
                                    page.logs.auto_scroll = *checked;
                                    if *checked {
                                        page.jump_to_latest_log(cx);
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("jump-to-latest-log")
                                .outline()
                                .small()
                                .label(zenclash_i18n::text("redesign.jump_latest"))
                                .on_click(
                                    cx.listener(|page, _, _, cx| page.jump_to_latest_log(cx)),
                                ),
                        ),
                ),
        );
        v_flex()
            .h_full()
            .min_h_0()
            .gap_3()
            .child(
                h_flex().gap_4().py_2().flex_wrap().children(
                    [
                        (None, "logs.levels.all"),
                        (Some("INFO"), "redesign.log_info"),
                        (Some("WARNING"), "redesign.log_warning"),
                        (Some("ERROR"), "redesign.log_error"),
                    ]
                    .into_iter()
                    .map(|(level, label)| {
                        let count = level.map_or(presentation.entries.len() as u64, |level| {
                            presentation
                                .level_counts
                                .iter()
                                .find(|(name, _)| name == level)
                                .map_or(0, |(_, count)| *count)
                        });
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .text_color(
                                level.map_or(theme.foreground, |level| level_color(level, theme)),
                            )
                            .child(zenclash_i18n::text(label))
                            .child(
                                div()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child(count.to_string()),
                            )
                    }),
                ),
            )
            .child(table)
            .into_any_element()
    }

    fn render_log_row(
        &self,
        index: usize,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let presentation = &self.logs.presentation;
        let row = &presentation.rows[index];
        let entry = presentation.entries[index].clone();
        let selected = self
            .logs
            .selected
            .as_ref()
            .is_some_and(|selected| Arc::ptr_eq(&selected.0, &entry));
        let id = Arc::as_ptr(&entry) as usize;
        let selected_row = row.clone();
        let detail_entry = entry.clone();
        let detail_row = row.clone();
        h_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .h(gpui_kit::rems(2.75))
            .py_1()
            .px_2()
            .border_b_1()
            .border_color(theme.border)
            .when(selected, |row| {
                row.rounded(theme.radius).bg(theme.table_active)
            })
            .child(
                div()
                    .w_24()
                    .flex_shrink_0()
                    .text_sm()
                    .truncate()
                    .child(row.time.clone()),
            )
            .child(
                h_flex()
                    .w(gpui_kit::rems(7.))
                    .flex_shrink_0()
                    .child(level_badge(row.level.clone(), theme)),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Button::new(("inspect-log", id))
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .text_sm()
                                .truncate()
                                .font_family(theme.mono_font_family.clone())
                                .child(row.payload.clone()),
                        )
                        .accessibility_label(zenclash_i18n::text("logs.actions.inspect"))
                        .tooltip(row.payload.clone())
                        .small()
                        .w_full()
                        .justify_start()
                        .px_0()
                        .when(selected, |button| {
                            button.font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        })
                        .custom(ButtonCustomVariant::new(cx).foreground(theme.foreground))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.logs.selected = Some((entry.clone(), selected_row.clone()));
                            this.open_log_details(window, cx);
                        })),
                ),
            )
            .child(
                Button::new(("log-details", id))
                    .small()
                    .ghost()
                    .w_20()
                    .label(zenclash_i18n::text("redesign.details"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.logs.selected = Some((detail_entry.clone(), detail_row.clone()));
                        this.open_log_details(window, cx);
                    })),
            )
            .into_any_element()
    }

    fn log_level_filters(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        h_flex().gap_1().flex_wrap().children(
            [
                None,
                Some("INFO"),
                Some("WARNING"),
                Some("ERROR"),
                Some("DEBUG"),
            ]
            .into_iter()
            .map(|level| {
                let label = level.map_or_else(
                    || zenclash_i18n::text("logs.levels.all"),
                    |level| {
                        zenclash_i18n::text(match level {
                            "INFO" => "redesign.log_info",
                            "WARNING" => "redesign.log_warning",
                            "ERROR" => "redesign.log_error",
                            _ => "redesign.log_debug",
                        })
                    },
                );
                Button::new((
                    gpui_kit::ElementId::from("log-level-filter"),
                    level.unwrap_or("ALL").to_owned(),
                ))
                .label(label)
                .small()
                .h_10()
                .outline()
                .selected(self.logs.level_filter.as_deref() == level)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.logs.level_filter = level.map(str::to_owned);
                    this.logs.page = 0;
                    this.logs.cancel_refresh();
                    this.update_log_presentation(cx);
                    cx.notify();
                }))
            }),
        )
    }

    fn render_log_order(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let oldest_first = self.logs.oldest_first;
        let owner = cx.entity().downgrade();
        Button::new("log-display-order")
            .small()
            .h_10()
            .outline()
            .dropdown_caret(true)
            .label(zenclash_i18n::text(if oldest_first {
                "logs.ui.oldest"
            } else {
                "logs.ui.latest"
            }))
            .dropdown_menu(move |mut menu, _, _| {
                for (value, label) in [(false, "logs.ui.latest"), (true, "logs.ui.oldest")] {
                    let owner = owner.clone();
                    menu = menu.item(
                        PopupMenuItem::new(zenclash_i18n::text(label))
                            .checked(oldest_first == value)
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |page, cx| {
                                    page.logs.oldest_first = value;
                                    page.logs.page = 0;
                                    page.logs.cancel_refresh();
                                    page.update_log_presentation(cx);
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            })
    }
}

fn panel(theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    v_flex()
        .gap_3()
        .p_4()
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius_lg)
        .bg(theme.group_box)
}

fn level_color(level: &str, theme: &gpui_kit::component::Theme) -> gpui_kit::Hsla {
    match level {
        "ERROR" => theme.danger,
        "WARNING" | "WARN" => theme.warning,
        "DEBUG" => theme.muted_foreground,
        _ => theme.muted_foreground,
    }
}

fn log_columns(theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    h_flex()
        .gap_3()
        .py_2()
        .px_2()
        .bg(theme.table_head)
        .rounded(theme.radius)
        .text_sm()
        .text_color(theme.muted_foreground)
        .border_b_1()
        .border_color(theme.border)
        .child(div().w_24().child(zenclash_i18n::text("logs.columns.time")))
        .child(
            div()
                .w(gpui_kit::rems(7.))
                .flex_shrink_0()
                .child(zenclash_i18n::text("logs.columns.level")),
        )
        .child(
            div()
                .flex_1()
                .child(zenclash_i18n::text("logs.columns.message")),
        )
        .child(div().w_20().child(zenclash_i18n::text("redesign.details")))
}

fn level_badge(level: gpui_kit::SharedString, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    let color = level_color(&level, theme);
    h_flex()
        .gap_1()
        .text_xs()
        .px_2()
        .py_0p5()
        .rounded(theme.radius)
        .bg(color.opacity(0.12))
        .text_color(color)
        .when(
            level.as_ref() == "WARNING" || level.as_ref() == "ERROR",
            |badge| {
                badge.child(
                    gpui_kit::component::Icon::new(if level.as_ref() == "ERROR" {
                        IconName::CircleX
                    } else {
                        IconName::TriangleAlert
                    })
                    .size_3(),
                )
            },
        )
        .child(level)
}
