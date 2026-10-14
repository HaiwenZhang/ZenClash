//! Read policy for this capability on the caller-selected runtime.
//!
//! The ordinary page reader supplies the original pinned client and verifies it
//! again after this policy returns. The payload grants no publication eligibility.

use crate::pages::runtime::PageSnapshotData;
use zenclash_core::{MihomoClient, SystemNetworkSnapshot};

pub(in crate::pages::runtime) async fn load(
    client: MihomoClient,
) -> Result<PageSnapshotData, String> {
    let system_task = tokio::task::spawn_blocking(SystemNetworkSnapshot::detect);
    let (config, system) = tokio::join!(client.runtime_config(), system_task);
    let config = config.map_err(|error| error.to_string())?;
    let system = system.map_err(|error| {
        zenclash_i18n::text_with(
            "runtime.load_errors.system_network",
            &[("error", error.to_string())],
        )
    })?;
    Ok(PageSnapshotData::Network { config, system })
}
