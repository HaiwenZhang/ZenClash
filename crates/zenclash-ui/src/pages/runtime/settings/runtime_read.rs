//! Runtime configuration and independent local autostart observation.
//!
//! For controller reads, the ordinary page reader supplies the original pinned
//! client and verifies it after this policy returns. The payload grants no
//! publication eligibility. `load_with_config` only reads local autostart facts.

use crate::pages::runtime::PageSnapshotData;
use zenclash_core::MihomoClient;

pub(in crate::pages::runtime) async fn load(
    client: MihomoClient,
) -> Result<PageSnapshotData, String> {
    let (config, autostart) = tokio::join!(client.runtime_config(), load_autostart_status());
    Ok(PageSnapshotData::Settings {
        config: config.ok(),
        autostart,
    })
}

pub(in crate::pages::runtime) async fn load_with_config(
    config: Option<zenclash_core::RuntimeConfig>,
) -> PageSnapshotData {
    PageSnapshotData::Settings {
        config,
        autostart: load_autostart_status().await,
    }
}

async fn load_autostart_status() -> Result<zenclash_core::AutostartStatus, String> {
    tokio::task::spawn_blocking(|| {
        let manager =
            zenclash_core::AutostartManager::discover().map_err(|error| error.to_string())?;
        manager.status().map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| {
        zenclash_i18n::text_with(
            "runtime.load_errors.autostart",
            &[("error", error.to_string())],
        )
    })?
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn controller_failure_does_not_discard_local_settings() {
        let client =
            super::MihomoClient::new(zenclash_core::MihomoEndpoint::new("http://127.0.0.1:1", ""))
                .unwrap();
        let data = super::load(client).await.unwrap();
        assert!(matches!(
            data,
            super::PageSnapshotData::Settings { config: None, .. }
        ));
    }
}
