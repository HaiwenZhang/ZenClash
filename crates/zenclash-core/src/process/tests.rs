use std::collections::VecDeque;
#[cfg(unix)]
use std::{path::PathBuf, process::Command, sync::Arc};

#[cfg(unix)]
use parking_lot::{Mutex, RwLock};

use super::*;

#[test]
fn controller_listener_conflict_is_detected_without_confusing_proxy_listener_errors() {
    let controller = VecDeque::from([String::from(
        "External controller listen error: listen tcp 127.0.0.1:19191: bind: address already in use",
    )]);
    let proxy = VecDeque::from([String::from(
        "Start Mixed(http+socks) proxy listening at: 127.0.0.1:7890: address already in use",
    )]);

    assert!(controller_listener_error(&controller).is_some());
    assert!(controller_listener_error(&proxy).is_none());
}

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
    std::fs::write(
        &binary,
        "#!/bin/sh\nif [ \"$1\" = '-t' ]; then exit 0; fi\nsleep 30\n",
    )
    .unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(directory.join("profile.yaml"), "rules:\n  - MATCH,DIRECT\n").unwrap();
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

#[cfg(unix)]
#[tokio::test]
async fn async_restart_rejection_does_not_stop_the_running_child() {
    use std::os::unix::fs::PermissionsExt;

    let directory = std::env::temp_dir().join(format!(
        "zenclash-process-restart-validation-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let binary = directory.join("mihomo");
    std::fs::write(
        &binary,
        "#!/bin/sh\nif [ \"$1\" = '-t' ]; then grep -q invalid \"$5\" && exit 1; exit 0; fi\nsleep 30\n",
    )
    .unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let profile = directory.join("profile.yaml");
    std::fs::write(&profile, "rules:\n  - MATCH,DIRECT\n").unwrap();
    let process = MihomoProcess::spawn(MihomoLaunchConfig {
        kind: CoreKind::Mihomo,
        binary,
        config_file: profile.clone(),
        home_dir: directory.join("data"),
        endpoint: MihomoEndpoint::default(),
        controller_override: None,
    })
    .unwrap();
    let first_pid = process.snapshot().pid.unwrap();
    std::fs::write(&profile, "invalid: true\n").unwrap();

    let error = process
        .restart_and_wait(Duration::from_millis(10))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("当前内核保持运行"));
    assert_eq!(process.snapshot().pid, Some(first_pid));
    process.stop().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[test]
fn stop_gives_the_child_a_graceful_sigterm_window() {
    let directory = std::env::temp_dir().join(format!(
        "zenclash-process-graceful-stop-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let marker = directory.join("terminated");
    let ready = directory.join("ready");
    let script = format!(
        "trap 'printf terminated > {} ; exit 0' TERM; printf ready > {}; while true; do sleep 0.05; done",
        marker.display(),
        ready.display()
    );
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .spawn()
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !ready.is_file() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        ready.is_file(),
        "child did not finish installing its signal handler"
    );
    stop_running_child(&mut child).unwrap();

    assert!(marker.is_file(), "child did not handle SIGTERM before exit");
    std::fs::remove_dir_all(directory).unwrap();
}
