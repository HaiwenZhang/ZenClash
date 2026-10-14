//! Isolated temporary data and synthetic local children shared by session tests.

use super::*;
#[cfg(unix)]
use crate::{CoreLaunchConfig, MihomoEndpoint};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub(super) fn session_test_root(label: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = loop {
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "zenclash-core-session-{label}-{}-{sequence}",
            std::process::id(),
        ));
        match std::fs::create_dir(&root) {
            Ok(()) => break root,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("cannot create isolated session directory: {error}"),
        }
    };
    std::fs::write(root.join("profile.yaml"), "rules:\n  - MATCH,DIRECT\n").unwrap();
    root
}

#[cfg(unix)]
pub(super) fn write_test_core_script(path: &std::path::Path, run: &str) {
    std::fs::write(
        path,
        format!("#!/bin/sh\nif [ \"$1\" = '-t' ]; then exit 0; fi\n{run}\n"),
    )
    .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
pub(super) fn spawn_test_core(root: &std::path::Path, binary: PathBuf) -> Arc<ManagedCoreProcess> {
    // These children intentionally never expose a ready controller. An isolated
    // socket prevents an unrelated local Mihomo on port 9090 from satisfying readiness.
    spawn_test_core_with_endpoint(root, binary, MihomoEndpoint::local_ipc(root).unwrap())
}

#[cfg(unix)]
pub(super) fn spawn_test_core_with_endpoint(
    root: &std::path::Path,
    binary: PathBuf,
    endpoint: MihomoEndpoint,
) -> Arc<ManagedCoreProcess> {
    ManagedCoreProcess::spawn(CoreLaunchConfig {
        kind: CoreKind::Mihomo,
        binary,
        config_file: root.join("profile.yaml"),
        home_dir: root.join("data"),
        endpoint,
        controller_override: None,
    })
    .unwrap()
}

mod owned_child;
pub(crate) use crate::test_support::{read_controller_request, read_controller_request_blocking};
pub(crate) use owned_child::OwnedCoreFixture;
