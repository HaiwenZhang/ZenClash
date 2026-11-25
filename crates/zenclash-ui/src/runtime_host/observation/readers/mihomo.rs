//! Controller read policy owned by this capability.
//!
//! The ordinary page reader supplies the original pinned client and verifies it
//! again after this policy returns. The payload grants no publication eligibility.

use crate::runtime_host::observation::PageSnapshotData;
use zenclash_core::MihomoClient;

pub(crate) async fn load(client: MihomoClient) -> Result<PageSnapshotData, String> {
    let (version, config) = tokio::try_join!(client.version(), client.runtime_config())
        .map_err(|error| error.to_string())?;
    Ok(PageSnapshotData::Core { version, config })
}
