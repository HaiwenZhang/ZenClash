use std::path::PathBuf;

use gpui_kit::PathPromptOptions;
use zenclash_core::BackupManager;

use super::super::super::{Context, Page, RuntimePage};
use super::{
    RestoreOutcome, format_backup_size,
    workflow::{RetryOutcome, refresh_committed_state, restore_backup, retry_runtime},
};

impl RuntimePage {
    pub(super) fn retry_backup_runtime(&mut self, cx: &mut Context<Self>) {
        if self.application_commands.pending_backup_restore().is_none() {
            return;
        }
        let Some(token) = self.begin_scoped_mutation(
            Page::Settings,
            crate::pages::runtime::busy::MutationDomain::BackupRecovery,
        ) else {
            return;
        };
        let data_root = self
            .controlled_config_store
            .root()
            .parent()
            .map(std::path::Path::to_path_buf);
        let commands = self.application_commands.clone();
        let task = self.runtime.spawn(retry_runtime(commands, data_root));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let mut preferences_pending = false;
                match result {
                    Ok(RetryOutcome::Failed {
                        runtime_version,
                        error,
                    }) if this.application_commands.is_current(runtime_version)
                        && this.application_commands.pending_backup_restore().is_some() =>
                    {
                        this.set_page_error(
                            token,
                            zenclash_i18n::text_with(
                                "backup.errors.retry_failed",
                                &[("error", error)],
                            ),
                        );
                    }
                    Ok(RetryOutcome::Restored {
                        runtime_version,
                        refresh,
                    }) if this.application_commands.is_current(runtime_version) => {
                        this.synchronize_committed_profile(cx);
                        match refresh {
                            Ok(refresh) => {
                                let snapshot = refresh.snapshot;
                                this.profiles.adopt_retry_catalog(snapshot.profiles);
                                this.controlled_config = snapshot.controlled_config;
                                this.controlled_config_generation =
                                    this.controlled_config_generation.wrapping_add(1);
                                this.overrides.adopt_restored_catalog(snapshot.overrides);
                                preferences_pending = true;
                                this.publish_restored_preferences(
                                    refresh.preferences_observation,
                                    token,
                                    cx,
                                );
                                if this.replace_page_data(token, refresh.page_data, cx) {
                                    this.notice = Some(zenclash_i18n::text(
                                        "backup.notices.runtime_recovered",
                                    ));
                                }
                            }
                            Err(error) => this.set_page_error(
                                token,
                                zenclash_i18n::text_with(
                                    "backup.errors.retry_refresh_failed",
                                    &[("error", error)],
                                ),
                            ),
                        }
                    }
                    Ok(_) => {}
                    Err(error) => this.set_page_error(
                        token,
                        zenclash_i18n::text_with(
                            "backup.errors.retry_task",
                            &[("error", error.to_string())],
                        ),
                    ),
                }
                if !preferences_pending {
                    this.finish_mutation(token);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

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
                    this.set_page_error(
                        token,
                        zenclash_i18n::text_with(
                            "backup.errors.save_dialog",
                            &[("error", error.to_string())],
                        ),
                    );
                    cx.notify();
                }
                Err(error) => {
                    this.set_page_error(
                        token,
                        zenclash_i18n::text_with(
                            "backup.errors.save_dialog_task",
                            &[("error", error.to_string())],
                        ),
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn export_backup(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(token) = self.begin_scoped_mutation(
            Page::Settings,
            crate::pages::runtime::busy::MutationDomain::Backup,
        ) else {
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
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "backup.errors.export_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(summary) if this.is_page_task_current(token) => {
                        this.notice = Some(zenclash_i18n::text_with(
                            "backup.notices.exported",
                            &[
                                ("count", summary.file_count.to_string()),
                                ("size", format_backup_size(summary.payload_bytes)),
                                ("path", summary.path.display().to_string()),
                            ],
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
            prompt: Some(zenclash_i18n::text("backup.prompts.import").into()),
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
                    this.set_page_error(
                        token,
                        zenclash_i18n::text_with(
                            "backup.errors.file_picker",
                            &[("error", error.to_string())],
                        ),
                    );
                    cx.notify();
                }
                Err(error) => {
                    this.set_page_error(
                        token,
                        zenclash_i18n::text_with(
                            "backup.errors.file_picker_task",
                            &[("error", error.to_string())],
                        ),
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn import_backup(&mut self, archive: PathBuf, cx: &mut Context<Self>) {
        let Some(token) = self.begin_scoped_mutation(
            Page::Settings,
            crate::pages::runtime::busy::MutationDomain::Backup,
        ) else {
            return;
        };
        let application_commands = self.application_commands.clone();
        let task = self
            .runtime
            .spawn(restore_backup(archive, application_commands));
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "backup.errors.restore_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(outcome) => this.apply_restore_outcome(outcome, token, cx),
                    Err(error) => {
                        this.finish_mutation(token);
                        this.set_page_error(token, error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(in crate::pages::runtime) fn apply_restore_outcome(
        &mut self,
        outcome: RestoreOutcome,
        token: super::super::super::PageTaskToken,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .runtime
            .spawn_blocking(move || refresh_committed_state(outcome));
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(outcome) => this.finish_restore_outcome(outcome, token, cx),
                    Err(error) => {
                        this.finish_mutation(token);
                        this.set_page_error(token, error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn finish_restore_outcome(
        &mut self,
        outcome: RestoreOutcome,
        token: super::super::super::PageTaskToken,
        cx: &mut Context<Self>,
    ) {
        let restored = outcome.restored;
        let runtime_version = restored.runtime_generation();
        let file_count = restored.restored_file_count();
        let payload_bytes = restored.restored_payload_bytes();
        let cleanup_warning = restored.cleanup_warning().map(str::to_owned);
        self.profiles
            .adopt_restored_store(restored.profile_store().clone());
        self.controlled_config_store = restored.controlled_store().clone();
        self.overrides
            .adopt_restored_store(restored.override_store().clone());
        let preferences_observation = restored.preferences_observation().clone();
        let snapshot = restored.into_snapshot();
        self.profiles.adopt_restore_catalog(snapshot.profiles);
        self.controlled_config_generation = self.controlled_config_generation.wrapping_add(1);
        self.controlled_config = snapshot.controlled_config;
        self.overrides.adopt_restored_catalog(snapshot.overrides);
        self.config_inputs.reset_on_next_refresh();
        self.invalidate_config_inputs(cx);
        self.overrides.invalidate_preview();
        self.synchronize_committed_profile(cx);
        self.publish_restored_preferences(preferences_observation, token, cx);
        if self.application_commands.is_current(runtime_version)
            && self.replace_page_data(token, outcome.page_data, cx)
        {
            let warning = cleanup_warning.map_or_else(String::new, |warning| {
                zenclash_i18n::text_with("backup.notices.cleanup_warning", &[("warning", warning)])
            });
            self.notice = Some(zenclash_i18n::text_with(
                "backup.notices.restored",
                &[
                    ("count", file_count.to_string()),
                    ("size", format_backup_size(payload_bytes)),
                    ("warning", warning),
                ],
            ));
        }
    }
}

#[cfg(test)]
mod tests;
