use std::sync::Arc;

use gpui::{Context, IntoElement, ParentElement, Styled, prelude::FluentBuilder};
use gpui_component::{
    Disableable, IconName, Sizable,
    button::{Button, ButtonVariants},
    h_flex, v_flex,
};
use zenclash_core::{MihomoClient, MihomoProcess, MihomoRelease, MihomoReleaseService};

use super::super::{
    Page, RuntimePage, format_bytes, info_row, load_page, message_banner, setting_card,
};

#[derive(Default)]
pub(crate) struct CoreReleaseState {
    releases: Vec<MihomoRelease>,
    loading: bool,
    error: Option<String>,
}

impl RuntimePage {
    fn fetch_core_releases(&mut self, cx: &mut Context<Self>) {
        if !self.core_kind.capabilities().core_upgrade {
            self.core_releases.error = Some(zenclash_i18n::text_with(
                "core_page.errors.release_unsupported",
                &[("core", self.core_kind.display_name().to_owned())],
            ));
            cx.notify();
            return;
        }
        if self.core_releases.loading {
            return;
        }
        self.core_releases.loading = true;
        self.core_releases.error = None;
        let task = self.runtime.spawn(async {
            MihomoReleaseService::new()
                .map_err(|error| error.to_string())?
                .releases(12)
                .await
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "core_page.errors.release_list_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.core_releases.loading = false;
                match result {
                    Ok(releases) => {
                        this.core_releases.releases = releases;
                        this.core_releases.error = None;
                    }
                    Err(error) => this.core_releases.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn install_core_release(&mut self, release: MihomoRelease, cx: &mut Context<Self>) {
        if !self.core_kind.capabilities().core_upgrade {
            self.error = Some(zenclash_i18n::text_with(
                "core_page.errors.release_unsupported",
                &[("core", self.core_kind.display_name().to_owned())],
            ));
            cx.notify();
            return;
        }
        let Some(process) = self.process.clone() else {
            self.error = Some(zenclash_i18n::text("core_page.errors.external_install"));
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(Page::Mihomo) else {
            return;
        };
        let client = self.client.clone();
        let tag = release.tag.clone();
        let task = self.runtime.spawn(async move {
            install_release(client.clone(), process, release).await?;
            load_page(client, Page::Mihomo).await
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "core_page.errors.release_install_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
                            this.notice = Some(zenclash_i18n::text_with(
                                "core_page.notices.release_installed",
                                &[("version", tag)],
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

    pub(super) fn render_versioned_core_updates(
        &self,
        current_version: &str,
        managed_process: bool,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let releases = self.core_releases.releases.clone();
        setting_card(
            zenclash_i18n::text("core_page.maintenance.versioned_title"),
            theme,
        )
        .child(info_row(
            zenclash_i18n::text("core_page.maintenance.trusted_source"),
            zenclash_i18n::text("core_page.maintenance.trusted_source_description"),
            theme,
        ))
        .child(info_row(
            zenclash_i18n::text("core_page.maintenance.replacement_strategy"),
            zenclash_i18n::text("core_page.maintenance.replacement_strategy_description"),
            theme,
        ))
        .when_some(self.core_releases.error.clone(), |this, error| {
            this.child(message_banner(error, theme.danger, theme))
        })
        .children(releases.into_iter().enumerate().map(|(index, release)| {
            let is_current = versions_match(current_version, &release.tag);
            let release_for_action = release.clone();
            let digest = format!("{}…", &release.asset.sha256[..12]);
            h_flex()
                .items_center()
                .justify_between()
                .gap_3()
                .px_4()
                .py_3()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    v_flex()
                        .min_w_0()
                        .gap_1()
                        .child(
                            h_flex().gap_2().child(release.tag.clone()).child(
                                gpui::div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(if release.prerelease {
                                        zenclash_i18n::text("core_page.maintenance.prerelease")
                                    } else {
                                        zenclash_i18n::text("core_page.maintenance.stable")
                                    }),
                            ),
                        )
                        .child(
                            gpui::div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!(
                                    "{} · {} · SHA-256 {digest}",
                                    release.published_at,
                                    format_bytes(release.asset.size)
                                )),
                        ),
                )
                .child(
                    Button::new(("install-core", index))
                        .icon(IconName::ArrowDown)
                        .label(zenclash_i18n::text(if is_current {
                            "core_page.maintenance.current_version"
                        } else {
                            "core_page.maintenance.install"
                        }))
                        .small()
                        .outline()
                        .loading(self.mutating && !is_current)
                        .disabled(
                            self.mutating
                                || !managed_process
                                || is_current
                                || !self.core_kind.capabilities().core_upgrade,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.install_core_release(release_for_action.clone(), cx);
                        })),
                )
        }))
        .child(
            h_flex().justify_end().p_4().child(
                Button::new("fetch-mihomo-releases")
                    .icon(IconName::Redo2)
                    .label(zenclash_i18n::text(if self.core_releases.loading {
                        "core_page.maintenance.loading_versions"
                    } else if self.core_releases.releases.is_empty() {
                        "core_page.maintenance.load_versions"
                    } else {
                        "core_page.maintenance.refresh_versions"
                    }))
                    .small()
                    .primary()
                    .loading(self.core_releases.loading)
                    .disabled(
                        self.core_releases.loading
                            || self.mutating
                            || !self.core_kind.capabilities().core_upgrade,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.fetch_core_releases(cx);
                    })),
            ),
        )
        .into_any_element()
    }
}

async fn install_release(
    client: MihomoClient,
    process: Arc<MihomoProcess>,
    release: MihomoRelease,
) -> Result<(), String> {
    MihomoReleaseService::new()
        .map_err(|error| error.to_string())?
        .install_managed(&release, process, client)
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn versions_match(left: &str, right: &str) -> bool {
    left.trim().trim_start_matches('v') == right.trim().trim_start_matches('v')
}

#[cfg(test)]
mod tests {
    use super::versions_match;

    #[test]
    fn version_comparison_ignores_only_the_conventional_v_prefix() {
        assert!(versions_match("v1.19.30", "1.19.30"));
        assert!(versions_match(" 1.19.30 ", "v1.19.30"));
        assert!(!versions_match("1.19.3", "v1.19.30"));
    }
}
