use gpui::{Context, IntoElement, ParentElement, Styled, prelude::FluentBuilder};
use gpui_component::{
    Disableable, IconName, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
};
use zenclash_core::{AppUpdateService, AppUpdateStatus};

use super::super::{RuntimePage, div, info_row, message_banner, setting_card};

#[derive(Default)]
pub(in crate::pages::runtime) struct AppUpdateUiState {
    pub(in crate::pages::runtime) status: Option<AppUpdateStatus>,
    pub(super) loading: bool,
    pub(in crate::pages::runtime) checked: bool,
    pub(super) error: Option<String>,
}

impl RuntimePage {
    pub(in crate::pages::runtime) fn refresh_app_update(&mut self, cx: &mut Context<Self>) {
        if self.app_update.loading {
            return;
        }
        self.app_update.loading = true;
        self.app_update.error = None;
        let task = self.runtime.spawn(async {
            AppUpdateService::new()
                .map_err(|error| error.to_string())?
                .check(env!("CARGO_PKG_VERSION"))
                .await
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.app_update.loading = false;
                this.app_update.checked = true;
                match result {
                    Ok(status) => {
                        this.app_update.status = Some(status);
                        this.app_update.error = None;
                    }
                    Err(error) => this.app_update.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_app_release(&mut self, url: String, cx: &mut Context<Self>) {
        if let Err(error) = crate::app::platform::open_external_url(url) {
            self.app_update.error = Some(zenclash_i18n::text_with(
                "settings.app_update.open_error",
                &[("error", error.to_string())],
            ));
            cx.notify();
        }
    }

    pub(super) fn render_app_update(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let state = &self.app_update;
        let mut card = setting_card(zenclash_i18n::text("settings.app_update.title"), theme)
            .child(info_row(
                zenclash_i18n::text("settings.app_update.current"),
                env!("CARGO_PKG_VERSION"),
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("settings.app_update.policy"),
                zenclash_i18n::text("settings.app_update.policy_description"),
                theme,
            ));
        if let Some(error) = &state.error {
            card = card.child(message_banner(error.clone(), theme.warning, theme));
        }
        card = match &state.status {
            Some(AppUpdateStatus::NoPublishedRelease { .. }) => card.child(info_row(
                zenclash_i18n::text("settings.app_update.status"),
                zenclash_i18n::text("settings.app_update.no_release"),
                theme,
            )),
            Some(AppUpdateStatus::UpToDate { latest, .. }) => card.child(info_row(
                zenclash_i18n::text("settings.app_update.status"),
                zenclash_i18n::text_with(
                    "settings.app_update.up_to_date",
                    &[("version", latest.clone())],
                ),
                theme,
            )),
            Some(AppUpdateStatus::Available { release, .. }) => {
                let url = release.page_url.clone();
                card.child(message_banner(
                    zenclash_i18n::text_with(
                        "settings.app_update.available",
                        &[("version", release.tag.clone())],
                    ),
                    theme.success,
                    theme,
                ))
                .child(info_row(
                    zenclash_i18n::text("settings.app_update.published"),
                    if release.published_at.is_empty() {
                        "—"
                    } else {
                        &release.published_at
                    },
                    theme,
                ))
                .when(!release.notes.is_empty(), |card| {
                    card.child(
                        div()
                            .px_4()
                            .py_3()
                            .border_b_1()
                            .border_color(theme.border)
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(release.notes.clone()),
                    )
                })
                .child(
                    h_flex().justify_end().p_4().child(
                        Button::new("open-app-release")
                            .icon(IconName::ExternalLink)
                            .label(zenclash_i18n::text("settings.app_update.open_release"))
                            .small()
                            .primary()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_app_release(url.clone(), cx);
                            })),
                    ),
                )
            }
            None => card,
        };
        card.child(
            h_flex().justify_end().p_4().child(
                Button::new("check-app-update")
                    .icon(crate::assets::AppIcon::RefreshCw)
                    .label(zenclash_i18n::text(if state.loading {
                        "settings.app_update.checking"
                    } else {
                        "settings.app_update.check"
                    }))
                    .small()
                    .outline()
                    .loading(state.loading)
                    .disabled(state.loading)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_app_update(cx))),
            ),
        )
    }
}
