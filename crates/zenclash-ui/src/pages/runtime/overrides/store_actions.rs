use std::path::PathBuf;

use super::super::{Context, Page, RuntimePage, YamlOverrideCatalog};
use crate::application_commands::{
    OverrideApplicationFailure, OverrideApplicationReceipt, OverrideFailureKind,
};

impl RuntimePage {
    pub(in crate::pages::runtime) fn enabled_override_paths(&self) -> Vec<PathBuf> {
        self.overrides.enabled_paths()
    }

    pub(super) fn import_override_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(store) = self.overrides.store().cloned() else {
            self.error = Some(zenclash_i18n::text("overrides.errors.store_unavailable"));
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(Page::Override) else {
            return;
        };
        let application = self
            .application_commands
            .override_application(store, self.controlled_config_store.clone());
        let task = self
            .runtime
            .spawn(async move { application.import_paths(paths).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(receipt)) => {
                        let imported_count = receipt.imported_count();
                        let applied = receipt.runtime_outcome().is_some();
                        this.accept_override_receipt(receipt, cx);
                        if this.is_page_task_current(token) {
                            this.notice = Some(if applied {
                                zenclash_i18n::text_with(
                                    "overrides.notices.imported",
                                    &[("count", imported_count.to_string())],
                                )
                            } else {
                                zenclash_i18n::text("overrides.notices.saved_unapplied")
                            });
                        }
                    }
                    Ok(Err(failure)) => this.accept_override_failure(failure, token, cx),
                    Err(error) => this.set_page_error(token, error.to_string()),
                }
                this.finish_mutation(token);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn set_override_enabled(&mut self, id: &str, enabled: bool, cx: &mut Context<Self>) {
        let before = self.overrides.catalog().clone();
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
        let before = self.overrides.catalog().clone();
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
        let Some(store) = self.overrides.store().cloned() else {
            return;
        };
        let Some(token) = self.begin_mutation(Page::Override) else {
            return;
        };
        let application = self
            .application_commands
            .override_application(store, self.controlled_config_store.clone());
        let task = self
            .runtime
            .spawn(async move { application.delete_disabled(id).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(receipt)) => {
                        this.accept_override_receipt(receipt, cx);
                        if this.is_page_task_current(token) {
                            this.notice = Some(zenclash_i18n::text("overrides.notices.deleted"));
                        }
                    }
                    Ok(Err(failure)) => this.accept_override_failure(failure, token, cx),
                    Err(error) => this.set_page_error(token, error.to_string()),
                }
                this.finish_mutation(token);
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
        let Some(store) = self.overrides.store().cloned() else {
            return;
        };
        let Some(token) = self.begin_mutation(Page::Override) else {
            return;
        };
        let application = self
            .application_commands
            .override_application(store, self.controlled_config_store.clone());
        let task = self
            .runtime
            .spawn(async move { application.replace_catalog(before, next).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(receipt)) => {
                        let applied = receipt.runtime_outcome().is_some();
                        this.accept_override_receipt(receipt, cx);
                        if this.is_page_task_current(token) {
                            this.notice = Some(if applied {
                                success
                            } else {
                                zenclash_i18n::text("overrides.notices.saved_unapplied")
                            });
                        }
                    }
                    Ok(Err(failure)) => this.accept_override_failure(failure, token, cx),
                    Err(error) => this.set_page_error(token, error.to_string()),
                }
                this.finish_mutation(token);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn accept_override_receipt(
        &mut self,
        receipt: OverrideApplicationReceipt,
        cx: &mut Context<Self>,
    ) {
        self.overrides.adopt_receipt(receipt);
        self.invalidate_config_inputs(cx);
    }

    pub(super) fn accept_override_failure(
        &mut self,
        failure: OverrideApplicationFailure,
        token: super::super::PageTaskToken,
        cx: &mut Context<Self>,
    ) {
        let feedback = self.overrides.adopt_failure(failure);
        if feedback.kind != OverrideFailureKind::Rejected {
            self.invalidate_config_inputs(cx);
        }
        self.set_page_error(token, feedback.message);
    }
}
