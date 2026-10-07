use super::*;
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};

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
                Button::new("copy-support-safe-logs")
                    .icon(gpui_kit::assets::IconName::Clipboard)
                    .label(zenclash_i18n::text("logs.actions.copy_safe"))
                    .small()
                    .h_10()
                    .outline()
                    .loading(self.logs.copying)
                    .disabled(presentation.entries.is_empty() || self.logs.copying)
                    .on_click(cx.listener(|this, _, _, cx| this.copy_support_safe_logs(cx))),
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
                Button::new("clear-logs")
                    .icon(gpui_kit::assets::IconName::Trash)
                    .label(zenclash_i18n::text("logs.actions.clear"))
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
                                .description(zenclash_i18n::text("logs.ui.clear_description"))
                                .ok_text(zenclash_i18n::text("logs.ui.clear_confirm"))
                                .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
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
            )
    }

    pub(super) fn log_dashboard(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let presentation = &self.logs.presentation;
        let ready = presentation.revision.is_some();
        let page = list_page(presentation.matches.len(), self.logs.page, LOGS_PER_PAGE);
        let mut table = panel(theme)
            .id("logs-table")
            .test_support()
            .max_w_full()
            .gap_0()
            .min_h(gpui_kit::rems(49.))
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
                    .child(self.log_level_filters(cx)),
            )
            .child(log_columns(theme));
        let mut rows = v_flex()
            .gap_0()
            .when(presentation.matches.is_empty(), |this| {
                this.child(empty_state(
                    zenclash_i18n::text(if !ready {
                        "runtime.empty.loading"
                    } else if presentation.entries.is_empty() {
                        "logs.empty.waiting"
                    } else {
                        "logs.empty.filtered"
                    }),
                    theme,
                ))
            });
        for &index in &presentation.matches[page.start..page.end] {
            let row = &presentation.rows[index];
            let entry = presentation.entries[index].clone();
            let selected = self
                .logs
                .selected
                .as_ref()
                .is_some_and(|selected| Arc::ptr_eq(&selected.0, &entry));
            let id = Arc::as_ptr(&entry) as usize;
            let selected_row = row.clone();
            rows = rows.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .min_h(gpui_kit::rems(2.5))
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
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.logs.selected =
                                        Some((entry.clone(), selected_row.clone()));
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        div()
                            .w_20()
                            .flex_shrink_0()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(column_source(row.time_source)),
                    ),
            );
        }
        table = table.child(
            div()
                .id(("log-table-viewport", page.index))
                .h(gpui_kit::rems(30.))
                .overflow_y_scrollbar()
                .child(rows),
        );
        table = table.child(div().flex_1()).child(
            h_flex()
                .justify_between()
                .flex_wrap()
                .gap_2()
                .pt_4()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(pagination_summary(page, presentation.matches.len())),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(self.render_log_order(cx))
                        .child(
                            Button::new("previous-logs-page")
                                .icon(IconName::ChevronLeft)
                                .small()
                                .h_10()
                                .outline()
                                .label(zenclash_i18n::text("common.actions.previous_page"))
                                .disabled(page.index == 0)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_logs_page(page.index.saturating_sub(1), cx)
                                })),
                        )
                        .child(
                            Button::new("next-logs-page")
                                .icon(IconName::ChevronRight)
                                .small()
                                .h_10()
                                .outline()
                                .label(zenclash_i18n::text("common.actions.next_page"))
                                .disabled(page.index + 1 >= page.count)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_logs_page(page.index + 1, cx)
                                })),
                        ),
                ),
        );
        v_flex()
            .gap_3()
            .child(
                render_log_header(
                    presentation.entries.len(),
                    presentation.matches.len(),
                    presentation.query.is_empty() && presentation.level_filter.is_none(),
                    ready.then_some(self.logs.connected),
                    presentation.rows.last().map(|row| row.time.clone()),
                    theme,
                )
                .child(div().flex_1())
                .child(self.render_log_collection_level(cx)),
            )
            .child(table)
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
                let label =
                    level.map_or_else(|| zenclash_i18n::text("logs.levels.all"), str::to_owned);
                Button::new((
                    gpui_kit::ElementId::from("log-level-filter"),
                    level.unwrap_or("ALL").to_owned(),
                ))
                .label(format!(
                    "{label} {}",
                    level.map_or(self.logs.presentation.entries.len() as u64, |level| {
                        self.logs
                            .presentation
                            .level_counts
                            .iter()
                            .find(|(name, _)| name == level)
                            .map_or(0, |(_, count)| *count)
                    })
                ))
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

    fn render_log_collection_level(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let current = self.logs.level;
        let owner = cx.entity().downgrade();
        h_flex()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .child(zenclash_i18n::text("logs.collection_level")),
            )
            .child(
                Button::new("log-collection-level")
                    .tooltip(current.map_or_else(
                        || zenclash_i18n::text("runtime.empty.loading"),
                        log_level_description,
                    ))
                    .small()
                    .h_10()
                    .outline()
                    .dropdown_caret(true)
                    .label(
                        current
                            .map_or_else(|| "—".into(), |level| level.api_value().to_uppercase()),
                    )
                    .disabled(
                        self.core_busy()
                            || self.config_inputs_loading
                            || !self
                                .config_inputs
                                .is_for_profile(self.profile_path.as_deref()),
                    )
                    .dropdown_menu(move |mut menu, _, _| {
                        for level in [
                            MihomoLogLevel::Info,
                            MihomoLogLevel::Debug,
                            MihomoLogLevel::Warning,
                            MihomoLogLevel::Error,
                            MihomoLogLevel::Silent,
                        ] {
                            let owner = owner.clone();
                            menu = menu.item(
                                PopupMenuItem::new(level.api_value().to_uppercase())
                                    .checked(current == Some(level))
                                    .on_click(move |_, _, cx| {
                                        let _ =
                                            owner.update(cx, |page, cx| {
                                                page.apply_controlled_config(
                                        serde_json::json!({"log-level": level.api_value()}),
                                        zenclash_i18n::text("logs.collection_level_updated"), cx);
                                            });
                                    }),
                            );
                        }
                        menu
                    }),
            )
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

fn column_source(source: LogTimeSource) -> String {
    zenclash_i18n::text(match source {
        LogTimeSource::Core => "logs.ui.core",
        LogTimeSource::LocalReceive => "logs.ui.local",
    })
}

fn level_color(level: &str, theme: &gpui_kit::component::Theme) -> gpui_kit::Hsla {
    match level {
        "ERROR" => theme.danger,
        "WARNING" | "WARN" => theme.warning,
        "DEBUG" => theme.chart_1.opacity(0.65),
        _ => theme.chart_1,
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
        .child(
            div()
                .w_20()
                .child(zenclash_i18n::text("logs.columns.source")),
        )
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
