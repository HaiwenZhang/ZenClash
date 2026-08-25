use super::{
    div, empty_dash, empty_state, h_flex, load_page, px, v_flex, Button, Context, Disableable,
    FluentBuilder, Icon, IconName, InteractiveElement, IntoElement, Page, ParentElement,
    ProviderCatalog, RuntimeData, RuntimePage, Sizable, Styled,
};

impl RuntimePage {
    fn update_provider(&mut self, name: String, is_rule: bool, cx: &mut Context<Self>) {
        let Some(token) = self.begin_mutation(Page::Resources) else {
            return;
        };
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            if is_rule {
                client.update_rule_provider(&name).await
            } else {
                client.update_proxy_provider(&name).await
            }
            .map_err(|error| error.to_string())?;
            load_page(client, Page::Resources).await
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("更新外部资源任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
                            this.notice = Some("外部资源已更新".into());
                        }
                    }
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_resources(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (proxy, rules) = match &self.data {
            RuntimeData::Resources { proxy, rules } => (proxy.clone(), rules.clone()),
            _ => (ProviderCatalog::default(), ProviderCatalog::default()),
        };
        v_flex()
            .gap_4()
            .child(provider_section(
                "代理提供者",
                proxy,
                false,
                self.mutating,
                theme,
                cx,
            ))
            .child(provider_section(
                "规则提供者",
                rules,
                true,
                self.mutating,
                theme,
                cx,
            ))
            .into_any_element()
    }
}

fn provider_section(
    title: &'static str,
    catalog: ProviderCatalog,
    is_rule: bool,
    mutating: bool,
    theme: &gpui_component::Theme,
    cx: &mut Context<RuntimePage>,
) -> gpui::AnyElement {
    let count = catalog.providers.len();
    v_flex()
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{count} 项")),
                ),
        )
        .child(
            v_flex()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .when(count == 0, |this| {
                    this.child(empty_state("没有提供者", theme))
                })
                .children(catalog.providers.into_iter().enumerate().map(
                    |(index, (key, provider))| {
                        let name = if provider.name.is_empty() {
                            key
                        } else {
                            provider.name
                        };
                        let name_for_click = name.clone();
                        let item_count = if is_rule {
                            provider.rule_count
                        } else {
                            provider.proxies.len()
                        };
                        h_flex()
                            .id((
                                if is_rule {
                                    "rule-provider"
                                } else {
                                    "proxy-provider"
                                },
                                index,
                            ))
                            .min_h(px(58.))
                            .px_4()
                            .gap_3()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(Icon::new(IconName::Inbox).size_4())
                            .child(
                                v_flex().flex_1().child(div().text_sm().child(name)).child(
                                    div().text_xs().text_color(theme.muted_foreground).child(
                                        format!(
                                            "{} · {} · {} 项",
                                            provider.vehicle_type,
                                            empty_dash(&provider.updated_at),
                                            item_count
                                        ),
                                    ),
                                ),
                            )
                            .child(
                                Button::new(("update-provider", index))
                                    .icon(IconName::Redo2)
                                    .label("更新")
                                    .small()
                                    .disabled(mutating)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.update_provider(name_for_click.clone(), is_rule, cx);
                                    })),
                            )
                    },
                )),
        )
        .into_any_element()
}
