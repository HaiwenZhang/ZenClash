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
            self.notice = Some(format!(
                "定时远端备份 {} 已创建，清理 {} 份当前设备旧备份",
                summary.backup.filename, summary.removed_backups
            ));
            cx.notify();
        }
    }

    pub(crate) fn report_background_webdav_error(&mut self, error: &str, cx: &mut Context<Self>) {
        self.webdav.verified = false;
        if self.page == Page::Settings {
            self.error = Some(format!("定时 WebDAV 备份失败：{error}"));
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
        let detail = format!(
            "将下载并完整验证 {filename}，随后替换当前偏好、受控设置和配置仓库。{} 拒绝配置时会自动回滚。",
            self.core_kind.display_name()
        );
        let receiver = window.prompt(
            gpui::PromptLevel::Warning,
            "恢复这份远端备份？",
            Some(&detail),
            &[
                gpui::PromptButton::cancel("取消"),
                gpui::PromptButton::ok("恢复"),
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
        let detail = format!("将从服务器永久删除 {filename}。此操作不能撤销。");
        let receiver = window.prompt(
            gpui::PromptLevel::Critical,
            "删除这份远端备份？",
            Some(&detail),
            &[
                gpui::PromptButton::cancel("取消"),
                gpui::PromptButton::ok("删除"),
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
        self.list_webdav_backups("WebDAV 连接已验证，设置已保存", cx);
    }

    pub(in crate::pages::runtime::settings) fn refresh_webdav_backups(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        self.list_webdav_backups("WebDAV 备份列表已刷新", cx);
    }

    fn list_webdav_backups(&mut self, success: &'static str, cx: &mut Context<Self>) {
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
                notice: success.into(),
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
                .map_err(|error| format!("本地备份目录任务异常结束：{error}"))?
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
                    format!("远端备份 {} 已创建", summary.backup.filename)
                } else {
                    format!(
                        "远端备份 {} 已创建，并清理 {} 份当前设备旧备份",
                        summary.backup.filename, summary.removed_backups
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
                notice: format!("远端备份 {filename} 已删除"),
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
        let client = self.client.clone();
        let core_runtime = super::super::super::profiles::workflow::CoreProfileRuntime::new(
            self.core_kind,
            client,
            self.process.clone(),
        );
        let previous_profile = self.profile_path.clone();
        let task = self.runtime.spawn(async move {
            save_webdav_settings(store, settings.clone()).await?;
            restore_remote_backup(settings, filename, core_runtime, previous_profile).await
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("WebDAV 恢复任务异常结束：{error}"))
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
                .map_err(|error| format!("WebDAV 任务异常结束：{error}"))
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
        .map_err(|error| format!("WebDAV 设置保存任务异常结束：{error}"))?
        .map_err(|error| error.to_string())
}
