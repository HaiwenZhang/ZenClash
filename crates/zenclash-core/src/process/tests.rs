use std::collections::VecDeque;
#[cfg(unix)]
use std::{path::PathBuf, process::Command, sync::Arc};

#[cfg(unix)]
use parking_lot::{Mutex, RwLock};

use super::*;

mod frozen_launch_tests;

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
        drop_gate: Mutex::new(None),
        child: Mutex::new(Some(child)),
        logs: Arc::new(RwLock::new(VecDeque::new())),
        last_exit_reason: RwLock::new(None),
        recovery_asset_root: RwLock::new(None),
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
    assert!(snapshot.exit_reason.is_some());
    assert_eq!(snapshot.kind, CoreKind::Mihomo);
}

#[cfg(unix)]
#[tokio::test]
async fn unobservable_local_restart_rejection_invalidates_the_lifecycle() {
    let directory = std::env::temp_dir().join(format!(
        "zenclash-unobservable-restart-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let config = directory.join("profile.yaml");
    std::fs::write(&config, "tun: {enable: true}\n").unwrap();
    let child = Command::new("/usr/bin/sleep").arg("30").spawn().unwrap();
    let pid = child.id() as libc::pid_t;
    let process = Arc::new(MihomoProcess {
        drop_gate: Mutex::new(None),
        child: Mutex::new(Some(child)),
        logs: Arc::new(RwLock::new(VecDeque::new())),
        last_exit_reason: RwLock::new(None),
        recovery_asset_root: RwLock::new(None),
        config: MihomoLaunchConfig {
            kind: CoreKind::Mihomo,
            binary: PathBuf::from("/usr/bin/sleep"),
            config_file: config,
            home_dir: directory.clone(),
            endpoint: MihomoEndpoint::default(),
            controller_override: None,
        },
    });
    let session = crate::CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(process.clone()).unwrap(),
    )
    .unwrap();
    // SAFETY: this test owns the one positive live child PID and reaps it once.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    let mut status = 0;
    // SAFETY: status is live native storage; waitpid targets only this owned child.
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    assert!(process.child.lock().as_mut().unwrap().try_wait().is_err());
    let result = session
        .maintain(crate::CoreMaintenanceIntent::Restart)
        .await;
    let lifecycle = session.lifecycle_snapshot();
    let generation = session.generation();
    // The fixture was already reaped above; release only its stale test handle.
    process.child.lock().take();
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert!(result.is_err());
    assert_eq!(lifecycle.phase, crate::CoreLifecyclePhase::Unknown);
    assert!(
        generation > 0,
        "failed observation retained a trusted read version"
    );
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readiness_timeout_stops_and_reaps_the_unconfirmed_child() {
    use std::os::unix::fs::PermissionsExt;

    let directory = std::env::temp_dir().join(format!(
        "zenclash-process-ready-timeout-{}-{}",
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
        "#!/bin/sh\nif [ \"$1\" = '-t' ]; then exit 0; fi\ntrap 'exit 0' TERM\nwhile true; do sleep 0.05; done\n",
    )
    .unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let profile = directory.join("profile.yaml");
    std::fs::write(&profile, "rules:\n  - MATCH,DIRECT\n").unwrap();
    let controller = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap();
    let process = MihomoProcess::spawn(MihomoLaunchConfig {
        kind: CoreKind::Mihomo,
        binary,
        config_file: profile,
        home_dir: directory.join("data"),
        endpoint: MihomoEndpoint::with_random_secret(controller.to_string()).unwrap(),
        controller_override: None,
    })
    .unwrap();

    let error = process
        .restart_and_wait(Duration::from_millis(75))
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("超时"));
    assert!(error.contains("已停止未就绪内核"));
    assert!(!process.is_running());
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

#[test]
fn ui_metadata_and_logs_remain_available_while_lifecycle_lock_is_held() {
    let process = MihomoProcess {
        drop_gate: Mutex::new(None),
        child: parking_lot::Mutex::new(None),
        execution: parking_lot::Mutex::new(None),
        isolated_test_child: true,
        logs: std::sync::Arc::new(parking_lot::RwLock::new(VecDeque::from(["ready".into()]))),
        last_exit_reason: parking_lot::RwLock::new(None),
        recovery_asset_root: RwLock::new(None),
        config: MihomoLaunchConfig {
            kind: CoreKind::Mihomo,
            binary: "mihomo".into(),
            config_file: "profile.yaml".into(),
            home_dir: "home".into(),
            endpoint: MihomoEndpoint::default(),
            controller_override: None,
        },
    };
    let process = std::sync::Arc::new(process);
    let session = crate::CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(process.clone()).unwrap(),
    )
    .unwrap();
    let transition = process.child.lock();
    let reader = process.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        sender
            .send((
                reader.launch_config().binary.clone(),
                reader.recent_logs(),
                session.is_managed(),
                session.generation(),
            ))
            .unwrap();
    });
    let result = receiver.recv_timeout(std::time::Duration::from_secs(1));
    drop(transition);
    worker.join().unwrap();
    let (binary, logs, managed, generation) = result.expect("UI read waited on lifecycle lock");
    assert_eq!(binary, std::path::Path::new("mihomo"));
    assert_eq!(logs, ["ready"]);
    assert!(managed);
    assert_eq!(generation, 0);
}

#[tokio::test]
async fn local_tun_restart_rejection_preserves_the_running_child() {
    let fixture =
        crate::core_session::ownership_tests::ChildFixture::new("geodata-local-tun-restart-gate")
            .await;
    let first_pid = fixture.process.snapshot().pid;
    std::fs::write(
        &fixture.process.launch_config().config_file,
        "tun: {enable: true}\n",
    )
    .unwrap();
    let process = fixture.process.clone();
    let result = tokio::task::spawn_blocking(move || process.restart())
        .await
        .unwrap();
    assert!(matches!(result, Err(MihomoError::ServiceRequired)));
    assert_eq!(fixture.process.snapshot().pid, first_pid);
    assert!(fixture.process.snapshot().running);
}

#[test]
fn local_tun_spawn_rejection_does_not_create_the_runtime_home() {
    let root = std::env::temp_dir().join(format!(
        "zenclash-local-tun-spawn-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("source.yaml");
    std::fs::write(&source, "tun: {enable: true}\n").unwrap();
    let home = root.join("home");
    let launch =
        MihomoLaunchConfig::new(root.join("must-not-run"), source.clone(), home.clone()).unwrap();
    let result = MihomoProcess::spawn(launch);
    assert!(matches!(result, Err(MihomoError::ServiceRequired)));
    assert!(!home.exists());
    assert_eq!(
        std::fs::read_to_string(source).unwrap(),
        "tun: {enable: true}\n"
    );
    std::fs::remove_dir_all(root).unwrap();
}
