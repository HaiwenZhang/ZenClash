//! Read policy for this capability on the caller-selected runtime.
//!
//! The ordinary page reader supplies the original pinned client and verifies it
//! again after this policy returns. The payload grants no publication eligibility.

use crate::pages::runtime::PageSnapshotData;
use zenclash_core::{MihomoClient, SystemProxyManager};

pub(in crate::pages::runtime) async fn load(
    client: MihomoClient,
) -> Result<PageSnapshotData, String> {
    let status_task = tokio::task::spawn_blocking(|| {
        let manager = SystemProxyManager::detect().map_err(|error| error.to_string())?;
        manager.status().map_err(|error| error.to_string())
    });
    let (config, status) = tokio::join!(client.runtime_config(), status_task);
    let config = config.map_err(|error| error.to_string())?;
    let status = status.map_err(|error| {
        zenclash_i18n::text_with(
            "runtime.load_errors.system_proxy",
            &[("error", error.to_string())],
        )
    })??;
    Ok(PageSnapshotData::SystemProxy { config, status })
}
