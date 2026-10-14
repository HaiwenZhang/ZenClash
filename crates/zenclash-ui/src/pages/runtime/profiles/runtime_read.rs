//! Controller read policy owned by this capability.
//!
//! The ordinary page reader supplies the original pinned client and verifies it
//! again after this policy returns. The payload grants no publication eligibility.

use crate::pages::runtime::PageSnapshotData;
use zenclash_core::MihomoClient;

pub(in crate::pages::runtime) async fn load(
    client: MihomoClient,
) -> Result<PageSnapshotData, String> {
    let (config, proxies, rules) = tokio::join!(
        client.runtime_config(),
        client.proxy_catalog(),
        client.rule_catalog()
    );
    Ok(PageSnapshotData::Profile {
        config: config.ok(),
        proxy_count: proxies.as_ref().ok().map(|catalog| catalog.proxy_count),
        group_count: proxies.ok().map(|catalog| catalog.groups().len()),
        rule_count: rules.ok().map(|catalog| catalog.rules.len()),
    })
}
