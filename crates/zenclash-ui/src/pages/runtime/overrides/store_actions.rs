use std::path::PathBuf;

use super::super::{Context, Page, RuntimePage, YamlOverrideCatalog};

impl RuntimePage {
    pub(in crate::pages::runtime) fn enabled_override_paths(&self) -> Vec<PathBuf> {
        self.overrides.enabled_paths()
    }

    pub(super) fn import_override_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(store) = self.overrides.store.clone() else {
            self.error = Some(zenclash_i18n::text("overrides.errors.store_unavailable"));
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(Page::Override) else {
            return;
        };
        let controlled = self.controlled_config_store.clone();
        let core_runtime = self.profile_service.clone();
        let core_name = self.core_kind.display_name();
        let task = self.runtime.spawn(async move {
            let import_store = store.clone();
            let imported = tokio::task::spawn_blocking(move || import_store.import_paths(paths))
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "overrides.errors.import_task",
                        &[("error", error.to_string())],
                    )
                })?
                .map_err(|error| error.to_string())?;
            let catalog_store = store.clone();
            let catalog = tokio::task::spawn_blocking(move || catalog_store.load())
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "overrides.errors.catalog_read_task",
                        &[("error", error.to_string())],
                    )
                })?
                .map_err(|error| error.to_string())?;
            let reapplication = core_runtime
                .reapply_with_overrides(controlled, store.enabled_paths(&catalog))
                .await;
            if let Err(error) = reapplication {
                let cleanup_store = store.clone();
                let ids = imported
                    .iter()
                    .map(|record| record.id.clone())
                    .collect::<Vec<_>>();
                let cleanup = tokio::task::spawn_blocking(move || {
                    for id in ids {
                        cleanup_store.delete(&id)?;
                    }
                    Ok::<_, zenclash_core::YamlOverrideError>(())
                })
                .await;
                return match cleanup {
                    Ok(Ok(())) => Err(zenclash_i18n::text_with(
                        "overrides.errors.import_rejected_removed",
                        &[("core", core_name.to_owned()), ("error", error.clone())],
                    )),
                    Ok(Err(cleanup)) => Err(zenclash_i18n::text_with(
                        "overrides.errors.import_rejected_cleanup_failed",
                        &[
                            ("core", core_name.to_owned()),
                            ("error", error.clone()),
                            ("cleanup", cleanup.to_string()),
                        ],
                    )),
                    Err(cleanup) => Err(zenclash_i18n::text_with(
                        "overrides.errors.import_rejected_cleanup_task",
                        &[
                            ("core", core_name.to_owned()),
                            ("error", error),
                            ("cleanup", cleanup.to_string()),
                        ],
                    )),
                };
            }
            reapplication.map(|applied| (catalog, imported.len(), applied))
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "overrides.errors.import_workflow",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok((catalog, count, applied)) => {
                        this.overrides.catalog = catalog;
                        this.overrides.invalidate_preview();
                        this.invalidate_config_inputs(cx);
                        if this.is_page_task_current(token) {
                            this.notice = Some(if applied.is_some() {
                                zenclash_i18n::text_with(
                                    "overrides.notices.imported",
                                    &[("count", count.to_string())],
                                )
                            } else {
                                zenclash_i18n::text("overrides.notices.saved_unapplied")
                            });
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

    pub(super) fn set_override_enabled(&mut self, id: &str, enabled: bool, cx: &mut Context<Self>) {
        let before = self.overrides.catalog.clone();
        let mut next = before.clone();
        let Some(record) = next.items.iter_mut().find(|record| record.id == id) else {
            self.error = Some(zenclash_i18n::text("overrides.errors.record_missing"));
            cx.notify();
            return;
        };
        record.enabled = enabled;
        self.apply_override_catalog_change(
            before,
            next,
            if enabled {
                zenclash_i18n::text("overrides.notices.enabled")
            } else {
                zenclash_i18n::text("overrides.notices.disabled")
            },
            cx,
        );
    }

    pub(super) fn move_override(&mut self, id: &str, offset: isize, cx: &mut Context<Self>) {
        let before = self.overrides.catalog.clone();
        let mut next = before.clone();
        let Some(current) = next.items.iter().position(|record| record.id == id) else {
            return;
        };
        let target = current.saturating_add_signed(offset);
        if target >= next.items.len() || target == current {
            return;
        }
        let record = next.items.remove(current);
        next.items.insert(target, record);
        self.apply_override_catalog_change(
            before,
            next,
            zenclash_i18n::text("overrides.notices.reordered"),
            cx,
        );
    }

    pub(super) fn delete_disabled_override(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(record) = self
            .overrides
            .catalog
            .items
            .iter()
            .find(|record| record.id == id)
        else {
            return;
        };
        if record.enabled {
            self.error = Some(zenclash_i18n::text(
                "overrides.errors.disable_before_delete",
            ));
            cx.notify();
            return;
        }
        let Some(store) = self.overrides.store.clone() else {
            return;
        };
        let Some(token) = self.begin_mutation(Page::Override) else {
            return;
        };
        let task = self.runtime.spawn_blocking(move || {
            store.delete(&id)?;
            store.load()
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "overrides.errors.delete_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result.map_err(|error| error.to_string()));
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(catalog) => {
                        this.overrides.catalog = catalog;
                        this.overrides.invalidate_preview();
                        this.invalidate_config_inputs(cx);
                        if this.is_page_task_current(token) {
                            this.notice = Some(zenclash_i18n::text("overrides.notices.deleted"));
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

    fn apply_override_catalog_change(
        &mut self,
        before: YamlOverrideCatalog,
        next: YamlOverrideCatalog,
        success: String,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.overrides.store.clone() else {
            return;
        };
        let Some(token) = self.begin_mutation(Page::Override) else {
            return;
        };
        let controlled = self.controlled_config_store.clone();
        let core_runtime = self.profile_service.clone();
        let core_name = self.core_kind.display_name();
        let next_for_task = next.clone();
        let task = self.runtime.spawn(async move {
            let persist_store = store.clone();
            let expected = before.clone();
            let persisted = next_for_task.clone();
            tokio::task::spawn_blocking(move || {
                persist_store.replace_catalog(&expected, &persisted)
            })
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "overrides.errors.catalog_save_task",
                    &[("error", error.to_string())],
                )
            })?
            .map_err(|error| error.to_string())?;
            let reapplication = core_runtime
                .reapply_with_overrides(controlled, store.enabled_paths(&next_for_task))
                .await;
            if let Err(error) = reapplication {
                let rollback_store = store.clone();
                let expected = next_for_task.clone();
                let rollback = before.clone();
                return match tokio::task::spawn_blocking(move || {
                    rollback_store.replace_catalog(&expected, &rollback)
                })
                .await
                {
                    Ok(Ok(())) => Err(zenclash_i18n::text_with(
                        "overrides.errors.change_rejected_rolled_back",
                        &[("core", core_name.to_owned()), ("error", error.clone())],
                    )),
                    Ok(Err(rollback)) => Err(zenclash_i18n::text_with(
                        "overrides.errors.change_rejected_rollback_failed",
                        &[
                            ("core", core_name.to_owned()),
                            ("error", error.clone()),
                            ("rollback", rollback.to_string()),
                        ],
                    )),
                    Err(rollback) => Err(zenclash_i18n::text_with(
                        "overrides.errors.change_rejected_rollback_task",
                        &[
                            ("core", core_name.to_owned()),
                            ("error", error),
                            ("rollback", rollback.to_string()),
                        ],
                    )),
                };
            }
            reapplication
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "overrides.errors.change_workflow",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(applied) => {
                        this.overrides.catalog = next;
                        this.overrides.invalidate_preview();
                        this.invalidate_config_inputs(cx);
                        if this.is_page_task_current(token) {
                            this.notice = Some(if applied.is_some() {
                                success
                            } else {
                                zenclash_i18n::text("overrides.notices.saved_unapplied")
                            });
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
}
