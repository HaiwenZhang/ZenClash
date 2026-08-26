use super::{
    div, empty_state, h_flex, info_row, load_page, message_banner, profiles, px, setting_card,
    v_flex, Button, Context, Disableable, FluentBuilder, IconName, InteractiveElement, IntoElement,
    Page, ParentElement, ProfileActivated, RemoteProfileOptions, RuntimeData, RuntimePage, Sizable,
    Styled, SubStoreClient, SubStoreItem, SubStoreItemKind, SubStoreSnapshot,
};

impl RuntimePage {
    fn import_substore_profile(
        &mut self,
        kind: SubStoreItemKind,
        item: SubStoreItem,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.profile_store.clone() else {
            self.error = Some("配置仓库不可用".into());
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(Page::SubStore) else {
            return;
        };
        let client = self.client.clone();
        let controlled = self.controlled_config_store.clone();
        let core_runtime = profiles::workflow::CoreProfileRuntime::new(
            self.core_kind,
            client.clone(),
            self.process.clone(),
        );
        let task = self.runtime.spawn(async move {
            let substore = SubStoreClient::from_env().map_err(|error| error.to_string())?;
            let url = substore
                .profile_url(kind, &item.name)
                .map_err(|error| error.to_string())?;
            let name = if item.display_name.trim().is_empty() {
                item.name
            } else {
                item.display_name
            };
            let outcome = profiles::workflow::add_remote(
                store,
                controlled,
                core_runtime,
                name,
                url,
                "clash.meta".into(),
                RemoteProfileOptions::default(),
            )
            .await?;
            let data = load_page(client, Page::SubStore).await?;
            Ok::<_, String>((outcome, data))
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("Sub-Store 导入任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok((outcome, data)) => {
                        this.profile_path = Some(outcome.path.clone());
                        this.invalidate_config_inputs();
                        this.config_preview = None;
                        if let Err(error) = this.reload_profile_catalog() {
                            this.set_page_error(token, error);
                        }
                        cx.emit(ProfileActivated { path: outcome.path });
                        if this.replace_page_data(token, data) {
                            this.notice = Some(format!(
                                "Sub-Store 配置“{}”已生成、导入并由 {} 启用",
                                outcome.name,
                                this.core_kind.display_name()
                            ));
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

    pub(super) fn render_substore(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let snapshot = match &self.data {
            RuntimeData::SubStore(snapshot) => snapshot.clone(),
            _ => SubStoreSnapshot::default(),
        };
        let frontend_url = snapshot.frontend_url.clone();
        v_flex()
            .gap_4()
            .child(
                setting_card("Sub-Store 服务", theme)
                    .child(info_row(
                        "后端服务",
                        if snapshot.connected {
                            "已连接"
                        } else {
                            "等待连接"
                        },
                        theme,
                    ))
                    .child(info_row("后端地址", &snapshot.backend_url, theme))
                    .child(info_row("前端地址", &snapshot.frontend_url, theme))
                    .child(
                        h_flex().justify_end().p_3().child(
                            Button::new("open-substore")
                                .icon(IconName::ExternalLink)
                                .label("在浏览器中打开")
                                .disabled(frontend_url.is_empty())
                                .on_click(move |_, _, cx| cx.open_url(&frontend_url)),
                        ),
                    ),
            )
            .when_some(snapshot.error, |this, error| {
                this.child(message_banner(
                    format!(
                        "未连接 Sub-Store：{error}。可通过 ZENCLASH_SUBSTORE_URL 和 ZENCLASH_SUBSTORE_FRONTEND_URL 接入现有服务。"
                    ),
                    theme.warning,
                    theme,
                ))
            })
            .child(substore_items(
                "订阅",
                snapshot.subscriptions,
                SubStoreItemKind::Subscription,
                theme.primary,
                theme,
                self.mutating,
                cx,
            ))
            .child(substore_items(
                "集合",
                snapshot.collections,
                SubStoreItemKind::Collection,
                theme.success,
                theme,
                self.mutating,
                cx,
            ))
            .into_any_element()
    }
}

fn substore_items(
    title: &'static str,
    items: Vec<SubStoreItem>,
    kind: SubStoreItemKind,
    accent: gpui::Hsla,
    theme: &gpui_component::Theme,
    mutating: bool,
    cx: &mut Context<RuntimePage>,
) -> gpui::AnyElement {
    let count = items.len();
    let id_prefix = if title == "订阅" {
        "substore-subscription"
    } else {
        "substore-collection"
    };
    let import_id_prefix = if title == "订阅" {
        "import-substore-subscription"
    } else {
        "import-substore-collection"
    };
    setting_card(title, theme)
        .when(count == 0, |this| {
            this.child(empty_state("没有可显示的项目", theme))
        })
        .children(items.into_iter().enumerate().map(|(index, item)| {
            let label = if item.display_name.is_empty() {
                item.name.clone()
            } else {
                item.display_name.clone()
            };
            h_flex()
                .id((id_prefix, index))
                .min_h(px(48.))
                .px_4()
                .gap_3()
                .border_b_1()
                .border_color(theme.border)
                .child(div().size_2().rounded_full().bg(accent))
                .child(div().flex_1().text_sm().child(label))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(item.tag.join(" · ")),
                )
                .child(
                    Button::new((import_id_prefix, index))
                        .icon(IconName::ArrowDown)
                        .label("导入并启用")
                        .small()
                        .disabled(mutating)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.import_substore_profile(kind, item.clone(), cx);
                        })),
                )
        }))
        .into_any_element()
}
