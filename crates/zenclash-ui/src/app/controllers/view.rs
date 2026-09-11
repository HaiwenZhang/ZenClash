use super::ControllersPage;
use crate::{components::sidebar::OutboundMode, pages::Page};
use gpui::{
    Context, ElementId, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window, div,
};
use gpui_component::{
    ActiveTheme, Disableable, Selectable, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    input::Input,
    menu::{DropdownMenu, PopupMenuItem},
    v_flex,
};

impl Render for ControllersPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut targets = v_flex()
            .gap_2()
            .p_3()
            .child(
                Button::new("target-local")
                    .small()
                    .outline()
                    .disabled(self.busy)
                    .label(zenclash_i18n::text("controllers.local"))
                    .on_click(cx.listener(|this, _, _, cx| this.local(cx))),
            )
            .child(
                Button::new("target-add")
                    .small()
                    .ghost()
                    .disabled(self.busy)
                    .label(zenclash_i18n::text("controllers.add"))
                    .on_click(cx.listener(|this, _, window, cx| this.edit(None, window, cx))),
            );
        for entry in &self.catalog.entries {
            let id = entry.id.clone();
            let edit = entry.clone();
            let test = id.clone();
            let remove = id.clone();
            targets = targets.child(
                v_flex()
                    .gap_1()
                    .child(
                        Button::new((ElementId::from("target"), id.clone()))
                            .small()
                            .outline()
                            .label(entry.name.clone())
                            .selected(
                                self.remote
                                    .as_ref()
                                    .is_some_and(|remote| remote.entry.id == entry.id),
                            )
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.connect(&id, window, cx)
                            })),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new((ElementId::from("target-edit"), entry.id.clone()))
                                    .small()
                                    .ghost()
                                    .label(zenclash_i18n::text("controllers.edit"))
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.edit(Some(edit.clone()), window, cx)
                                    })),
                            )
                            .child(
                                Button::new((ElementId::from("target-test"), entry.id.clone()))
                                    .small()
                                    .ghost()
                                    .label(zenclash_i18n::text("controllers.test"))
                                    .disabled(self.busy)
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| this.test(&test, cx)),
                                    ),
                            )
                            .child(
                                Button::new((ElementId::from("target-remove"), entry.id.clone()))
                                    .small()
                                    .ghost()
                                    .label(zenclash_i18n::text("common.actions.delete"))
                                    .disabled(self.busy)
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| this.remove(&remove, cx)),
                                    ),
                            ),
                    ),
            );
        }
        let mut body = v_flex().size_full().min_w_0();
        if let Some(error) = &self.error {
            body = body.child(
                div()
                    .px_4()
                    .py_2()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        if let Some(notice) = &self.notice {
            body = body.child(div().px_4().py_2().child(notice.clone()));
        }
        if self.busy {
            body = body.child(
                div()
                    .px_4()
                    .py_2()
                    .child(zenclash_i18n::text("common.actions.loading")),
            );
        }
        if self.manager || self.remote.is_none() {
            body = body.child(
                div()
                    .id("controller-settings-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(self.render_editor(cx)),
            );
        } else if let Some(remote) = &self.remote {
            let traffic = remote.traffic.snapshot();
            body = body.child(
                h_flex()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .child(remote.entry.name.clone())
                    .child(format!(
                        "↑ {}  ↓ {}",
                        zenclash_core::format_speed(traffic.upload),
                        zenclash_core::format_speed(traffic.download)
                    ))
                    .child(if traffic.connected {
                        zenclash_i18n::text("runtime.stream.connected")
                    } else {
                        zenclash_i18n::text("runtime.stream.reconnecting")
                    }),
            );
            let selected_mode = remote.mode.displayed();
            let mut tabs = h_flex().gap_2().px_4().py_2().flex_wrap();
            for mode in [
                OutboundMode::Rule,
                OutboundMode::Global,
                OutboundMode::Direct,
            ] {
                tabs = tabs.child(
                    Button::new(mode.api_value())
                        .small()
                        .outline()
                        .selected(selected_mode == mode)
                        .disabled(self.busy)
                        .label(mode.label())
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.set_remote_mode(mode, cx)),
                        ),
                );
            }
            body = body.child(tabs);
            let mut tabs = h_flex().gap_2().px_4().py_2().flex_wrap();
            for page in [Page::Proxies, Page::Connections, Page::Logs, Page::Rules] {
                tabs = tabs.child(
                    Button::new(page.route())
                        .small()
                        .ghost()
                        .selected(remote.page == page)
                        .label(page.label())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(remote) = &mut this.remote {
                                remote.navigate(page, cx);
                            }
                        })),
                );
            }
            body = body.child(tabs).child(div().flex_1().min_h_0().child(
                if remote.page == Page::Proxies {
                    remote.proxies.clone().into_any_element()
                } else {
                    remote.pages.clone().into_any_element()
                },
            ));
        }
        h_flex()
            .size_full()
            .min_h_0()
            .text_sm()
            .child(
                div()
                    .id("controller-targets-scroll")
                    .w_64()
                    .h_full()
                    .flex_shrink_0()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .overflow_y_scroll()
                    .child(targets),
            )
            .child(div().flex_1().min_w_0().h_full().child(body))
    }
}

impl ControllersPage {
    fn render_editor(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut form = v_flex()
            .gap_3()
            .p_4()
            .child(
                div()
                    .text_lg()
                    .child(zenclash_i18n::text("controllers.title")),
            )
            .child(zenclash_i18n::text("controllers.scope"))
            .child(zenclash_i18n::text("controllers.name"))
            .child(Input::new(&self.name).disabled(self.busy))
            .child(zenclash_i18n::text("controllers.address"))
            .child(Input::new(&self.address).disabled(self.busy))
            .child(zenclash_i18n::text("controllers.secret"))
            .child(Input::new(&self.secret).disabled(self.busy))
            .child(
                Button::new("save-controller")
                    .label(zenclash_i18n::text("controllers.save"))
                    .disabled(self.busy || self.store.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.save_entry(cx))),
            );
        if self.remote.is_some() {
            form = form.child(
                Button::new("back-to-remote")
                    .label(zenclash_i18n::text("controllers.back"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.manager = false;
                        if let Some(remote) = &mut this.remote {
                            remote.navigate(remote.page, cx);
                        }
                        cx.notify();
                    })),
            );
        }
        form = form
            .child(
                div()
                    .pt_4()
                    .text_lg()
                    .child(zenclash_i18n::text("ssid.title")),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(zenclash_i18n::text("ssid.scope")),
            )
            .child(self.render_wifi(cx));
        form
    }

    fn render_wifi(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut form = v_flex()
            .gap_3()
            .child(zenclash_i18n::text(self.wifi.status_key()));
        let enabled = self.ssid_rules.as_ref().is_some_and(|rules| rules.enabled);
        form = form.child(
            Button::new("ssid-enable")
                .small()
                .outline()
                .selected(enabled)
                .disabled(!cfg!(target_os = "macos") || self.busy || self.ssid_rules.is_none())
                .label(zenclash_i18n::text(if enabled {
                    "ssid.disable"
                } else {
                    "ssid.enable"
                }))
                .on_click(cx.listener(move |this, _, _, cx| this.set_wifi_enabled(!enabled, cx))),
        );
        if enabled {
            form = form.child(
                Button::new("ssid-permission")
                    .small()
                    .outline()
                    .label(zenclash_i18n::text("ssid.permission_action"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.wifi.request_access(cx);
                        cx.notify();
                    })),
            );
        }
        if !cfg!(target_os = "macos") {
            return form;
        }
        let owner = cx.entity().downgrade();
        let profiles = self.profiles.profiles.clone();
        let selected = self.ssid_profile.clone();
        let label = profiles
            .iter()
            .find(|profile| Some(&profile.id) == selected.as_ref())
            .map_or_else(
                || zenclash_i18n::text("ssid.choose_profile"),
                |profile| profile.name.clone(),
            );
        form = form
            .child(zenclash_i18n::text("ssid.name"))
            .child(Input::new(&self.ssid).disabled(self.busy))
            .child(
                Button::new("ssid-profile")
                    .label(label)
                    .dropdown_caret(true)
                    .dropdown_menu(move |mut menu, _, _| {
                        for profile in &profiles {
                            let owner = owner.clone();
                            let id = profile.id.clone();
                            menu = menu.item(
                                PopupMenuItem::new(profile.name.clone())
                                    .checked(selected.as_ref() == Some(&id))
                                    .on_click(move |_, _, cx| {
                                        let _ = owner.update(cx, |this, cx| {
                                            this.ssid_profile = Some(id.clone());
                                            cx.notify();
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            )
            .child(
                Button::new("ssid-save")
                    .label(zenclash_i18n::text("ssid.save"))
                    .disabled(self.busy || self.ssid_rules.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.save_wifi_rule(cx))),
            );
        if let Some(rules) = &self.ssid_rules {
            for (ssid, profile_id) in &rules.profiles {
                let profile = self
                    .profiles
                    .profiles
                    .iter()
                    .find(|profile| &profile.id == profile_id)
                    .map_or_else(
                        || zenclash_i18n::text("ssid.missing_profile"),
                        |profile| profile.name.clone(),
                    );
                let name = ssid.clone();
                form = form.child(
                    h_flex().gap_2().child(format!("{ssid} → {profile}")).child(
                        Button::new((ElementId::from("remove-ssid"), ssid.clone()))
                            .small()
                            .ghost()
                            .disabled(self.busy)
                            .label(zenclash_i18n::text("common.actions.delete"))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.remove_wifi_rule(&name, cx)),
                            ),
                    ),
                );
            }
        }
        form
    }
}
