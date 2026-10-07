use super::*;
use crate::components::mint_switch::MintSwitch;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, WindowExt};

#[derive(Clone, Copy)]
pub(super) struct LogSettingsDraft {
    level: Option<MihomoLogLevel>,
    enabled: bool,
    max_mebibytes: u16,
}

impl RuntimePage {
    pub(super) fn open_log_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let owner = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let content = owner
                .update(cx, |page, cx| page.log_details_content(cx))
                .ok();
            dialog
                .bg(cx.theme().group_box)
                .title(zenclash_i18n::text("redesign.log_details"))
                .width(window.rem_size() * 38.)
                .margin_top(
                    ((window.viewport_size().height - window.rem_size() * 25.) / 2.)
                        .max(gpui_kit::px(16.)),
                )
                .when_some(content, |dialog, content| dialog.child(content))
        });
        cx.notify();
    }

    fn log_details_content(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let Some((entry, row)) = &self.logs.selected else {
            return div().into_any_element();
        };
        let time = i64::try_from(entry.timestamp_ms)
            .ok()
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|time| {
                time.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S%.3f")
                    .to_string()
            })
            .unwrap_or_else(|| row.time.to_string());
        let payload = entry.payload.clone();
        let time_source = zenclash_i18n::text(match row.time_source {
            LogTimeSource::Core => "logs.ui.core",
            LogTimeSource::LocalReceive => "logs.ui.local",
        });
        v_flex()
            .gap_3()
            .child(div().text_sm().child(format!(
                "{}   {time} · {time_source}",
                zenclash_i18n::text("logs.columns.time")
            )))
            .child(div().text_sm().child(format!(
                "{}   {}",
                zenclash_i18n::text("logs.columns.level"),
                row.level
            )))
            .child(div().text_sm().child(format!(
                "{}   {}",
                zenclash_i18n::text("logs.columns.source"),
                self.core_kind.display_name()
            )))
            .child(
                div()
                    .p_3()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().secondary)
                    .text_sm()
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(payload.clone()),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("copy-log-content")
                            .outline()
                            .label(zenclash_i18n::text("redesign.copy_content"))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(payload.clone()))
                            }),
                    )
                    .child(
                        Button::new("close-log-details")
                            .primary()
                            .label(zenclash_i18n::text("common.actions.close"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn open_log_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.logs.settings = Some(LogSettingsDraft {
            level: self.logs.level,
            enabled: self.preferences.log_file_enabled,
            max_mebibytes: self.preferences.log_file_max_mebibytes,
        });
        let owner = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let content = owner
                .update(cx, |page, cx| page.log_settings_content(cx))
                .ok();
            let owner = owner.clone();
            dialog
                .bg(cx.theme().group_box)
                .title(zenclash_i18n::text("redesign.log_settings_title"))
                .width(window.rem_size() * 30.)
                .margin_top(
                    ((window.viewport_size().height - window.rem_size() * 24.) / 2.)
                        .max(gpui_kit::px(16.)),
                )
                .when_some(content, |dialog, content| dialog.child(content))
                .on_cancel(move |_, _, cx| {
                    let _ = owner.update(cx, |page, _| page.logs.settings = None);
                    true
                })
        });
        cx.notify();
    }

    fn log_settings_content(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let Some(draft) = self.logs.settings else {
            return div().into_any_element();
        };
        let owner = cx.entity().downgrade();
        let level = Button::new("log-collection-level")
            .outline()
            .dropdown_caret(true)
            .label(draft.level.map_or_else(
                || "—".into(),
                |level| {
                    zenclash_i18n::text(match level {
                        MihomoLogLevel::Info => "redesign.log_info",
                        MihomoLogLevel::Warning => "redesign.log_warning",
                        MihomoLogLevel::Error => "redesign.log_error",
                        MihomoLogLevel::Debug => "redesign.log_debug",
                        MihomoLogLevel::Silent => "redesign.log_silent",
                    })
                },
            ))
            .tooltip(
                draft
                    .level
                    .map_or_else(|| "—".into(), log_level_description),
            )
            .disabled(
                self.core_busy()
                    || self.config_inputs_loading
                    || !self
                        .config_inputs
                        .is_for_profile(self.profile_path.as_deref()),
            )
            .dropdown_menu(move |mut menu, _, _| {
                for level in [
                    MihomoLogLevel::Info,
                    MihomoLogLevel::Debug,
                    MihomoLogLevel::Warning,
                    MihomoLogLevel::Error,
                    MihomoLogLevel::Silent,
                ] {
                    let owner = owner.clone();
                    menu = menu.item(
                        PopupMenuItem::new(level.api_value().to_uppercase())
                            .checked(draft.level == Some(level))
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |page, cx| {
                                    if let Some(draft) = &mut page.logs.settings {
                                        draft.level = Some(level);
                                    }
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            });
        let owner = cx.entity().downgrade();
        let limit = Button::new("log-draft-file-limit")
            .outline()
            .dropdown_caret(true)
            .label(format!("{} MiB", draft.max_mebibytes))
            .dropdown_menu(move |mut menu, _, _| {
                for limit in [5, 10, 25, 50] {
                    let owner = owner.clone();
                    menu = menu.item(
                        PopupMenuItem::new(format!("{limit} MiB"))
                            .checked(draft.max_mebibytes == limit)
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |page, cx| {
                                    if let Some(draft) = &mut page.logs.settings {
                                        draft.max_mebibytes = limit;
                                    }
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            });
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .justify_between()
                    .child(zenclash_i18n::text("logs.collection_level"))
                    .child(level),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(zenclash_i18n::text("logs.persistence.enabled.title"))
                    .child(
                        MintSwitch::new("log-draft-file-enabled")
                            .checked(draft.enabled)
                            .on_click(cx.listener(|page, checked, _, cx| {
                                if let Some(draft) = &mut page.logs.settings {
                                    draft.enabled = *checked;
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(zenclash_i18n::text("logs.persistence.limit"))
                    .child(limit),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(zenclash_i18n::text("redesign.log_privacy")),
            )
            .child(
                Button::new("copy-support-safe-logs")
                    .ghost()
                    .label(zenclash_i18n::text("logs.actions.copy_safe"))
                    .loading(self.logs.copying)
                    .disabled(self.logs.copying || self.logs.presentation.entries.is_empty())
                    .on_click(cx.listener(|page, _, _, cx| page.copy_support_safe_logs(cx))),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-log-settings")
                            .outline()
                            .label(zenclash_i18n::text("common.actions.cancel"))
                            .on_click(cx.listener(|page, _, window, cx| {
                                page.logs.settings = None;
                                window.close_dialog(cx);
                            })),
                    )
                    .child(
                        Button::new("save-log-settings")
                            .primary()
                            .label(zenclash_i18n::text("common.actions.save"))
                            .disabled(self.preferences_store.is_none() || self.core_busy())
                            .on_click(cx.listener(|page, _, window, cx| {
                                if let Some(draft) = page.logs.settings.take() {
                                    if draft.level != page.logs.level
                                        && let Some(level) = draft.level
                                    {
                                        page.apply_controlled_config(
                                            serde_json::json!({"log-level": level.api_value()}),
                                            zenclash_i18n::text("logs.collection_level_updated"),
                                            cx,
                                        );
                                    }
                                    page.persist_log_preferences(
                                        Some(draft.enabled),
                                        Some(draft.max_mebibytes),
                                        zenclash_i18n::text("logs.notices.limit_saved"),
                                        cx,
                                    );
                                    window.close_dialog(cx);
                                }
                            })),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::runtime::ui_tests::{Fixture, open};
    use gpui_kit::test::TestWindowExt;

    #[gpui_kit::test]
    fn cancelling_log_settings_discards_draft_without_changing_monitor_or_preferences(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let fixture = Fixture::new();
        let (window, page) = open(cx, &fixture, Page::Logs);
        fixture.settle(cx, &page, |page| !page.persistent_loading);
        cx.update_window(window, |_, window, cx| {
            let enabled = page.read(cx).preferences.log_file_enabled;
            let level = page.read(cx).log_monitor.level();
            window.render_frame(cx);
            window.click("log-settings", cx);
            window.render_frame(cx);
            window.render_frame(cx);
            window.click("log-draft-file-enabled", cx);
            assert_eq!(page.read(cx).logs.settings.unwrap().enabled, !enabled);
            window.press("escape", cx);
            assert!(!window.has_active_dialog(cx));
            assert!(page.read(cx).logs.settings.is_none());
            assert_eq!(page.read(cx).preferences.log_file_enabled, enabled);
            assert_eq!(page.read(cx).log_monitor.level(), level);
            window.remove_window();
        })
        .unwrap();
    }
}
