use std::time::Duration;

use gpui::Context;
use zenclash_core::ProfileStore;

use super::ZenClashApp;
use crate::pages::runtime::profiles::workflow;

const PROFILE_UPDATE_SCAN_INTERVAL: Duration = Duration::from_secs(60);

impl ZenClashApp {
    pub(super) fn start_profile_updates(&mut self, cx: &mut Context<Self>) {
        let runtime = self.runtime.clone();
        let controlled = self.controlled_config_store.clone();
        let core_session = self.core_session.clone();
        cx.spawn(async move |this, cx| {
            loop {
                let scan = runtime.spawn_blocking(|| {
                    let store = ProfileStore::discover().map_err(|error| error.to_string())?;
                    let catalog = store.load().map_err(|error| error.to_string())?;
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    Ok::<_, String>((store, catalog.due_profile_ids(now)))
                });
                match scan.await {
                    Ok(Ok((store, profile_ids))) => {
                        for id in profile_ids {
                            let core_runtime =
                                workflow::CoreProfileRuntime::new(core_session.clone());
                            let task = runtime.spawn(workflow::update_remote_background(
                                store.clone(),
                                controlled.clone(),
                                core_runtime,
                                id,
                            ));
                            let result = task
                                .await
                                .map_err(|error| {
                                    zenclash_i18n::text_with(
                                        "app.errors.profile_update_task",
                                        &[("error", error.to_string())],
                                    )
                                })
                                .and_then(|result| result);
                            if this
                                .update(cx, |this, cx| {
                                    match result {
                                        Ok(outcome) => {
                                            this.runtime_page.update(cx, |page, cx| {
                                                page.profile_updated_in_background(outcome, cx);
                                            });
                                        }
                                        Err(error) => {
                                            this.runtime_page.update(cx, |page, cx| {
                                                page.report_background_profile_error(&error, cx);
                                            });
                                        }
                                    }
                                    this.refresh_tray_menu(cx);
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "failed to scan automatic profile updates");
                    }
                    Err(error) => tracing::warn!(%error, "automatic profile scan task failed"),
                }
                tokio::time::sleep(PROFILE_UPDATE_SCAN_INTERVAL).await;
            }
        })
        .detach();
    }
}
