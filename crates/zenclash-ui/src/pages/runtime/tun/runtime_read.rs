//! Read policy for this capability on the caller-selected runtime.
//!
//! The ordinary page reader supplies the original pinned client and verifies it
//! again after this policy returns. The payload grants no publication eligibility.

use crate::pages::runtime::{PageSnapshotData, page_observation::now_ms};
use zenclash_core::{CoreSession, MihomoClient, Observation, RecoveryAction};

pub(in crate::pages::runtime) async fn load(
    client: MihomoClient,
    core: Option<CoreSession>,
) -> Result<PageSnapshotData, String> {
    let permission_task = async {
        match core {
            Some(core) => core
                .tun_permission_status()
                .await
                .map_err(|error| error.to_string()),
            None => Ok(Observation::Loading),
        }
    };
    let (config, permissions) = tokio::join!(client.runtime_config(), permission_task);
    let config = config.map_err(|error| error.to_string())?;
    let permissions = permissions.unwrap_or_else(|error| {
        Observation::record(
            &Observation::Loading,
            Err::<zenclash_core::CoreTunPermissionStatus, _>(error),
            now_ms(),
            RecoveryAction::Retry,
        )
    });
    Ok(PageSnapshotData::Tun {
        config,
        permissions,
    })
}
