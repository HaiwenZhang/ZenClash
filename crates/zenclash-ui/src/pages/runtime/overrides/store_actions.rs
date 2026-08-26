use std::path::PathBuf;

use super::super::{Context, Page, RuntimePage, YamlOverrideCatalog};

impl RuntimePage {
    pub(in crate::pages::runtime) fn enabled_override_paths(&self) -> Vec<PathBuf> {
        self.override_store.as_ref().map_or_else(Vec::new, |store| {
            store.enabled_paths(&self.override_catalog)
        })
    }

    pub(super) fn import_override_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(store) = self.override_store.clone() else {
            self.error = Some("YAML 覆写仓库不可用".into());
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(Page::Override) else {
            return;
        };
        let controlled = self.controlled_config_store.clone();
        let client = self.client.clone();
        let profile = self.profile_path.clone();
        let core_runtime = super::super::profiles::workflow::CoreProfileRuntime::new(
            self.core_kind,
            client,
            self.process.clone(),
        );
        let core_name = self.core_kind.display_name();
        let task = self.runtime.spawn(async move {
            let import_store = store.clone();
            let imported = tokio::task::spawn_blocking(move || import_store.import_paths(paths))
                .await
                .map_err(|error| format!("导入 YAML 覆写任务异常结束：{error}"))?
                .map_err(|error| error.to_string())?;
            let catalog_store = store.clone();
            let catalog = tokio::task::spawn_blocking(move || catalog_store.load())
                .await
                .map_err(|error| format!("读取 YAML 覆写清单任务异常结束：{error}"))?
                .map_err(|error| error.to_string())?;
            if let Some(profile) = profile {
                if let Err(error) = super::super::profiles::workflow::reload_effective(
                    controlled,
                    &core_runtime,
                    &profile,
                )
                .await
                {
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
                        Ok(Ok(())) => {
                            Err(format!("{core_name} 拒绝导入的覆写，已移除副本：{error}"))
                        }
                        Ok(Err(cleanup)) => Err(format!(
                            "{core_name} 拒绝导入的覆写：{error}；清理副本失败：{cleanup}"
                        )),
                        Err(cleanup) => Err(format!(
                            "{core_name} 拒绝导入的覆写：{error}；清理任务异常结束：{cleanup}"
                        )),
                    };
                }
            }
            Ok::<_, String>((catalog, imported.len()))
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("YAML 覆写导入工作流异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok((catalog, count)) if this.is_page_task_current(token) => {
                        this.override_catalog = catalog;
                        this.config_preview = None;
                        this.notice = Some(format!("已导入并应用 {count} 份 YAML 覆写"));
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

    pub(super) fn set_override_enabled(&mut self, id: &str, enabled: bool, cx: &mut Context<Self>) {
        let before = self.override_catalog.clone();
        let mut next = before.clone();
        let Some(record) = next.items.iter_mut().find(|record| record.id == id) else {
            self.error = Some("找不到要切换的 YAML 覆写".into());
            cx.notify();
            return;
        };
        record.enabled = enabled;
        self.apply_override_catalog_change(
            before,
            next,
            if enabled {
                "覆写已启用"
            } else {
                "覆写已停用"
            },
            cx,
        );
    }

    pub(super) fn move_override(&mut self, id: &str, offset: isize, cx: &mut Context<Self>) {
        let before = self.override_catalog.clone();
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
        self.apply_override_catalog_change(before, next, "覆写顺序已更新", cx);
    }

    pub(super) fn delete_disabled_override(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(record) = self
            .override_catalog
            .items
            .iter()
            .find(|record| record.id == id)
        else {
            return;
        };
        if record.enabled {
            self.error = Some("请先停用覆写，再删除托管副本".into());
            cx.notify();
            return;
        }
        let Some(store) = self.override_store.clone() else {
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
                .map_err(|error| format!("删除 YAML 覆写任务异常结束：{error}"))
                .and_then(|result| result.map_err(|error| error.to_string()));
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(catalog) if this.is_page_task_current(token) => {
                        this.override_catalog = catalog;
                        this.config_preview = None;
                        this.notice = Some("已删除停用的 YAML 覆写副本".into());
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

    fn apply_override_catalog_change(
        &mut self,
        before: YamlOverrideCatalog,
        next: YamlOverrideCatalog,
        success: &'static str,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.override_store.clone() else {
            return;
        };
        let Some(token) = self.begin_mutation(Page::Override) else {
            return;
        };
        let profile = self.profile_path.clone();
        let controlled = self.controlled_config_store.clone();
        let client = self.client.clone();
        let core_runtime = super::super::profiles::workflow::CoreProfileRuntime::new(
            self.core_kind,
            client,
            self.process.clone(),
        );
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
            .map_err(|error| format!("保存 YAML 覆写清单任务异常结束：{error}"))?
            .map_err(|error| error.to_string())?;
            if let Some(profile) = profile {
                if let Err(error) = super::super::profiles::workflow::reload_effective(
                    controlled,
                    &core_runtime,
                    &profile,
                )
                .await
                {
                    let rollback_store = store.clone();
                    let expected = next_for_task.clone();
                    let rollback = before.clone();
                    return match tokio::task::spawn_blocking(move || {
                        rollback_store.replace_catalog(&expected, &rollback)
                    })
                    .await
                    {
                        Ok(Ok(())) => Err(format!("{core_name} 拒绝覆写变更，清单已恢复：{error}")),
                        Ok(Err(rollback)) => Err(format!(
                            "{core_name} 拒绝覆写变更：{error}；恢复清单失败：{rollback}"
                        )),
                        Err(rollback) => Err(format!(
                            "{core_name} 拒绝覆写变更：{error}；恢复任务异常结束：{rollback}"
                        )),
                    };
                }
            }
            Ok::<_, String>(())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("YAML 覆写变更工作流异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(()) if this.is_page_task_current(token) => {
                        this.override_catalog = next;
                        this.config_preview = None;
                        this.notice = Some(success.into());
                    }
                    Ok(()) => {}
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
