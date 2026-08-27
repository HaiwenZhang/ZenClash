use super::{
    Button, ButtonVariants, ConnectionsSnapshot, Context, Disableable, FluentBuilder, Icon,
    IconName, Input, InteractiveElement, IntoElement, Page, ParentElement, RuntimeData,
    RuntimePage, Sizable, Styled, div, empty_state, format_bytes, h_flex, message_banner, metric,
    px, v_flex,
};

impl RuntimePage {
    fn close_all_connections(&mut self, cx: &mut Context<Self>) {
        if !self.closing_connections.is_empty() {
            return;
        }
        let Some(token) = self.begin_mutation(Page::Connections) else {
            return;
        };
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            client
                .close_all_connections()
                .await
                .map_err(|error| error.to_string())?;
            client
                .connections_snapshot()
                .await
                .map(RuntimeData::Connections)
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(zenclash_i18n::text_with(
                    "connections.errors.close_task",
                    &[("error", error.to_string())],
                )),
            };
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
                            this.notice =
                                Some(zenclash_i18n::text("connections.notices.closed_all"));
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

    fn close_connection(&mut self, id: String, cx: &mut Context<Self>) {
        if self.mutating || !self.closing_connections.insert(id.clone()) {
            return;
        }
        self.invalidate_page_load();
        self.error = None;
        let token = self.page_task_token_for(Page::Connections);
        let client = self.client.clone();
        let id_for_task = id.clone();
        let task = self.runtime.spawn(async move {
            client
                .close_connection(&id_for_task)
                .await
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(zenclash_i18n::text_with(
                    "connections.errors.close_task",
                    &[("error", error.to_string())],
                )),
            };
            let _ = this.update(cx, |this, cx| {
                this.closing_connections.remove(&id);
                match result {
                    Ok(()) if this.is_page_task_current(token) => this.refresh(cx),
                    Ok(()) => {}
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the connection page is a cohesive declarative GPUI element tree"
    )]
    pub(super) fn render_connections(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let data = match &self.data {
            RuntimeData::Connections(data) => data.clone(),
            _ => ConnectionsSnapshot::default(),
        };
        let total = data.connections.len();
        let query = normalize_connection_query(&self.connection_filter.read(cx).value());
        let filtered = data
            .connections
            .iter()
            .filter(|connection| connection_matches(connection, &query))
            .collect::<Vec<_>>();
        let visible = filtered.len();
        v_flex()
            .gap_4()
            .when(
                !self.core_kind.capabilities().udp_connection_tracking,
                |this| {
                    this.child(message_banner(
                        zenclash_i18n::text_with(
                            "connections.warnings.udp_tracking",
                            &[("core", self.core_kind.display_name().to_owned())],
                        ),
                        theme.warning,
                        theme,
                    ))
                },
            )
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(metric(
                        zenclash_i18n::text("connections.metrics.active"),
                        total.to_string(),
                        theme.primary,
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("connections.metrics.upload"),
                        format_bytes(data.upload_total),
                        theme.success,
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("connections.metrics.download"),
                        format_bytes(data.download_total),
                        theme.primary,
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("connections.metrics.memory"),
                        format_bytes(data.memory),
                        theme.warning,
                        theme,
                    )),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("connections.refresh_hint")),
                    )
                    .child(
                        Button::new("close-all-connections")
                            .icon(IconName::CircleX)
                            .label(zenclash_i18n::text("connections.actions.close_all"))
                            .danger()
                            .small()
                            .disabled(
                                total == 0 || self.mutating || !self.closing_connections.is_empty(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.close_all_connections(cx))),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.connection_filter).small()),
                    )
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        if query.is_empty() {
                            zenclash_i18n::text_with(
                                "connections.count.active",
                                &[("total", total.to_string())],
                            )
                        } else {
                            zenclash_i18n::text_with(
                                "common.count.visible_total",
                                &[
                                    ("visible", visible.to_string()),
                                    ("total", total.to_string()),
                                ],
                            )
                        },
                    )),
            )
            .child(
                v_flex()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .when(filtered.is_empty(), |this| {
                        this.child(empty_state(
                            if total == 0 {
                                zenclash_i18n::text("connections.empty.active")
                            } else {
                                zenclash_i18n::text("connections.empty.filtered")
                            },
                            theme,
                        ))
                    })
                    .children(filtered.into_iter().enumerate().map(|(index, connection)| {
                        let id = connection.id.clone();
                        let closing = self.closing_connections.contains(&id);
                        let host = if connection.metadata.host.is_empty() {
                            connection.metadata.destination_ip.clone()
                        } else {
                            connection.metadata.host.clone()
                        };
                        let summary = connection_summary(connection);
                        h_flex()
                            .id(("connection-row", index))
                            .min_h(px(58.))
                            .px_4()
                            .gap_3()
                            .items_center()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(Icon::new(IconName::ExternalLink).size_4())
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().child(format!(
                                        "{}:{}",
                                        host, connection.metadata.destination_port
                                    )))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(summary),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .items_end()
                                    .text_xs()
                                    .child(format!("↑ {}", format_bytes(connection.upload)))
                                    .child(format!("↓ {}", format_bytes(connection.download))),
                            )
                            .child(
                                Button::new(("close-connection", index))
                                    .icon(IconName::CircleX)
                                    .ghost()
                                    .small()
                                    .disabled(self.mutating || closing)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.close_connection(id.clone(), cx);
                                    })),
                            )
                    })),
            )
            .into_any_element()
    }
}

fn normalize_connection_query(query: &str) -> String {
    query.trim().to_ascii_lowercase()
}

fn connection_matches(connection: &zenclash_core::Connection, query: &str) -> bool {
    query.is_empty()
        || connection
            .metadata
            .host
            .to_ascii_lowercase()
            .contains(query)
        || connection
            .metadata
            .destination_ip
            .to_ascii_lowercase()
            .contains(query)
        || connection
            .metadata
            .source_ip
            .to_ascii_lowercase()
            .contains(query)
        || connection
            .metadata
            .process
            .to_ascii_lowercase()
            .contains(query)
        || connection.rule.to_ascii_lowercase().contains(query)
        || connection.rule_payload.to_ascii_lowercase().contains(query)
        || connection
            .chains
            .iter()
            .any(|chain| chain.to_ascii_lowercase().contains(query))
}

fn connection_summary(connection: &zenclash_core::Connection) -> String {
    let mut parts = Vec::with_capacity(4);
    if !connection.metadata.network.is_empty() {
        parts.push(connection.metadata.network.clone());
    }
    if !connection.metadata.process.is_empty() {
        parts.push(connection.metadata.process.clone());
    }
    if !connection.rule.is_empty() {
        parts.push(connection.rule.clone());
    }
    if !connection.chains.is_empty() {
        parts.push(connection.chains.join(" → "));
    }
    if parts.is_empty() {
        zenclash_i18n::text("connections.empty.details")
    } else {
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_matches_connection_identity_and_route_fields() {
        let connection = zenclash_core::Connection {
            metadata: zenclash_core::ConnectionMetadata {
                host: "Example.COM".into(),
                destination_ip: "203.0.113.8".into(),
                process: "Browser".into(),
                ..Default::default()
            },
            rule: "DomainSuffix".into(),
            chains: vec!["Hong Kong".into()],
            ..Default::default()
        };

        for query in ["example", "203.0.113", "browser", "domainsuffix", "hong"] {
            assert!(connection_matches(
                &connection,
                &normalize_connection_query(query)
            ));
        }
        assert!(!connection_matches(
            &connection,
            &normalize_connection_query("direct")
        ));
        assert_eq!(
            connection_summary(&connection),
            "Browser · DomainSuffix · Hong Kong"
        );
    }
}
