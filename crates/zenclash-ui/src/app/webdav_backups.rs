use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::Context;
use zenclash_core::{
    BackupManager, WebDavBackup, WebDavService, WebDavSettings, WebDavSettingsStore,
    WebDavUploadSummary,
};

use super::ZenClashApp;

const WEBDAV_SCHEDULE_SCAN_INTERVAL: Duration = Duration::from_secs(30);

impl ZenClashApp {
    pub(super) fn start_webdav_backups(&mut self, cx: &mut Context<Self>) {
        let runtime = self.runtime.clone();
        cx.spawn(async move |this, cx| {
            let mut scheduled_settings: Option<WebDavSettings> = None;
            let mut next_run = None;
            loop {
                let now = unix_time();
                let load = runtime.spawn_blocking(load_webdav_settings).await;
                match load {
                    Ok(Ok(settings)) => {
                        if scheduled_settings.as_ref() != Some(&settings) {
                            next_run = match settings.next_backup_after(now) {
                                Ok(next) => next,
                                Err(error) => {
                                    tracing::warn!(%error, "invalid WebDAV backup schedule");
                                    None
                                }
                            };
                            scheduled_settings = Some(settings.clone());
                        }
                        if next_run.is_some_and(|scheduled| scheduled <= now) {
                            let result = run_scheduled_backup(&runtime, settings.clone()).await;
                            let completed_at = unix_time();
                            next_run = settings
                                .next_backup_after(completed_at)
                                .inspect_err(|error| {
                                    tracing::warn!(%error, "failed to calculate next WebDAV backup");
                                })
                                .unwrap_or(None);
                            if this
                                .update(cx, |this, cx| match result {
                                    Ok((summary, backups)) => {
                                        this.runtime_page.update(cx, |page, cx| {
                                            page.webdav_backup_completed_in_background(
                                                &summary, backups, cx,
                                            );
                                        });
                                    }
                                    Err(error) => {
                                        tracing::warn!(%error, "scheduled WebDAV backup failed");
                                        this.runtime_page.update(cx, |page, cx| {
                                            page.report_background_webdav_error(&error, cx);
                                        });
                                    }
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "failed to load WebDAV schedule settings");
                    }
                    Err(error) => {
                        tracing::warn!(%error, "WebDAV schedule settings task failed");
                    }
                }
                tokio::time::sleep(WEBDAV_SCHEDULE_SCAN_INTERVAL).await;
            }
        })
        .detach();
    }
}

fn load_webdav_settings() -> Result<WebDavSettings, String> {
    let store = WebDavSettingsStore::discover().map_err(|error| error.to_string())?;
    store.load().map_err(|error| error.to_string())
}

async fn run_scheduled_backup(
    runtime: &tokio::runtime::Handle,
    settings: WebDavSettings,
) -> Result<(WebDavUploadSummary, Vec<WebDavBackup>), String> {
    let service = WebDavService::new(settings).map_err(|error| error.to_string())?;
    let manager = runtime
        .spawn_blocking(BackupManager::discover)
        .await
        .map_err(|error| {
            zenclash_i18n::text_with(
                "app.errors.scheduled_backup_directory",
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
    Ok((summary, backups))
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::unix_time;

    #[test]
    fn unix_clock_is_available_for_schedule_comparison() {
        assert!(unix_time() > 1_700_000_000);
    }
}
