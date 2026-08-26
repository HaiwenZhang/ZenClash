use std::path::Path;

use crate::{TunPermissionError, TunPermissionGrant, TunPermissionResult, TunPermissionStatus};

pub(super) fn status(binary: &Path) -> TunPermissionResult<TunPermissionStatus> {
    Ok(TunPermissionStatus {
        granted: false,
        can_request: false,
        requires_relaunch: false,
        binary: binary.to_path_buf(),
        detail: "当前平台没有可用的 TUN 权限安装流程".into(),
    })
}

pub(super) fn request_grant(_binary: &Path) -> TunPermissionResult<TunPermissionGrant> {
    Err(TunPermissionError::Unsupported(
        std::env::consts::OS.to_owned(),
    ))
}
