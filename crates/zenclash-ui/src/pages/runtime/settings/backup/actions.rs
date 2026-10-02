use std::path::PathBuf;

use gpui_kit::PathPromptOptions;
use zenclash_core::BackupManager;

use super::super::super::{Context, Page, PreferencesRestored, RuntimePage};
use super::{
    RestoreOutcome, format_backup_size,
    workflow::{RetryOutcome, refresh_committed_state, restore_backup, retry_runtime},
};

impl RuntimePage {
    pub(super) fn retry_backup_runtime(&mut self, cx: &mut Context<Self>) {
        if self.profile_service.pending_backup_restore().is_none() {
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
        let runtime = self.profile_service.clone();
        let task = self.runtime.spawn(retry_runtime(runtime, data_root));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(RetryOutcome::Failed {
                        runtime_version,
                        error,
                    }) if this.profile_service.is_current(runtime_version)
                        && this.profile_service.pending_backup_restore().is_some() =>
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
                    }) if this.profile_service.is_current(runtime_version) => {
                        this.synchronize_committed_profile(cx);
                        match refresh {
                            Ok(refresh) => {
                                let snapshot = refresh.snapshot;
                                this.profiles.forms.catalog_view.prepare(&snapshot.profiles);
                                this.profiles.catalog = snapshot.profiles;
                                this.profiles.generation = this.profiles.generation.wrapping_add(1);
                                this.controlled_config = snapshot.controlled_config;
                                this.controlled_config_generation =
                                    this.controlled_config_generation.wrapping_add(1);
                                this.overrides.catalog = snapshot.overrides;
                                this.preferences = snapshot.preferences.clone();
                                this.system_proxy_editor = None;
                                cx.emit(PreferencesRestored {
                                    scope: crate::pages::runtime::PreferenceScope::Restore,
                                    preferences: snapshot.preferences,
                                });
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
        let core_runtime = self.profile_service.clone();
        let task = self.runtime.spawn(restore_backup(archive, core_runtime));
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
                this.finish_mutation(token);
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
                    Err(error) => this.set_page_error(token, error),
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
        self.profiles.store = Some(outcome.profile_store);
        self.profiles.generation = self.profiles.generation.wrapping_add(1);
        self.profiles.forms.catalog_view.prepare(&outcome.catalog);
        self.profiles.catalog = outcome.catalog;
        self.controlled_config_store = outcome.controlled_store;
        self.controlled_config_generation = self.controlled_config_generation.wrapping_add(1);
        self.controlled_config = outcome.controlled_config;
        self.overrides.store = Some(outcome.override_store);
        self.overrides.catalog = outcome.override_catalog;
        self.config_inputs.reset_on_next_refresh();
        self.invalidate_config_inputs(cx);
        self.overrides.invalidate_preview();
        self.synchronize_committed_profile(cx);
        self.preferences = outcome.preferences.clone();
        self.system_proxy_editor = None;
        cx.emit(PreferencesRestored {
            scope: crate::pages::runtime::PreferenceScope::Restore,
            preferences: outcome.preferences,
        });
        if self.profile_service.is_current(outcome.runtime_version)
            && self.replace_page_data(token, outcome.page_data, cx)
        {
            let warning = outcome.cleanup_warning.map_or_else(String::new, |warning| {
                zenclash_i18n::text_with("backup.notices.cleanup_warning", &[("warning", warning)])
            });
            self.notice = Some(zenclash_i18n::text_with(
                "backup.notices.restored",
                &[
                    ("count", outcome.file_count.to_string()),
                    ("size", format_backup_size(outcome.payload_bytes)),
                    ("warning", warning),
                ],
            ));
        }
    }
}
