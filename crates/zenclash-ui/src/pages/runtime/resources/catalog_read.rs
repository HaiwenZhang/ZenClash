//! Controller read policy owned by this capability.
//!
//! The ordinary page reader supplies the original pinned client and verifies it
//! again after this policy returns. The payload grants no publication eligibility.

use crate::pages::runtime::PageSnapshotData;
use zenclash_core::MihomoClient;

pub(in crate::pages::runtime) async fn load(
    client: MihomoClient,
) -> Result<PageSnapshotData, String> {
    let (config, proxy, rules) = tokio::try_join!(
        client.runtime_config(),
        client.proxy_provider_catalog(),
        client.rule_provider_catalog()
    )
    .map_err(|error| error.to_string())?;
    Ok(PageSnapshotData::Resources {
        config,
        proxy,
        rules,
    })
}
