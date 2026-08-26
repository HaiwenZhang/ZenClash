use std::path::PathBuf;

use gpui::PathPromptOptions;
use zenclash_core::BackupManager;

use super::super::super::{Context, Page, PreferencesRestored, ProfileActivated, RuntimePage};
use super::{format_backup_size, workflow::restore_backup, RestoreOutcome};

impl RuntimePage {
    pub(super) fn choose_backup_export(&mut self, cx: &mut Context<Self>) {
        let token = self.page_task_token_for(Page::Settings);
        let directory = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
        let receiver = cx.prompt_for_new_path(&directory, Some("zenclash-backup.zip"));
        cx.spawn(async move |this, cx| {
            let selection = receiver.await;
            let _ = this.update(cx, |this, cx| match selection {
                Ok(Ok(Some(path))) if this.is_page_task_current(token) => {
                    this.export_backup(path, cx);
                }
                Ok(Ok(Some(_))) => tracing::info!("discarded backup export after leaving settings"),
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    this.set_page_error(token, format!("无法打开备份保存对话框：{error}"));
                    cx.notify();
                }
                Err(error) => {
                    this.set_page_error(token, format!("备份保存对话框异常结束：{error}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn export_backup(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(token) = self.begin_mutation(Page::Settings) else {
            return;
        };
        let task = self.runtime.spawn_blocking(move || {
            BackupManager::discover()
                .and_then(|manager| manager.export_to(path))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("备份导出任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(summary) if this.is_page_task_current(token) => {
                        this.notice = Some(format!(
                            "已导出 {} 个文件（{}）到 {}",
                            summary.file_count,
                            format_backup_size(summary.payload_bytes),
                            summary.path.display()
                        ));
                    }
                    Ok(_) => {}
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn choose_backup_import(&mut self, cx: &mut Context<Self>) {
        let token = self.page_task_token_for(Page::Settings);
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择 ZenClash 备份 ZIP（将替换当前设置与配置）".into()),
        });
        cx.spawn(async move |this, cx| {
            let selection = receiver.await;
            let _ = this.update(cx, |this, cx| match selection {
                Ok(Ok(Some(paths))) if this.is_page_task_current(token) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.import_backup(path, cx);
                    }
                }
                Ok(Ok(Some(_))) => tracing::info!("discarded backup import after leaving settings"),
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    this.set_page_error(token, format!("无法打开备份选择器：{error}"));
                    cx.notify();
                }
                Err(error) => {
                    this.set_page_error(token, format!("备份选择器异常结束：{error}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn import_backup(&mut self, archive: PathBuf, cx: &mut Context<Self>) {
        let Some(token) = self.begin_mutation(Page::Settings) else {
            return;
        };
        let client = self.client.clone();
        let core_runtime = super::super::super::profiles::workflow::CoreProfileRuntime::new(
            self.core_kind,
            client,
            self.process.clone(),
        );
        let previous_profile = self.profile_path.clone();
        let task = self
            .runtime
            .spawn(restore_backup(archive, core_runtime, previous_profile));
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("备份恢复任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(outcome) => this.apply_restore_outcome(outcome, token, cx),
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(in crate::pages::runtime::settings) fn apply_restore_outcome(
        &mut self,
        outcome: RestoreOutcome,
        token: super::super::super::PageTaskToken,
        cx: &mut Context<Self>,
    ) {
        self.profile_path = Some(outcome.profile_path.clone());
        self.profile_store = Some(outcome.profile_store);
        self.profile_catalog = outcome.catalog;
        self.controlled_config_store = outcome.controlled_store;
        self.controlled_config = outcome.controlled_config;
        self.override_store = Some(outcome.override_store);
        self.override_catalog = outcome.override_catalog;
        self.invalidate_config_inputs();
        self.config_preview = None;
        cx.emit(ProfileActivated {
            path: outcome.profile_path,
        });
        self.preferences = outcome.preferences.clone();
        self.system_proxy_editor = None;
        cx.emit(PreferencesRestored {
            preferences: outcome.preferences,
        });
        if self.replace_page_data(token, outcome.page_data) {
            let warning = outcome.cleanup_warning.map_or_else(String::new, |warning| {
                format!("；但旧快照清理失败：{warning}")
            });
            self.notice = Some(format!(
                "备份已验证并恢复：{} 个文件，共 {}{warning}",
                outcome.file_count,
                format_backup_size(outcome.payload_bytes)
            ));
        }
    }
}
