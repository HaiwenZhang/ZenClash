mod detail;
mod summary;

use super::*;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::{InteractiveElement, StatefulInteractiveElement, TestSupportExt};

impl ConnectionsView {
    fn update_projection_immediate(&mut self, cx: &mut Context<Self>) {
        let mut data = match self
            .paused_snapshot
            .as_ref()
            .or(self.live_snapshot.as_ref())
        {
            Some(snapshot) => snapshot.clone(),
            None => {
                self.release_presentation();
                return;
            }
        };
        let Some(runtime_context) = self.runtime_context.clone() else {
            return;
        };
        if self.paused_snapshot.is_none() {
            self.timeline.observe(
                runtime_context.configuration_generation,
                data.clone(),
                std::time::Instant::now(),
            );
        }
        self.history
            .observe(runtime_context.configuration_generation, &data);
        if self.show_closed {
            data = self.timeline.closed();
        }

        self.worker.cancel();
        let show_closed = self.show_closed;
        let policy = self.worker.policy.clone();
        let query = self.query.clone();
        let transport = self.transport;
        let sort = self.sort;
        let expected_snapshot = data.clone();
        let expected_policy = policy.clone();
        let expected_query = query.clone();
        let task = self.runtime.spawn_blocking(move || {
            let mut order = present_connections(&data.connections, &query, transport, sort);
            if let Some(policy) = &policy {
                order.retain(|&index| data.connections[index].chains.last() == Some(policy));
            }
            let policies = data
                .connections
                .iter()
                .filter_map(|connection| connection.chains.last().map(String::as_str))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .map(str::to_owned)
                .collect();
            let (processes, protocols) = connection_distributions(&data.connections);
            let by_id = data
                .connections
                .iter()
                .enumerate()
                .map(|(index, connection)| (connection.id.clone(), index))
                .collect();
            let now = chrono::Utc::now();
            let durations = data
                .connections
                .iter()
                .map(|connection| projection::connection_duration(&connection.start, now))
                .collect();
            projection::ConnectionProjection {
                snapshot: data,
                query,
                transport,
                sort,
                order,
                processes,
                protocols,
                by_id,
                durations,
                policies,
            }
        });
        self.projecting = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await.map_err(|error| error.to_string());
            let _ = this.update(cx, |this, cx| {
                let current_snapshot = if show_closed {
                    Some(this.timeline.closed())
                } else {
                    this.paused_snapshot
                        .as_ref()
                        .or(this.live_snapshot.as_ref())
                        .cloned()
                };
                let still_current = this.runtime_context.as_ref() == Some(&runtime_context)
                    && this.show_closed == show_closed
                    && this.worker.policy == expected_policy
                    && this.query == expected_query
                    && this.transport == transport
                    && this.sort == sort
                    && current_snapshot.as_ref().is_some_and(|snapshot| {
                        std::sync::Arc::ptr_eq(snapshot, &expected_snapshot)
                    });
                if !still_current {
                    return;
                }
                this.worker.cancel();
                this.projecting = false;
                match result {
                    Ok(projection) => {
                        this.projection = Some(projection);
                    }
                    Err(error) => cx.emit(ConnectionEvent::PresentationFailed(
                        zenclash_i18n::text_with(
                            "connections.errors.filter_task",
                            &[("error", error)],
                        ),
                    )),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(in crate::features) fn render_connection_close_all(
        &self,
        cx: &mut Context<Self>,
    ) -> Button {
        let empty = self
            .live_snapshot
            .as_ref()
            .is_none_or(|snapshot| snapshot.connections.is_empty());

        Button::new("close-all-connections")
            .icon(gpui_kit::assets::IconName::Square)
            .label(zenclash_i18n::text("connections.actions.close_all"))
            .outline()
            .danger()
            .h_10()
            .small()
            .disabled(
                self.show_closed
                    || empty
                    || !self.close_allowed
                    || !self.closing_connections.is_empty(),
            )
            .on_click(
                cx.listener(|this, _, window, cx| this.confirm_connection_close(None, window, cx)),
            )
    }

    pub(super) fn connection_dashboard(
        &self,
        compact: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let Some(projection) = &self.projection else {
            return empty_state(
                zenclash_i18n::text(if self.projecting {
                    "connections.filtering"
                } else {
                    "connections.empty.active"
                }),
                theme,
            )
            .into_any_element();
        };
        let data = &projection.snapshot;
        let metrics = self
            .paused_snapshot
            .as_ref()
            .or(self.live_snapshot.as_ref())
            .unwrap_or(data);
        let page = list_page(
            projection.order.len(),
            self.page_index,
            CONNECTIONS_PER_PAGE,
        );
        let mut inline_details = Some(self.render_connection_inspector(projection, theme, cx));
        let tabs = h_flex()
            .gap_1()
            .children([false, true].into_iter().map(|closed| {
                use gpui_kit::component::Selectable as _;
                let count = if closed {
                    self.timeline.closed().connections.len()
                } else {
                    self.timeline.active_count()
                };
                Button::new(if closed {
                    "connections-closed"
                } else {
                    "connections-active"
                })
                .label(format!(
                    "{}  {count}",
                    zenclash_i18n::text(if closed {
                        "unified.connections.closed"
                    } else {
                        "unified.connections.active"
                    })
                ))
                .outline()
                .selected(self.show_closed == closed)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.show_closed = closed;
                    this.page_index = 0;
                    this.paused_snapshot = None;
                    this.update_projection_immediate(cx);
                    cx.notify();
                }))
            }));
        let mut table = panel(theme)
            .id("connections-table")
            .test_support()
            .gap_0()
            .flex_1()
            .min_h(gpui_kit::rems(18.))
            .w_full()
            .min_w_0()
            .child(tabs)
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(
                        div().flex_1().min_w(gpui_kit::rems(12.)).child(
                            Input::new(&self.filter_input)
                                .prefix(gpui_kit::component::Icon::new(IconName::Search))
                                .small()
                                .h_10(),
                        ),
                    )
                    .child({
                        let owner = cx.entity().downgrade();
                        let selected = self.worker.policy.clone();
                        let policies = projection.policies.clone();
                        Button::new("connections-policy-filter")
                            .outline()
                            .icon(IconName::ChevronDown)
                            .label(selected.clone().unwrap_or_else(|| {
                                zenclash_i18n::text("unified.rules.all_policies")
                            }))
                            .dropdown_menu(move |mut menu, _, _| {
                                for value in
                                    std::iter::once(None).chain(policies.iter().cloned().map(Some))
                                {
                                    let owner = owner.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(value.clone().unwrap_or_else(|| {
                                            zenclash_i18n::text("unified.rules.all_policies")
                                        }))
                                        .checked(value == selected)
                                        .on_click(
                                            move |_, _, cx| {
                                                let _ = owner.update(cx, |view, cx| {
                                                    view.worker.policy = value.clone();
                                                    view.page_index = 0;
                                                    view.update_projection_immediate(cx);
                                                    cx.notify();
                                                });
                                            },
                                        ),
                                    );
                                }
                                menu
                            })
                    })
                    .child(self.render_connection_options(cx)),
            )
            .when(!compact, |table| {
                table.child(connection_columns(theme).mt_3())
            });
        let mut rows = v_flex().gap_0().when(projection.order.is_empty(), |this| {
            this.child(empty_state(
                zenclash_i18n::text(if data.connections.is_empty() {
                    "connections.empty.active"
                } else {
                    "connections.empty.filtered"
                }),
                theme,
            ))
        });
        for &index in &projection.order[page.start..page.end] {
            let connection = &data.connections[index];
            let id = connection.id.clone();
            let detail_id = id.clone();
            let rule_label = if connection.rule.is_empty() {
                "—".to_owned()
            } else if connection.rule_payload.is_empty() {
                connection.rule.clone()
            } else {
                format!("{} ({})", connection.rule, connection.rule_payload)
            };
            let host = if connection.metadata.host.is_empty() {
                &connection.metadata.destination_ip
            } else {
                &connection.metadata.host
            };
            let active = self.inspected_connection.as_deref() == Some(id.as_str());
            let rates = if self.show_closed {
                Some([0, 0])
            } else {
                self.timeline.rates(&id)
            };
            let duration = if self.show_closed {
                self.timeline.duration(&id).unwrap_or("—").to_owned()
            } else {
                projection.durations[index].clone()
            };
            rows = rows.child(
                h_flex()
                    .flex_wrap()
                    .when(compact, |row| row.gap_2().py_3())
                    .gap_3()
                    .min_h(gpui_kit::rems(2.875))
                    .py_1()
                    .px_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .when(active, |row| {
                        row.rounded(theme.radius)
                            .border_color(theme.table_active)
                            .bg(theme.table_active)
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .when(compact, |identity| identity.flex_none().w_full())
                            .line_height(gpui_kit::relative(1.25))
                            .child(
                                Button::new(connection_element_id("connection-target", &id))
                                    .label(host.clone())
                                    .w_full()
                                    .min_w_0()
                                    .ghost()
                                    .small()
                                    .justify_start()
                                    .icon(if active {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .on_click(cx.listener({
                                        let id = id.clone();
                                        move |this, _, _, cx| {
                                            this.toggle_connection_details(id.clone(), cx)
                                        }
                                    })),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(format!(
                                        "{} · {}",
                                        if connection.metadata.process.is_empty() {
                                            "—"
                                        } else {
                                            &connection.metadata.process
                                        },
                                        connection.metadata.source_ip
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .w_16()
                            .flex_shrink_0()
                            .text_sm()
                            .child(connection.metadata.network.to_uppercase()),
                    )
                    .child(
                        div().w_24().flex_shrink_0().text_sm().truncate().child(
                            connection
                                .chains
                                .iter()
                                .rev()
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(" → "),
                        ),
                    )
                    .child(
                        div()
                            .id(connection_element_id("connection-rule", &id))
                            .role(gpui_kit::Role::Status)
                            .aria_label(rule_label.clone())
                            .test_support()
                            .w_24()
                            .flex_shrink_0()
                            .text_sm()
                            .truncate()
                            .child(rule_label),
                    )
                    .child(div().w_20().flex_shrink_0().text_sm().text_right().child(
                        rates.map_or_else(
                            || "—".into(),
                            |rate| format!("↑ {}", zenclash_core::format_speed(rate[0])),
                        ),
                    ))
                    .child(div().w_20().flex_shrink_0().text_sm().text_right().child(
                        rates.map_or_else(
                            || "—".into(),
                            |rate| format!("↓ {}", zenclash_core::format_speed(rate[1])),
                        ),
                    ))
                    .child(div().w_16().flex_shrink_0().text_sm().child(duration))
                    .child(
                        Button::new(connection_element_id("connection-details", &id))
                            .label("⋯")
                            .accessibility_label(zenclash_i18n::text(
                                "connections.actions.show_details",
                            ))
                            .tooltip(zenclash_i18n::text("connections.actions.show_details"))
                            .w_8()
                            .flex_shrink_0()
                            .ghost()
                            .small()
                            .bg(gpui_kit::transparent_black())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_connection_details(detail_id.clone(), cx)
                            })),
                    ),
            );
            if active && let Some(details) = inline_details.take() {
                rows = rows.child(details);
            }
        }
        table = table.child(
            div()
                .id("connection-table-viewport")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .child(rows)
                .vertical_scrollbar(&self.scroll),
        );
        table = table.child(
            h_flex()
                .flex_wrap()
                .gap_2()
                .justify_between()
                .pt_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(pagination_summary(page, projection.order.len())),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("previous-connections-page")
                                .icon(IconName::ChevronLeft)
                                .small()
                                .outline()
                                .label(zenclash_i18n::text("common.actions.previous_page"))
                                .disabled(page.index == 0)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_connections_page(page.index.saturating_sub(1), cx)
                                })),
                        )
                        .child(
                            Button::new("next-connections-page")
                                .icon(IconName::ChevronRight)
                                .small()
                                .outline()
                                .label(zenclash_i18n::text("common.actions.next_page"))
                                .disabled(page.index + 1 >= page.count)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_connections_page(page.index + 1, cx)
                                })),
                        ),
                ),
        );
        v_flex()
            .h_full()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(zenclash_i18n::text("unified.connections.analytics")),
            )
            .child(self.render_connection_statistics(metrics, projection, theme))
            .child(table)
            .into_any_element()
    }
}

type DistributionSeries = Vec<(String, u64)>;

fn connection_distributions(
    connections: &[zenclash_core::Connection],
) -> (DistributionSeries, DistributionSeries) {
    let mut processes = std::collections::BTreeMap::<&str, u64>::new();
    let mut protocols = std::collections::BTreeMap::<String, u64>::new();
    for connection in connections {
        *processes
            .entry(connection.metadata.process.as_str())
            .or_default() += 1;
        *protocols
            .entry(connection.metadata.network.to_uppercase())
            .or_default() += 1;
    }
    let mut processes = processes
        .into_iter()
        .map(|(name, count)| (name.to_owned(), count))
        .collect::<Vec<_>>();
    processes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    processes.truncate(5);
    (processes, protocols.into_iter().collect())
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

fn connection_columns(theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    h_flex()
        .gap_3()
        .py_2()
        .px_2()
        .bg(theme.table_head)
        .text_sm()
        .text_color(theme.muted_foreground)
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(zenclash_i18n::text("connections.columns.target")),
        )
        .child(
            div()
                .w_16()
                .flex_shrink_0()
                .child(zenclash_i18n::text("connections.columns.protocol")),
        )
        .child(
            div()
                .w_24()
                .flex_shrink_0()
                .child(zenclash_i18n::text("connections.columns.outbound")),
        )
        .child(
            div()
                .w_24()
                .flex_shrink_0()
                .child(zenclash_i18n::text("connections.details.rule")),
        )
        .child(
            div()
                .w_20()
                .flex_shrink_0()
                .text_right()
                .child(zenclash_i18n::text("unified.connections.upload_rate")),
        )
        .child(
            div()
                .w_20()
                .flex_shrink_0()
                .text_right()
                .child(zenclash_i18n::text("unified.connections.download_rate")),
        )
        .child(
            div()
                .w_16()
                .flex_shrink_0()
                .child(zenclash_i18n::text("connections.columns.duration")),
        )
        .child(div().w_8().flex_shrink_0())
}
