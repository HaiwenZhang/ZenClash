use super::super::super::{
    Button, ButtonVariants, Context, Disableable, FluentBuilder, Input, ParentElement,
    RemoteProfileRoute, RuntimePage, Styled, div, h_flex, v_flex,
};
use crate::components::mint_switch::MintSwitch;
use gpui_kit::base::Selectable;
use gpui_kit::component::WindowExt;
use gpui_kit::{InteractiveElement, TestSupportExt};

impl RuntimePage {
    pub(in crate::pages::runtime) fn render_remote_profile_editor(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl gpui_kit::IntoElement + use<> {
        let name = self
            .profiles
            .forms
            .editing_profile_id
            .as_deref()
            .and_then(|id| {
                self.profiles
                    .catalog
                    .profiles
                    .iter()
                    .find(|profile| profile.id == id)
            })
            .map_or_else(String::new, |profile| profile.name.clone());
        let field = |label: String, input: Input| {
            v_flex()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(input.disabled(self.core_busy()))
        };
        v_flex()
            .id("profile-request-dialog")
            .test_support()
            .gap_4()
            .min_w_0()
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(name),
            )
            .child(field(
                zenclash_i18n::text("profiles.form.name"),
                Input::new(&self.profiles.forms.request_name),
            ))
            .child(field(
                zenclash_i18n::text("profiles.editor.url"),
                Input::new(&self.profiles.forms.request_url),
            ))
            .child(field(
                "User-Agent".into(),
                Input::new(&self.profiles.forms.request_user_agent),
            ))
            .child(field(
                "Authorization".into(),
                Input::new(&self.profiles.forms.request_authorization).mask_toggle(),
            ))
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .w(gpui_kit::rems(7.))
                            .flex_shrink_0()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("profiles.design.download_route")),
                    )
                    .child(
                        h_flex().flex_1().min_w_0().gap_2().children(
                            [
                                ("edit-profile-direct", "profiles.dialog.direct", false),
                                (
                                    "edit-profile-use-mihomo-proxy",
                                    "profiles.dialog.mihomo",
                                    true,
                                ),
                            ]
                            .into_iter()
                            .map(|(id, label, proxy)| {
                                Button::new(id)
                                    .outline()
                                    .flex_1()
                                    .h_10()
                                    .label(zenclash_i18n::text(label))
                                    .selected(
                                        (self.profiles.forms.editing_route
                                            == RemoteProfileRoute::Mihomo)
                                            == proxy,
                                    )
                                    .disabled(self.core_busy())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.profiles.forms.editing_route = if proxy {
                                            RemoteProfileRoute::Mihomo
                                        } else {
                                            RemoteProfileRoute::DirectWithMihomoFallback
                                        };
                                        cx.notify();
                                    }))
                            }),
                        ),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .w(gpui_kit::rems(7.))
                            .flex_shrink_0()
                            .text_sm()
                            .child(zenclash_i18n::text("profiles.editor.fallback")),
                    )
                    .child(
                        MintSwitch::new("edit-profile-mihomo-fallback")
                            .checked(
                                self.profiles.forms.editing_route
                                    == RemoteProfileRoute::DirectWithMihomoFallback,
                            )
                            .disabled(
                                self.core_busy()
                                    || self.profiles.forms.editing_route
                                        == RemoteProfileRoute::Mihomo,
                            )
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.profiles.forms.editing_route = if *checked {
                                    RemoteProfileRoute::DirectWithMihomoFallback
                                } else {
                                    RemoteProfileRoute::Direct
                                };
                                cx.notify();
                            })),
                    ),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(div().text_sm().text_color(theme.danger).child(error))
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .pt_3()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        Button::new("cancel-profile-request-edit")
                            .min_w_24()
                            .outline()
                            .h_10()
                            .label(zenclash_i18n::text("profiles.actions.cancel"))
                            .disabled(self.core_busy())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.cancel_edit_remote_profile(cx);
                                window.close_dialog(cx);
                            })),
                    )
                    .child(
                        Button::new("save-profile-request-edit")
                            .min_w(gpui_kit::rems(7.))
                            .primary()
                            .h_10()
                            .label(zenclash_i18n::text("profiles.dialog.save"))
                            .loading(self.core_busy())
                            .on_click(
                                cx.listener(|this, _, _, cx| this.save_remote_profile_settings(cx)),
                            ),
                    ),
            )
    }
}
