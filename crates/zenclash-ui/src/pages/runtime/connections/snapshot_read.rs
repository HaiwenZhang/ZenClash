//! Connection snapshots shared by Connections and live Traffic pages.
//!
//! The ordinary page reader supplies the original pinned client and verifies it
//! again after this policy returns. The payload grants no publication eligibility.

use crate::pages::runtime::PageSnapshotData;
use std::sync::Arc;
use zenclash_core::MihomoClient;

pub(in crate::pages::runtime) async fn load(
    client: MihomoClient,
) -> Result<PageSnapshotData, String> {
    client
        .connections_snapshot()
        .await
        .map(|data| PageSnapshotData::Connections(Arc::new(data)))
        .map_err(|error| error.to_string())
}
