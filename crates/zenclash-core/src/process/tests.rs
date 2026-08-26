use std::{collections::VecDeque, path::PathBuf, process::Command, sync::Arc};

use parking_lot::{Mutex, RwLock};

use super::*;

#[test]
fn readiness_attempt_timeout_never_exceeds_remaining_deadline() {
    assert_eq!(
        readiness_attempt_timeout(Duration::from_millis(25)),
        Duration::from_millis(25)
    );
}

#[test]
fn readiness_attempt_timeout_caps_long_controller_attempts() {
    assert_eq!(
        readiness_attempt_timeout(Duration::from_secs(5)),
        Duration::from_millis(750)
    );
}

#[test]
fn mihomo_does_not_receive_a_rust_log_override() {
    assert_eq!(core_rust_log_filter(CoreKind::Mihomo, Some("debug")), None);
}

#[test]
fn meow_log_filter_caps_recursive_websocket_frame_logging() {
    let filter = core_rust_log_filter(
        CoreKind::Meow,
        Some("debug,tungstenite=trace,tokio_tungstenite=trace"),
    )
    .unwrap();

    assert!(filter.ends_with("tokio_tungstenite=warn,tungstenite=warn"));
}

#[cfg(unix)]
#[test]
fn exited_process_snapshot_does_not_expose_a_stale_pid() {
    let mut child = Command::new("/usr/bin/true").spawn().unwrap();
    child.wait().unwrap();
    let process = MihomoProcess {
        child: Mutex::new(Some(child)),
        logs: Arc::new(RwLock::new(VecDeque::new())),
        config: MihomoLaunchConfig {
            kind: CoreKind::Mihomo,
            binary: PathBuf::from("/usr/bin/true"),
            config_file: PathBuf::from("profile.yaml"),
            home_dir: PathBuf::new(),
            endpoint: MihomoEndpoint::default(),
            controller_override: None,
        },
    };

    let snapshot = process.snapshot();

    assert_eq!((snapshot.running, snapshot.pid), (false, None));
    assert_eq!(snapshot.kind, CoreKind::Mihomo);
}

#[cfg(unix)]
#[test]
fn restart_replaces_the_child_and_preserves_process_owner() {
    use std::os::unix::fs::PermissionsExt;

    let directory = std::env::temp_dir().join(format!(
        "zenclash-process-restart-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let binary = directory.join("mihomo");
    std::fs::write(&binary, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let process = MihomoProcess::spawn(MihomoLaunchConfig {
        kind: CoreKind::Mihomo,
        binary,
        config_file: directory.join("profile.yaml"),
        home_dir: directory.join("data"),
        endpoint: MihomoEndpoint::default(),
        controller_override: None,
    })
    .unwrap();
    let first_pid = process.snapshot().pid.unwrap();

    process.restart().unwrap();
    let second_pid = process.snapshot().pid.unwrap();
    process.stop().unwrap();
    std::fs::remove_dir_all(directory).unwrap();

    assert_ne!(first_pid, second_pid);
}
