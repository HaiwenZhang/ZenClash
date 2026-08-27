use zenclash_core::{
    BackupManager, WebDavBackup, WebDavService, WebDavSettings, WebDavSettingsStore,
};

use super::super::super::{Context, Page, RuntimePage, Window};
use super::workflow::restore_remote_backup;

struct WebDavActionOutcome {
    backups: Vec<WebDavBackup>,
    notice: String,
}

impl RuntimePage {
    pub(crate) fn webdav_backup_completed_in_background(
        &mut self,
        summary: &zenclash_core::WebDavUploadSummary,
        backups: Vec<WebDavBackup>,
        cx: &mut Context<Self>,
    ) {
        self.webdav.backups = backups;
        self.webdav.verified = !self.webdav.dirty;
        if self.page == Page::Settings {
            self.notice = Some(zenclash_i18n::text_with(
                "webdav.notices.scheduled_created",
                &[
                    ("filename", summary.backup.filename.clone()),
                    ("count", summary.removed_backups.to_string()),
                ],
            ));
            cx.notify();
        }
    }

    pub(crate) fn report_background_webdav_error(&mut self, error: &str, cx: &mut Context<Self>) {
        self.webdav.verified = false;
        if self.page == Page::Settings {
            self.error = Some(zenclash_i18n::text_with(
                "webdav.errors.scheduled",
                &[("error", error.to_owned())],
            ));
            cx.notify();
        }
    }

    pub(in crate::pages::runtime::settings) fn confirm_webdav_restore(
        &mut self,
        filename: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let token = self.page_task_token_for(Page::Settings);
        let detail = zenclash_i18n::text_with(
            "webdav.prompts.restore_detail",
            &[
                ("filename", filename.clone()),
                ("core", self.core_kind.display_name().to_owned()),
            ],
        );
        let receiver = window.prompt(
            gpui::PromptLevel::Warning,
            &zenclash_i18n::text("webdav.prompts.restore_title"),
            Some(&detail),
            &[
                gpui::PromptButton::cancel(zenclash_i18n::text("common.actions.cancel")),
                gpui::PromptButton::ok(zenclash_i18n::text("common.actions.restore")),
            ],
            cx,
        );
        cx.spawn(async move |this, cx| {
            let choice = receiver.await;
            let _ = this.update(cx, |this, cx| {
                if matches!(choice, Ok(1)) && this.is_page_task_current(token) {
                    this.restore_webdav_backup(filename, cx);
                }
            });
        })
        .detach();
    }

    pub(in crate::pages::runtime::settings) fn confirm_webdav_delete(
        &mut self,
        filename: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let token = self.page_task_token_for(Page::Settings);
        let detail = zenclash_i18n::text_with(
            "webdav.prompts.delete_detail",
            &[("filename", filename.clone())],
        );
        let receiver = window.prompt(
            gpui::PromptLevel::Critical,
            &zenclash_i18n::text("webdav.prompts.delete_title"),
            Some(&detail),
            &[
                gpui::PromptButton::cancel(zenclash_i18n::text("common.actions.cancel")),
                gpui::PromptButton::ok(zenclash_i18n::text("common.actions.delete")),
            ],
            cx,
        );
        cx.spawn(async move |this, cx| {
            let choice = receiver.await;
            let _ = this.update(cx, |this, cx| {
                if matches!(choice, Ok(1)) && this.is_page_task_current(token) {
                    this.delete_webdav_backup(filename, cx);
                }
            });
        })
        .detach();
    }

    pub(in crate::pages::runtime::settings) fn test_webdav(&mut self, cx: &mut Context<Self>) {
        self.list_webdav_backups(zenclash_i18n::text("webdav.notices.connection_saved"), cx);
    }

    pub(in crate::pages::runtime::settings) fn refresh_webdav_backups(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        self.list_webdav_backups(zenclash_i18n::text("webdav.notices.list_refreshed"), cx);
    }

    fn list_webdav_backups(&mut self, success: String, cx: &mut Context<Self>) {
        let Some((token, settings, store)) = self.begin_webdav_action(cx) else {
            return;
        };
        let task = self.runtime.spawn(async move {
            save_webdav_settings(store, settings.clone()).await?;
            let backups = WebDavService::new(settings)
                .map_err(|error| error.to_string())?
                .test_connection()
                .await
                .map_err(|error| error.to_string())?;
            Ok::<_, String>(WebDavActionOutcome {
                backups,
                notice: success,
            })
        });
        Self::finish_webdav_action(token, task, cx);
    }

    pub(in crate::pages::runtime::settings) fn upload_webdav_backup(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        let Some((token, settings, store)) = self.begin_webdav_action(cx) else {
            return;
        };
        let task = self.runtime.spawn(async move {
            save_webdav_settings(store, settings.clone()).await?;
            let service = WebDavService::new(settings).map_err(|error| error.to_string())?;
            let manager = tokio::task::spawn_blocking(BackupManager::discover)
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "webdav.errors.local_directory_task",
                        &[("error", error.to_string())],
                    )
                })?
                .map_err(|error| error.to_string())?;
            let summary = service
                .upload_snapshot(&manager)
                .await
                .map_err(|error| error.to_string())?;
            let backups = service
                .list_backups()
                .await
                .map_err(|error| error.to_string())?;
            Ok::<_, String>(WebDavActionOutcome {
                backups,
                notice: if summary.removed_backups == 0 {
                    zenclash_i18n::text_with(
                        "webdav.notices.created",
                        &[("filename", summary.backup.filename)],
                    )
                } else {
                    zenclash_i18n::text_with(
                        "webdav.notices.created_cleaned",
                        &[
                            ("filename", summary.backup.filename),
                            ("count", summary.removed_backups.to_string()),
                        ],
                    )
                },
            })
        });
        Self::finish_webdav_action(token, task, cx);
    }

    pub(in crate::pages::runtime::settings) fn delete_webdav_backup(
        &mut self,
        filename: String,
        cx: &mut Context<Self>,
    ) {
        let Some((token, settings, store)) = self.begin_webdav_action(cx) else {
            return;
        };
        let task = self.runtime.spawn(async move {
            save_webdav_settings(store, settings.clone()).await?;
            let service = WebDavService::new(settings).map_err(|error| error.to_string())?;
            service
                .delete_backup(&filename)
                .await
                .map_err(|error| error.to_string())?;
            let backups = service
                .list_backups()
                .await
                .map_err(|error| error.to_string())?;
            Ok::<_, String>(WebDavActionOutcome {
                backups,
                notice: zenclash_i18n::text_with(
                    "webdav.notices.deleted",
                    &[("filename", filename)],
                ),
            })
        });
        Self::finish_webdav_action(token, task, cx);
    }

    pub(in crate::pages::runtime::settings) fn restore_webdav_backup(
        &mut self,
        filename: String,
        cx: &mut Context<Self>,
    ) {
        let Some((token, settings, store)) = self.begin_webdav_action(cx) else {
            return;
        };
        let core_runtime = super::super::super::profiles::workflow::CoreProfileRuntime::new(
            self.core_session.clone(),
        );
        let previous_profile = self.profile_path.clone();
        let task = self.runtime.spawn(async move {
            save_webdav_settings(store, settings.clone()).await?;
            restore_remote_backup(settings, filename, core_runtime, previous_profile).await
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "webdav.errors.restore_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok((outcome, backups)) if this.is_page_task_current(token) => {
                        this.webdav.backups = backups;
                        this.webdav.verified = true;
                        this.webdav.dirty = false;
                        this.apply_restore_outcome(outcome, token, cx);
                    }
                    Ok(_) => {}
                    Err(error) => {
                        this.webdav.verified = false;
                        this.set_page_error(token, error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn begin_webdav_action(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<(
        super::super::super::PageTaskToken,
        WebDavSettings,
        WebDavSettingsStore,
    )> {
        let settings = match self.webdav.settings(cx) {
            Ok(settings) => settings,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return None;
            }
        };
        let store = match self.webdav.store() {
            Ok(store) => store,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return None;
            }
        };
        if let Err(error) = WebDavService::new(settings.clone()) {
            self.error = Some(error.to_string());
            cx.notify();
            return None;
        }
        self.begin_mutation(Page::Settings)
            .map(|token| (token, settings, store))
    }

    fn finish_webdav_action(
        token: super::super::super::PageTaskToken,
        task: tokio::task::JoinHandle<Result<WebDavActionOutcome, String>>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "webdav.errors.action_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(outcome) if this.is_page_task_current(token) => {
                        this.webdav.backups = outcome.backups;
                        this.webdav.verified = true;
                        this.webdav.dirty = false;
                        this.notice = Some(outcome.notice);
                    }
                    Ok(_) => {}
                    Err(error) => {
                        this.webdav.verified = false;
                        this.set_page_error(token, error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

async fn save_webdav_settings(
    store: WebDavSettingsStore,
    settings: WebDavSettings,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || store.save(&settings))
        .await
        .map_err(|error| {
            zenclash_i18n::text_with("webdav.errors.save_task", &[("error", error.to_string())])
        })?
        .map_err(|error| error.to_string())
}
