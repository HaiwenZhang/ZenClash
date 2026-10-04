//! Real OS locks around server dispatch; no installed service or privileged kernel.

use super::*;
use crate::installer::OwnedTestRoot;
use crate::maintenance_lock::MaintenanceLock;
use tokio::io::DuplexStream;

async fn fixture() -> (
    Arc<OwnedTestRoot>,
    Arc<Mutex<State>>,
    DuplexStream,
    tokio::task::JoinHandle<io::Result<()>>,
) {
    let root = Arc::new(OwnedTestRoot::create().unwrap());
    let directory = root.path().join("service");
    root.create_directory(&directory).unwrap();
    let peer = crate::platform::current_identity().unwrap();
    let mut state = super::tests::state_for_test(directory);
    state.fixture_root = Some(root.clone());
    state.runtime_recovered = false;
    state.installation =
        InstalledMetadata::new("aa".repeat(32), "bb".repeat(32), peer.user().to_owned()).unwrap();
    let shared = Arc::new(Mutex::new(state));
    let (mut client, server) = tokio::io::duplex(4096);
    let task = tokio::spawn(connection(server, peer, shared.clone()));
    assert!(matches!(
        exchange(
            &mut client,
            Request::Hello {
                protocol_version: PROTOCOL_VERSION
            }
        )
        .await,
        Response::Hello { .. }
    ));
    (root, shared, client, task)
}

async fn exchange(client: &mut DuplexStream, request: Request) -> Response {
    write_frame(client, &request, Duration::from_secs(3))
        .await
        .unwrap();
    read_frame(client, Duration::from_secs(3)).await.unwrap()
}

fn exclusive(root: &OwnedTestRoot) -> io::Result<MaintenanceLock> {
    MaintenanceLock::acquire_with(&root.path().join("service"), false, &|path, directory| {
        root.validate(path, directory)
    })
}

#[tokio::test]
async fn maintenance_acquired_idle_owner_blocks_exclusive_until_release() {
    let (root, shared, mut client, task) = fixture().await;
    let Response::Acquired {
        proof,
        next_sequence,
    } = exchange(&mut client, Request::Acquire {}).await
    else {
        panic!("Acquire did not publish owner");
    };
    assert!(
        matches!(exclusive(&root), Err(error) if error.kind() == io::ErrorKind::WouldBlock),
        "idle owner allowed an exclusive maintenance worker"
    );
    assert!(matches!(
        exchange(
            &mut client,
            Request::Session {
                proof,
                sequence: next_sequence,
                operation: SessionOperation::Release {},
            }
        )
        .await,
        Response::Ok
    ));
    assert!(shared.lock().await.authority.owner().is_none());
    let guard = exclusive(&root).unwrap();
    task.abort();
    let _ = task.await;
    drop(guard);
}

#[tokio::test]
async fn maintenance_worker_excludes_acquire_without_publishing_owner() {
    let (root, shared, mut client, task) = fixture().await;
    let guard = exclusive(&root).unwrap();
    assert!(matches!(
        exchange(&mut client, Request::Acquire {}).await,
        Response::Error {
            code: ServiceErrorCode::MaintenancePending
        }
    ));
    assert!(shared.lock().await.authority.owner().is_none());
    drop(guard);
    assert!(matches!(
        exchange(&mut client, Request::Acquire {}).await,
        Response::Acquired { .. }
    ));
    shared.lock().await.release().await.unwrap();
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn maintenance_idempotent_acquire_and_stop_keep_owner_lock() {
    let (root, shared, mut client, task) = fixture().await;
    let Response::Acquired {
        proof,
        next_sequence,
    } = exchange(&mut client, Request::Acquire {}).await
    else {
        panic!("Acquire did not publish owner");
    };
    assert!(matches!(exchange(&mut client, Request::Acquire {}).await,
        Response::Acquired { proof: repeated, next_sequence: sequence }
            if repeated == proof && sequence == next_sequence));
    shared.lock().await.kernel = Some(Kernel::fixture(Vec::new(), 42));
    assert!(matches!(
        exchange(
            &mut client,
            Request::Session {
                proof,
                sequence: next_sequence,
                operation: SessionOperation::Stop {},
            }
        )
        .await,
        Response::Ok
    ));
    assert!(matches!(exclusive(&root), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    shared.lock().await.release().await.unwrap();
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn maintenance_failed_cleanup_keeps_owner_and_lock_until_retry_succeeds() {
    let (root, shared, mut client, task) = fixture().await;
    assert!(matches!(
        exchange(&mut client, Request::Acquire {}).await,
        Response::Acquired { .. }
    ));
    let directory = root.path().join("service/session");
    std::fs::write(&directory, b"directory replaced").unwrap();
    {
        let mut state = shared.lock().await;
        state.session_directory = Some(directory.clone());
        assert!(state.release().await.is_err());
        assert!(state.authority.owner().is_some());
    }
    assert!(matches!(exclusive(&root), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    std::fs::remove_file(&directory).unwrap();
    root.create_directory(&directory).unwrap();
    shared.lock().await.release().await.unwrap();
    assert!(exclusive(&root).is_ok());
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn maintenance_disconnected_connection_cannot_release_owner_lock() {
    let (root, shared, mut client, task) = fixture().await;
    assert!(matches!(
        exchange(&mut client, Request::Acquire {}).await,
        Response::Acquired { .. }
    ));
    drop(client);
    assert!(task.await.unwrap().is_err());
    assert!(matches!(exclusive(&root), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    shared.lock().await.release().await.unwrap();
    assert!(exclusive(&root).is_ok());
}

#[tokio::test]
async fn maintenance_unreadable_journal_rejects_owner_without_leaking_shared_lock() {
    let (root, shared, mut client, task) = fixture().await;
    std::fs::write(
        root.path().join("service/maintenance.json"),
        b"invalid journal",
    )
    .unwrap();
    assert!(matches!(
        exchange(&mut client, Request::Acquire {}).await,
        Response::Error {
            code: ServiceErrorCode::Internal
        }
    ));
    assert!(shared.lock().await.authority.owner().is_none());
    assert!(exclusive(&root).is_ok());
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn maintenance_cancelled_queued_acquire_has_no_owner_or_shared_lock() {
    let (root, shared, mut client, task) = fixture().await;
    let state = shared.lock().await;
    write_frame(&mut client, &Request::Acquire {}, Duration::from_secs(3))
        .await
        .unwrap();
    tokio::task::yield_now().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(state.authority.owner().is_none());
    assert!(exclusive(&root).is_ok());
}

#[tokio::test]
async fn maintenance_exclusive_startup_answers_hello_but_defers_runtime_recovery() {
    let (root, shared, _idle_client, idle_task) = fixture().await;
    let service = root.path().join("service");
    let stale = service
        .join("runtimes")
        .join(format!("stage-{}", "aa".repeat(32)));
    root.create_directory(&stale).unwrap();
    let guard = exclusive(&root).unwrap();
    let (mut client, server) = tokio::io::duplex(4096);
    let peer = crate::platform::current_identity().unwrap();
    let task = tokio::spawn(connection(server, peer, shared.clone()));
    assert!(matches!(
        exchange(
            &mut client,
            Request::Hello {
                protocol_version: PROTOCOL_VERSION
            }
        )
        .await,
        Response::Hello { .. }
    ));
    assert!(matches!(
        exchange(&mut client, Request::Acquire {}).await,
        Response::Error {
            code: ServiceErrorCode::MaintenancePending
        }
    ));
    assert!(stale.is_dir());
    assert!(!service.join("ipc").exists());
    assert!(!shared.lock().await.runtime_recovered);
    drop(guard);
    assert!(matches!(
        exchange(&mut client, Request::Acquire {}).await,
        Response::Acquired { .. }
    ));
    assert!(!stale.exists());
    assert!(service.join("runtimes").is_dir());
    assert!(service.join("ipc").is_dir());
    assert!(shared.lock().await.runtime_recovered);
    assert!(matches!(exclusive(&root), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    shared.lock().await.release().await.unwrap();
    task.abort();
    idle_task.abort();
    let _ = task.await;
    let _ = idle_task.await;
}

#[tokio::test]
async fn maintenance_failed_startup_recovery_never_publishes_owner_and_can_retry() {
    let (root, shared, mut client, task) = fixture().await;
    let unknown = root.path().join("service/runtimes/unknown");
    root.create_directory(&unknown).unwrap();
    assert!(matches!(
        exchange(&mut client, Request::Acquire {}).await,
        Response::Error {
            code: ServiceErrorCode::Internal
        }
    ));
    {
        let state = shared.lock().await;
        assert!(!state.runtime_recovered);
        assert!(state.authority.owner().is_none());
    }
    assert!(unknown.is_dir());
    assert!(exclusive(&root).is_ok());
    std::fs::remove_dir(&unknown).unwrap();
    assert!(matches!(
        exchange(&mut client, Request::Acquire {}).await,
        Response::Acquired { .. }
    ));
    assert!(shared.lock().await.runtime_recovered);
    shared.lock().await.release().await.unwrap();
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn maintenance_verified_policy_peer_can_hello_but_cannot_acquire_or_stop() {
    // The role input models the native classification at Listener admission.
    // This proves policy and dispatch, not administrator-token authentication.
    let root = Arc::new(OwnedTestRoot::create().unwrap());
    let service = root.path().join("service");
    root.create_directory(&service).unwrap();
    let peer = crate::platform::current_identity().unwrap();
    let mut state = super::tests::state_for_test(service);
    state.fixture_root = Some(root.clone());
    state.installation = InstalledMetadata::new(
        "aa".repeat(32),
        "bb".repeat(32),
        "different-business-user".into(),
    )
    .unwrap();
    state.kernel = Some(Kernel::fixture(Vec::new(), 42));
    assert!(
        !state.accepts_hello_from(&peer, false),
        "ordinary unauthorized peer was admitted"
    );
    assert!(
        state.accepts_hello_from(&peer, true),
        "native-verified maintenance peer was rejected before Hello"
    );
    let shared = Arc::new(Mutex::new(state));
    let (mut client, server) = tokio::io::duplex(4096);
    let task = tokio::spawn(connection(server, peer, shared.clone()));
    assert!(matches!(
        exchange(
            &mut client,
            Request::Hello {
                protocol_version: PROTOCOL_VERSION
            }
        )
        .await,
        Response::Hello { .. }
    ));
    assert!(matches!(
        exchange(&mut client, Request::Acquire {}).await,
        Response::Error {
            code: ServiceErrorCode::Unauthorized
        }
    ));
    assert!(matches!(
        exchange(
            &mut client,
            Request::Session {
                proof: crate::SessionProof::new(1, crate::SessionToken([7; 32])),
                sequence: 1,
                operation: SessionOperation::Stop {},
            }
        )
        .await,
        Response::Error {
            code: ServiceErrorCode::Unauthorized
        }
    ));
    let mut state = shared.lock().await;
    assert!(state.authority.owner().is_none());
    assert!(state.owner_lock.is_none());
    assert!(state.snapshot().unwrap().running);
    drop(state);
    task.abort();
    let _ = task.await;
}
