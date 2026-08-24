use std::collections::HashSet;

use gpui::{
    div, prelude::FluentBuilder, px, App, Context, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::{h_flex, scroll::ScrollableElement, v_flex, ActiveTheme, Icon, IconName};
use zenclash_core::{DelayHistory, MihomoClient, ProxyCatalog, ProxyGroup, ProxyNode};

pub struct ProxiesPage {
    client: MihomoClient,
    runtime: tokio::runtime::Handle,
    catalog: Option<ProxyCatalog>,
    expanded: HashSet<String>,
    testing: HashSet<String>,
    switching: Option<(String, String)>,
    loading: bool,
    error: Option<String>,
    focus_handle: gpui::FocusHandle,
}

impl ProxiesPage {
    pub fn new(
        client: MihomoClient,
        runtime: tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut page = Self {
            client,
            runtime,
            catalog: None,
            expanded: HashSet::new(),
            testing: HashSet::new(),
            switching: None,
            loading: false,
            error: None,
            focus_handle: cx.focus_handle(),
        };
        page.refresh(cx);
        page
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.error = None;
        cx.notify();

        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            client
                .proxy_catalog()
                .await
                .map_err(|error| error.to_string())
        });

        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("代理数据任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(catalog) => {
                        if this.expanded.is_empty() {
                            if let Some(group) = catalog.groups.first() {
                                this.expanded.insert(group.name.clone());
                            }
                        }
                        this.catalog = Some(catalog);
                        this.error = None;
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_group(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(name) {
            self.expanded.insert(name.to_owned());
        }
        cx.notify();
    }

    fn change_proxy(&mut self, group: String, proxy: String, cx: &mut Context<Self>) {
        if self.switching.is_some() {
            return;
        }
        self.switching = Some((group.clone(), proxy.clone()));
        self.error = None;
        cx.notify();

        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            client.change_proxy(&group, &proxy).await?;
            client.close_all_connections().await?;
            client.proxy_catalog().await
        });

        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result.map_err(|error| error.to_string()),
                Err(error) => Err(format!("切换代理任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                this.switching = None;
                match result {
                    Ok(catalog) => {
                        this.catalog = Some(catalog);
                        this.error = None;
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn test_proxy(
        &mut self,
        group: String,
        proxy: String,
        test_url: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let test_key = test_key(&group, &proxy);
        if !self.testing.insert(test_key.clone()) {
            return;
        }
        cx.notify();

        let client = self.client.clone();
        let proxy_for_task = proxy.clone();
        let task = self.runtime.spawn(async move {
            client
                .proxy_delay(&proxy_for_task, test_url.as_deref(), 5_000)
                .await
                .map_err(|error| error.to_string())
        });

        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("延迟测试任务异常结束：{error}")),
            };
            let _ = this.update(cx, |this, cx| {
                this.testing.remove(&test_key);
                match result {
                    Ok(result) => {
                        this.record_delay(&group, &proxy, result.delay, result.mean_delay);
                    }
                    Err(error) => {
                        this.record_delay(&group, &proxy, 0, 0);
                        this.error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn test_group(&mut self, group: ProxyGroup, cx: &mut Context<Self>) {
        let pending = group
            .all
            .iter()
            .map(|proxy| test_key(&group.name, &proxy.name))
            .filter(|key| self.testing.insert(key.clone()))
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return;
        }
        cx.notify();

        let client = self.client.clone();
        let group_name = group.name.clone();
        let test_url = group.test_url.clone();
        let task = self.runtime.spawn(async move {
            let mut set = tokio::task::JoinSet::new();
            for proxy in group.all {
                let client = client.clone();
                let test_url = test_url.clone();
                set.spawn(async move {
                    let result = client
                        .proxy_delay(&proxy.name, test_url.as_deref(), 5_000)
                        .await;
                    (proxy.name, result)
                });
            }

            let mut results = Vec::new();
            while let Some(result) = set.join_next().await {
                if let Ok(result) = result {
                    results.push(result);
                }
            }
            results
        });

        cx.spawn(async move |this, cx| {
            let results = task.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                for key in pending {
                    this.testing.remove(&key);
                }
                let mut failed = 0usize;
                for (proxy, result) in results {
                    match result {
                        Ok(result) => {
                            this.record_delay(&group_name, &proxy, result.delay, result.mean_delay)
                        }
                        Err(_) => {
                            failed += 1;
                            this.record_delay(&group_name, &proxy, 0, 0);
                        }
                    }
                }
                if failed > 0 {
                    this.error = Some(format!("{failed} 个节点延迟测试失败"));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn record_delay(&mut self, group: &str, proxy: &str, delay: u32, mean_delay: u32) {
        let Some(catalog) = self.catalog.as_mut() else {
            return;
        };
        let Some(group) = catalog.groups.iter_mut().find(|item| item.name == group) else {
            return;
        };
        let Some(proxy) = group.all.iter_mut().find(|item| item.name == proxy) else {
            return;
        };
        proxy.history.push(DelayHistory {
            time: String::new(),
            delay,
            mean_delay,
        });
        proxy.alive = Some(delay > 0);
    }

    fn render_header(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let loading = self.loading;
        h_flex()
            .h(px(49.))
            .px_5()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("代理组"),
            )
            .child(
                div()
                    .id("refresh-proxies")
                    .px_3()
                    .py_1()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .text_xs()
                    .text_color(if loading {
                        theme.muted_foreground
                    } else {
                        theme.foreground
                    })
                    .hover(|this| this.bg(theme.muted))
                    .child(if loading { "加载中…" } else { "刷新" })
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
    }

    fn render_group(
        &self,
        group_index: usize,
        group: &ProxyGroup,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let expanded = self.expanded.contains(&group.name);
        let group_name = group.name.clone();
        let group_for_test = group.clone();
        let testing_group = group
            .all
            .iter()
            .any(|proxy| self.testing.contains(&test_key(&group.name, &proxy.name)));

        v_flex()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary.opacity(0.68))
            .child(
                h_flex()
                    .id(("proxy-group", group_index))
                    .min_h(px(54.))
                    .px_4()
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.muted.opacity(0.55)))
                    .child(
                        h_flex()
                            .items_center()
                            .gap_3()
                            .child(Icon::new(if expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            }))
                            .child(
                                v_flex()
                                    .gap_0()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(gpui::FontWeight::BOLD)
                                                    .child(group.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(group.kind.clone()),
                                            ),
                                    )
                                    .child(
                                        div().text_xs().text_color(theme.muted_foreground).child(
                                            format!(
                                                "当前：{} · {} 个节点",
                                                group.now,
                                                group.all.len()
                                            ),
                                        ),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id(("test-group", group_index))
                            .px_3()
                            .py_1()
                            .rounded(theme.radius)
                            .bg(theme.background)
                            .border_1()
                            .border_color(theme.border)
                            .text_xs()
                            .child(if testing_group {
                                "测速中…"
                            } else {
                                "全部测速"
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.test_group(group_for_test.clone(), cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_group(&group_name, cx);
                    })),
            )
            .when(expanded, |this| {
                this.child(
                    h_flex()
                        .p_3()
                        .gap_2()
                        .flex_wrap()
                        .border_t_1()
                        .border_color(theme.border)
                        .children(group.all.iter().enumerate().map(|(proxy_index, proxy)| {
                            self.render_proxy(group_index, proxy_index, group, proxy, theme, cx)
                        })),
                )
            })
    }

    fn render_proxy(
        &self,
        group_index: usize,
        proxy_index: usize,
        group: &ProxyGroup,
        proxy: &ProxyNode,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let selected = group.now == proxy.name;
        let testing = self.testing.contains(&test_key(&group.name, &proxy.name));
        let switching = self.switching.as_ref() == Some(&(group.name.clone(), proxy.name.clone()));
        let group_name = group.name.clone();
        let proxy_name = proxy.name.clone();
        let delay_group = group.name.clone();
        let delay_proxy = proxy.name.clone();
        let test_url = group.test_url.clone();
        let delay = proxy.latest_delay();
        let delay_color = match delay {
            Some(0) => theme.danger,
            Some(value) if value < 500 => theme.success,
            Some(_) => theme.warning,
            None => theme.muted_foreground,
        };
        let delay_text = if testing {
            "测速中…".to_owned()
        } else {
            match delay {
                Some(0) => "超时".to_owned(),
                Some(value) => format!("{value} ms"),
                None => "测速".to_owned(),
            }
        };
        let capabilities = proxy.capabilities().collect::<Vec<_>>().join(" · ");

        v_flex()
            .id((
                gpui::ElementId::from(("proxy", group_index)),
                proxy_index.to_string(),
            ))
            .w(px(220.))
            .min_h(px(78.))
            .gap_2()
            .p_3()
            .rounded(theme.radius)
            .border_1()
            .border_color(if selected {
                theme.primary
            } else {
                theme.border
            })
            .bg(if selected {
                theme.primary.opacity(0.2)
            } else {
                theme.background
            })
            .cursor_pointer()
            .hover(|this| this.bg(theme.muted))
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_weight(if selected {
                                gpui::FontWeight::BOLD
                            } else {
                                gpui::FontWeight::NORMAL
                            })
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(if switching {
                                format!("{}（切换中…）", proxy.name)
                            } else {
                                proxy.name.clone()
                            }),
                    )
                    .child(
                        div()
                            .id((
                                gpui::ElementId::from(("proxy-delay", group_index)),
                                proxy_index.to_string(),
                            ))
                            .px_2()
                            .py_1()
                            .rounded(theme.radius)
                            .text_xs()
                            .text_color(delay_color)
                            .hover(|this| this.bg(theme.muted))
                            .child(delay_text)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.test_proxy(
                                    delay_group.clone(),
                                    delay_proxy.clone(),
                                    test_url.clone(),
                                    cx,
                                );
                            })),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(proxy.kind.clone())
                    .child(if capabilities.is_empty() {
                        "—".to_owned()
                    } else {
                        capabilities
                    }),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.change_proxy(group_name.clone(), proxy_name.clone(), cx);
            }))
            .into_any_element()
    }
}

impl Focusable for ProxiesPage {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ProxiesPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let catalog = self.catalog.clone();
        let error = self.error.clone();

        v_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .child(self.render_header(&theme, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .gap_4()
                    .p_5()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_2xl()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("代理组"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child("选择策略组节点、查看协议能力并执行延迟测试。"),
                            ),
                    )
                    .when_some(error, |this, error| {
                        this.child(
                            h_flex()
                                .gap_2()
                                .p_3()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.danger.opacity(0.6))
                                .bg(theme.danger.opacity(0.12))
                                .text_sm()
                                .text_color(theme.danger)
                                .child(Icon::new(IconName::CircleX).size_4())
                                .child(error),
                        )
                    })
                    .when(self.loading && catalog.is_none(), |this| {
                        this.child(
                            div()
                                .p_4()
                                .rounded(theme.radius)
                                .bg(theme.secondary)
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("正在读取 Mihomo 代理组…"),
                        )
                    })
                    .when_some(catalog, |this, catalog| {
                        if catalog.groups.is_empty() {
                            this.child(
                                div()
                                    .p_4()
                                    .rounded(theme.radius)
                                    .bg(theme.secondary)
                                    .text_sm()
                                    .child("当前配置没有可用的代理组。"),
                            )
                        } else {
                            this.children(
                                catalog.groups.iter().enumerate().map(|(index, group)| {
                                    self.render_group(index, group, &theme, cx)
                                }),
                            )
                        }
                    }),
            )
    }
}

fn test_key(group: &str, proxy: &str) -> String {
    format!("{group}\0{proxy}")
}
