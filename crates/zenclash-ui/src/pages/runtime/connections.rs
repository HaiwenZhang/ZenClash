use super::{
    div, empty_state, format_bytes, h_flex, message_banner, metric, px, v_flex, Button,
    ButtonVariants, ConnectionsSnapshot, Context, Disableable, FluentBuilder, Icon, IconName,
    InteractiveElement, IntoElement, Page, ParentElement, RuntimeData, RuntimePage, Sizable,
    Styled,
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
                Err(error) => Err(format!("关闭连接任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
                            this.notice = Some("全部连接已关闭".into());
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
                Err(error) => Err(format!("关闭连接任务异常结束：{error}")),
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
        v_flex()
            .gap_4()
            .when(
                !self.core_kind.capabilities().udp_connection_tracking,
                |this| {
                    this.child(message_banner(
                        format!(
                            "{} 当前不会完整上报 UDP 连接；TCP 连接、累计流量和关闭操作仍来自真实控制器。",
                            self.core_kind.display_name()
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
                    .child(metric("活动连接", total.to_string(), theme.primary, theme))
                    .child(metric(
                        "累计上传",
                        format_bytes(data.upload_total),
                        theme.success,
                        theme,
                    ))
                    .child(metric(
                        "累计下载",
                        format_bytes(data.download_total),
                        theme.primary,
                        theme,
                    ))
                    .child(metric(
                        "内存",
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
                            .child("连接数据每 500ms 从真实控制器刷新"),
                    )
                    .child(
                        Button::new("close-all-connections")
                            .icon(IconName::CircleX)
                            .label("关闭全部")
                            .danger()
                            .small()
                            .disabled(
                                total == 0 || self.mutating || !self.closing_connections.is_empty(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.close_all_connections(cx))),
                    ),
            )
            .child(
                v_flex()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .when(total == 0, |this| {
                        this.child(empty_state("当前没有活动连接", theme))
                    })
                    .children(
                        data.connections
                            .iter()
                            .enumerate()
                            .map(|(index, connection)| {
                                let id = connection.id.clone();
                                let closing = self.closing_connections.contains(&id);
                                let host = if connection.metadata.host.is_empty() {
                                    connection.metadata.destination_ip.clone()
                                } else {
                                    connection.metadata.host.clone()
                                };
                                let chain = connection.chains.join(" → ");
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
                                                    .child(format!(
                                                        "{} · {} · {}",
                                                        connection.metadata.network,
                                                        connection.rule,
                                                        chain
                                                    )),
                                            ),
                                    )
                                    .child(
                                        v_flex()
                                            .items_end()
                                            .text_xs()
                                            .child(format!("↑ {}", format_bytes(connection.upload)))
                                            .child(format!(
                                                "↓ {}",
                                                format_bytes(connection.download)
                                            )),
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
                            }),
                    ),
            )
            .into_any_element()
    }
}
