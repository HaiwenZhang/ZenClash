use super::*;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement;

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
                Button::new("copy-support-safe-logs")
                    .icon(IconName::Copy)
                    .label(zenclash_i18n::text("logs.actions.copy_safe"))
                    .small()
                    .outline()
                    .loading(self.logs.copying)
                    .disabled(presentation.entries.is_empty() || self.logs.copying)
                    .on_click(cx.listener(|this, _, _, cx| this.copy_support_safe_logs(cx))),
            )
            .child(
                Button::new("export-logs")
                    .icon(crate::assets::AppIcon::SquareArrowRightExit)
                    .label(zenclash_i18n::text("logs.actions.export"))
                    .small()
                    .outline()
                    .loading(self.logs.exporting)
                    .disabled(presentation.entries.is_empty() || self.logs.exporting)
                    .on_click(cx.listener(|this, _, _, cx| this.choose_log_export(cx))),
            )
            .child(
                Button::new("clear-logs")
                    .icon(IconName::CircleX)
                    .label(zenclash_i18n::text("logs.actions.clear"))
                    .small()
                    .outline()
                    .disabled(presentation.entries.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.log_monitor.clear();
                        this.logs.paused = false;
                        this.logs.selected = None;
                        this.logs.page = 0;
                        this.logs.cancel_refresh();
                        this.update_log_presentation(cx);
                        cx.notify();
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
            .w_full()
            .min_w(gpui_kit::rems(42.))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        div().flex_1().min_w(gpui_kit::rems(12.)).child(
                            Input::new(&self.logs.filter)
                                .prefix(gpui_kit::component::Icon::new(IconName::Search))
                                .small(),
                        ),
                    )
                    .child(self.log_level_filters(cx)),
            )
            .child(log_columns(theme))
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
            let color = level_color(&row.level, theme);
            table = table.child(
                h_flex()
                    .gap_3()
                    .py_2()
                    .px_2()
                    .rounded(theme.radius)
                    .border_b_1()
                    .border_color(theme.border)
                    .when(selected, |row| row.bg(theme.primary.opacity(0.1)))
                    .child(div().w_24().text_xs().truncate().child(row.time.clone()))
                    .child(
                        div()
                            .w_20()
                            .text_xs()
                            .text_color(color)
                            .child(row.level.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .truncate()
                            .child(row.payload.clone()),
                    )
                    .child(
                        div()
                            .w_20()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(time_source(row.time_source)),
                    )
                    .child(
                        Button::new(("inspect-log", id))
                            .icon(IconName::Eye)
                            .accessibility_label(zenclash_i18n::text("logs.actions.inspect"))
                            .tooltip(zenclash_i18n::text("logs.actions.inspect"))
                            .w_8()
                            .small()
                            .ghost()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.logs.selected = Some((entry.clone(), selected_row.clone()));
                                cx.notify();
                            })),
                    ),
            );
        }
        table = table.child(
            h_flex()
                .justify_between()
                .pt_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(pagination_summary(page, presentation.matches.len())),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("previous-logs-page")
                                .icon(IconName::ChevronLeft)
                                .small()
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
                                .outline()
                                .label(zenclash_i18n::text("common.actions.next_page"))
                                .disabled(page.index + 1 >= page.count)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_logs_page(page.index + 1, cx)
                                })),
                        ),
                ),
        );
        let mut details = panel(theme).child(
            h_flex()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(zenclash_i18n::text("logs.details.title")),
                )
                .child(
                    Button::new("pause-logs-display")
                        .label(zenclash_i18n::text(if self.logs.paused {
                            "common.actions.resume_display"
                        } else {
                            "common.actions.pause_display"
                        }))
                        .tooltip(zenclash_i18n::text("logs.display_pause_description"))
                        .small()
                        .outline()
                        .disabled(!ready)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.logs.paused = !this.logs.paused;
                            this.logs.cancel_refresh();
                            this.logs.last_refresh = None;
                            this.update_log_presentation(cx);
                            cx.notify();
                        })),
                ),
        );
        if let Some((_, row)) = &self.logs.selected {
            details = details
                .child(div().text_sm().child(row.payload.clone()))
                .child(info_row(
                    zenclash_i18n::text("logs.columns.level"),
                    row.level.clone(),
                    theme,
                ))
                .child(info_row(
                    zenclash_i18n::text("logs.columns.time"),
                    row.time.clone(),
                    theme,
                ))
                .child(info_row(
                    zenclash_i18n::text("logs.columns.source"),
                    time_source(row.time_source),
                    theme,
                ))
                .when_some(row.fields.clone(), |this, fields| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("logs.details.fields")),
                    )
                    .child(
                        div()
                            .p_3()
                            .bg(theme.muted)
                            .rounded(theme.radius)
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .child(fields),
                    )
                });
        } else {
            details = details.child(empty_state(
                zenclash_i18n::text("logs.empty.waiting"),
                theme,
            ));
        }
        v_flex()
            .gap_4()
            .child(render_log_header(
                presentation.entries.len(),
                presentation.matches.len(),
                presentation.query.is_empty() && presentation.level_filter.is_none(),
                ready.then_some(self.logs.connected),
                &self.logs.persistence,
                theme,
            ))
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .flex_wrap()
                    .child(
                        div()
                            .flex_1()
                            .flex_basis(gpui_kit::rems(42.))
                            .min_w_0()
                            .child(table)
                            .overflow_x_scrollbar(),
                    )
                    .child(
                        v_flex()
                            .w_80()
                            .flex_shrink_0()
                            .gap_3()
                            .child(details)
                            .child(level_distribution(&presentation.level_counts, theme))
                            .child(self.render_log_persistence(theme, cx)),
                    ),
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

fn time_source(source: LogTimeSource) -> String {
    zenclash_i18n::text(match source {
        LogTimeSource::Core => "logs.time.core",
        LogTimeSource::LocalReceive => "logs.time.local_receive",
    })
}

fn level_color(level: &str, theme: &gpui_kit::component::Theme) -> gpui_kit::Hsla {
    match level {
        "ERROR" => theme.danger,
        "WARNING" | "WARN" => theme.warning,
        "DEBUG" => theme.muted_foreground,
        _ => theme.info,
    }
}

fn level_distribution(
    values: &[(String, u64)],
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    let maximum = values.iter().map(|value| value.1).max().unwrap_or(1).max(1);
    panel(theme)
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(zenclash_i18n::text("logs.charts.levels")),
        )
        .children(values.iter().map(|(level, count)| {
            h_flex()
                .gap_3()
                .child(div().w_20().text_xs().child(level.clone()))
                .child(
                    Progress::new((gpui_kit::ElementId::from("log-level-share"), level.clone()))
                        .accessibility_label(level.clone())
                        .value(*count as f32 / maximum as f32 * 100.)
                        .color(level_color(level, theme))
                        .flex_1(),
                )
                .child(div().w_10().text_xs().text_right().child(count.to_string()))
        }))
}

fn log_columns(theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    h_flex()
        .gap_3()
        .py_2()
        .px_2()
        .bg(theme.table_head)
        .rounded(theme.radius)
        .text_xs()
        .text_color(theme.muted_foreground)
        .border_b_1()
        .border_color(theme.border)
        .child(div().w_24().child(zenclash_i18n::text("logs.columns.time")))
        .child(
            div()
                .w_20()
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
        .child(div().w_8())
}
