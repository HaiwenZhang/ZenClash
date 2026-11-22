//! Local managed Mihomo binary validation, child process management, and official release updates.

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
