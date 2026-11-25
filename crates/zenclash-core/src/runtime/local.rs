//! Local managed Mihomo binary validation, child process management, and official release updates.

use std::sync::Arc;

pub(crate) mod installation;
pub(crate) mod process;
pub(crate) mod update;

pub use installation::{
    CoreBinaryError, CoreBinaryInfo, validate_core_binary, verify_ordinary_local_executable,
};
pub use process::{
    CoreLaunchConfig, CoreProcessSnapshot, ManagedCoreProcess, MihomoRuntimeResources,
    bundled_default_profile, bundled_recovery_profile,
};
pub use update::{
    CoreUpdateError, CoreUpdateResult, CoreUpdateTransaction, MihomoRelease, MihomoReleaseAsset,
    MihomoReleaseService, PreparedCoreUpdate,
};

use crate::runtime::launcher::{CoreProcessLease, LeftoverLeaseTracker, LocalLauncher};

impl LocalLauncher {
    /// Creates a concrete zero-privilege local launcher backed by
    /// [`ManagedCoreProcess::spawn`] executed on a blocking worker thread.
    #[must_use]
    pub fn managed(leftovers: LeftoverLeaseTracker) -> Self {
        Self::with_blocking(leftovers, |descriptor| {
            let process = ManagedCoreProcess::spawn(CoreLaunchConfig::from(descriptor))?;
            Ok(process as Arc<dyn CoreProcessLease>)
        })
    }
}
