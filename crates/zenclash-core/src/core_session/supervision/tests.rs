use super::super::test_support::{
    session_test_root, spawn_test_core, spawn_test_core_with_endpoint, write_test_core_script,
};
use super::*;
use crate::MihomoEndpoint;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

#[cfg(unix)]
use crate::core_session::capture_recovery::CoreRecoveryHookFuture;
#[cfg(unix)]
use std::sync::atomic::AtomicUsize;

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unexpected_exit_retries_are_bounded_and_end_in_visible_failure() {
    let root = session_test_root("bounded-recovery");
    let launches = root.join("launches");
    let binary = root.join("mihomo");
    write_test_core_script(
        &binary,
        &format!(
            "printf x >> '{}'; printf crash >&2; exit 23",
            launches.display()
        ),
    );
    let process = spawn_test_core(&root, binary);
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(process.clone()).unwrap(),
    )
    .unwrap();
    assert!(session.start_supervisor_with_policy(
        &Handle::current(),
        CoreRecoveryPolicy {
            interval: Duration::from_millis(10),
            retry_delay: Duration::from_millis(10),
            ready_timeout: Duration::from_millis(50),
            max_attempts: 2,
        },
        None,
    ));
    assert!(!session.start_supervisor(&Handle::current()));

    tokio::time::timeout(Duration::from_secs(10), async {
        while session.lifecycle_snapshot().phase != CoreLifecyclePhase::Failed {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("supervisor did not reach bounded failure");
    let lifecycle = session.lifecycle_snapshot();
    let launch_count = std::fs::read_to_string(&launches).unwrap().len();

    assert_eq!(lifecycle.recovery_attempts, 2);
    assert!(lifecycle.exit_reason.is_some());
    assert!(lifecycle.last_error.is_some());
    assert_eq!(launch_count, 3, "initial launch plus exactly two retries");
    assert!(!process.is_running());
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_recovery_after_exhaustion_rearms_the_supervisor() {
    let root = session_test_root("rearmed-supervisor");
    let launches = root.join("launches");
    let binary = root.join("mihomo");
    let reservation = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let endpoint = MihomoEndpoint::new(format!("http://{address}"), "");
    write_test_core_script(
        &binary,
        &format!("printf x >> '{}'; exit 23", launches.display()),
    );
    let process = spawn_test_core_with_endpoint(&root, binary.clone(), endpoint.clone());
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(process.clone()).unwrap(),
    )
    .unwrap();
    assert!(session.start_supervisor_with_policy(
        &Handle::current(),
        CoreRecoveryPolicy {
            interval: Duration::from_millis(10),
            retry_delay: Duration::from_millis(10),
            ready_timeout: Duration::from_millis(50),
            max_attempts: 1,
        },
        None,
    ));
    tokio::time::timeout(Duration::from_secs(10), async {
        while session.lifecycle_snapshot().phase != CoreLifecyclePhase::Failed {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("initial recovery did not exhaust its retry");

    write_test_core_script(
        &binary,
        &format!(
            "printf x >> '{}'; trap 'exit 0' TERM; while true; do sleep 0.05; done",
            launches.display()
        ),
    );
    let listener = TcpListener::bind(address).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2_048];
        let _ = stream.read(&mut request).unwrap();
        let body = r#"{"meta":true,"version":"test"}"#;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
    });
    assert_eq!(
        session
            .maintain_with_timeout(CoreMaintenanceIntent::Restart, Duration::from_secs(1))
            .await
            .unwrap(),
        1
    );
    server.join().unwrap();
    assert_eq!(
        session.lifecycle_snapshot().phase,
        CoreLifecyclePhase::Stable
    );

    process.stop_async().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let lifecycle = session.lifecycle_snapshot();
            if lifecycle.phase == CoreLifecyclePhase::Failed && lifecycle.recovery_attempts == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("supervisor was not rearmed after the manual recovery");
    assert!(
        (3..=4).contains(&std::fs::read_to_string(&launches).unwrap().len()),
        "the rearmed retry may be stopped before its script is scheduled"
    );
    session.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successful_crash_recovery_releases_then_reconciles_capture_once() {
    let root = session_test_root("capture-hooks");
    let launches = root.join("launches");
    let crashed = root.join("crashed");
    let binary = root.join("mihomo");
    write_test_core_script(
        &binary,
        &format!(
            "printf x >> '{}'; if [ ! -f '{}' ]; then touch '{}'; exit 23; fi; trap 'exit 0' TERM; while true; do sleep 0.05; done",
            launches.display(),
            crashed.display(),
            crashed.display(),
        ),
    );
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let endpoint = MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2_048];
        let _ = stream.read(&mut request).unwrap();
        let body = r#"{"meta":true,"version":"test"}"#;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
    });
    let process = spawn_test_core_with_endpoint(&root, binary, endpoint.clone());
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(process.clone()).unwrap(),
    )
    .unwrap();
    let capture = Arc::new(RecordingCapture::default());
    assert!(session.start_supervisor_with_policy(
        &Handle::current(),
        CoreRecoveryPolicy {
            interval: Duration::from_millis(10),
            retry_delay: Duration::from_millis(10),
            ready_timeout: Duration::from_secs(1),
            max_attempts: 2,
        },
        Some(capture.clone()),
    ));

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let lifecycle = session.lifecycle_snapshot();
            let hooks_completed = capture.releases.load(Ordering::Acquire) >= 1
                && capture.reconciles.load(Ordering::Acquire) >= 1;
            let launches_completed =
                std::fs::read_to_string(&launches).is_ok_and(|launches| launches.len() >= 2);
            if lifecycle.phase == CoreLifecyclePhase::Stable
                && lifecycle.recovery_attempts == 1
                && hooks_completed
                && launches_completed
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("managed core recovery did not complete its observable effects");

    assert_eq!(capture.releases.load(Ordering::Acquire), 1);
    assert_eq!(capture.reconciles.load(Ordering::Acquire), 1);
    assert_eq!(std::fs::read_to_string(&launches).unwrap().len(), 2);
    assert!(process.is_running());
    session.shutdown().await.unwrap();
    server.join().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[derive(Default)]
struct RecordingCapture {
    releases: AtomicUsize,
    reconciles: AtomicUsize,
}

#[cfg(unix)]
impl CoreRecoveryCapture for RecordingCapture {
    fn release_owned(&self) -> CoreRecoveryHookFuture<'_> {
        self.releases.fetch_add(1, Ordering::AcqRel);
        Box::pin(async { Ok(()) })
    }

    fn reconcile(&self) -> CoreRecoveryHookFuture<'_> {
        self.reconciles.fetch_add(1, Ordering::AcqRel);
        Box::pin(async { Ok(()) })
    }
}
