//! Storage/dispatch fixtures only; no real Mihomo or native transport acceptance.

use super::*;
use crate::protocol::{ProviderCacheRead, ProviderKind};

fn fixture(bytes: &[u8]) -> (State, Arc<crate::installer::OwnedTestRoot>, PathBuf) {
    let root = Arc::new(crate::installer::OwnedTestRoot::create().unwrap());
    for directory in [
        "snapshots",
        "assets",
        "configurations",
        "snapshots/revision-1",
        "configurations/revision-1",
    ] {
        root.create_directory(&root.path().join(directory)).unwrap();
    }
    let runtime = StagedRuntime::new_with_configuration(
        root.path().join("snapshots/revision-1"),
        root.path().join("configurations/revision-1"),
        "rule-providers:\n  r:\n    type: http\n    behavior: domain\n    url: https://example.invalid\n",
    ).unwrap();
    let config = runtime
        .materialize("private-controller", "private-secret")
        .unwrap();
    let value: serde_yaml::Value = serde_yaml::from_slice(&std::fs::read(config).unwrap()).unwrap();
    let cache = PathBuf::from(value["rule-providers"]["r"]["path"].as_str().unwrap());
    std::fs::write(&cache, bytes).unwrap();
    let mut state = super::tests::state_for_test(root.path().to_path_buf());
    state.fixture_root = Some(root.clone());
    state.active = Some(Stage {
        revision: 1,
        runtime,
        validated: true,
        patch: None,
    });
    (state, root, cache)
}

fn begin(revision: u64) -> SessionOperation {
    SessionOperation::BeginProviderCacheRead {
        revision,
        kind: ProviderKind::Rule,
        name: "r".into(),
    }
}

fn exhaust_readback(state: &mut State, expired: bool) {
    let started = Instant::now() - Duration::from_secs(if expired { 16 } else { 0 });
    let attempts = if expired { 1 } else { 256 };
    for _ in 0..attempts {
        let snapshot = state.readback.begin(1, Vec::new(), started).unwrap();
        state.readback.finish(&snapshot.token).unwrap();
    }
}

#[tokio::test]
async fn readback_rejected_stale_start_cannot_refresh_exhausted_or_expired_wave() {
    for expired in [false, true] {
        let (mut state, _root, _cache) = fixture(b"preserved cache");
        exhaust_readback(&mut state, expired);
        assert!(matches!(
            state
                .operate(SessionOperation::Start { revision: 999 })
                .await,
            Err(ServiceErrorCode::StaleRevision)
        ));
        assert!(
            matches!(
                state.operate(begin(1)).await,
                Err(ServiceErrorCode::BudgetExceeded)
            ),
            "rejected Start refreshed wave (expired={expired})"
        );
        state.release().await.unwrap();
    }
}

#[tokio::test]
async fn readback_rejected_unvalidated_start_cannot_refresh_exhausted_or_expired_wave() {
    for expired in [false, true] {
        let (mut state, _root, _cache) = fixture(b"preserved cache");
        state.active.as_mut().unwrap().validated = false;
        exhaust_readback(&mut state, expired);
        assert!(matches!(
            state.operate(SessionOperation::Start { revision: 1 }).await,
            Err(ServiceErrorCode::InvalidConfiguration)
        ));
        assert!(
            matches!(
                state.operate(begin(1)).await,
                Err(ServiceErrorCode::BudgetExceeded)
            ),
            "unvalidated Start refreshed wave (expired={expired})"
        );
        state.release().await.unwrap();
    }
}

#[tokio::test]
async fn readback_rejected_maintenance_start_cannot_refresh_exhausted_or_expired_wave() {
    for expired in [false, true] {
        let (mut state, root, _cache) = fixture(b"preserved cache");
        std::fs::write(root.path().join("helper.new"), b"new helper").unwrap();
        std::fs::write(root.path().join("core.new"), b"new core").unwrap();
        let helper_hash = format!("{:x}", Sha256::digest(b"new helper"));
        let metadata = InstalledMetadata::new(
            format!("{:x}", Sha256::digest(b"new core")),
            helper_hash.clone(),
            "test-user".into(),
        )
        .unwrap();
        crate::maintenance_journal::Journal::prepare(root.path(), &metadata, &helper_hash, false)
            .unwrap();
        exhaust_readback(&mut state, expired);
        assert!(matches!(
            state.operate(SessionOperation::Start { revision: 1 }).await,
            Err(ServiceErrorCode::MaintenancePending)
        ));
        std::fs::remove_file(root.path().join("maintenance.json")).unwrap();
        assert!(
            matches!(
                state.operate(begin(1)).await,
                Err(ServiceErrorCode::BudgetExceeded)
            ),
            "maintenance-blocked Start refreshed wave (expired={expired})"
        );
        state.release().await.unwrap();
    }
}

#[tokio::test]
async fn readback_dispatch_requires_native_stop_and_exact_committed_candidate_free_revision() {
    let (mut state, root, cache) = fixture(b"accepted cache");
    state.kernel = Some(Kernel::fixture(Vec::new(), 42));
    assert!(matches!(
        state.operate(begin(1)).await,
        Err(ServiceErrorCode::KernelUnavailable)
    ));
    assert_eq!(std::fs::read(&cache).unwrap(), b"accepted cache");
    state.operate(SessionOperation::Stop {}).await.unwrap();
    assert!(matches!(
        state.operate(begin(2)).await,
        Err(ServiceErrorCode::StaleRevision)
    ));
    state.runtime_unknown = true;
    assert!(matches!(
        state.operate(begin(1)).await,
        Err(ServiceErrorCode::StaleRevision)
    ));
    state.runtime_unknown = false;
    let directory = root.path().join("configurations/revision-2");
    root.create_directory(&directory).unwrap();
    let candidate = state
        .active
        .as_ref()
        .unwrap()
        .runtime
        .fork_patch(directory, &serde_json::json!({"mode":"global"}))
        .unwrap();
    state.staged = Some(Stage {
        revision: 2,
        runtime: candidate,
        validated: true,
        patch: None,
    });
    assert!(matches!(
        state.operate(begin(1)).await,
        Err(ServiceErrorCode::StaleRevision)
    ));
    assert_eq!(std::fs::read(cache).unwrap(), b"accepted cache");
    state.release().await.unwrap();
}

#[tokio::test]
async fn readback_dispatch_pages_immutable_bytes_then_finish_and_release_revoke_the_token() {
    let bytes = vec![255; crate::provider_readback::CHUNK_BYTES + 3];
    let (mut state, _root, cache) = fixture(&bytes);
    let Response::ProviderCacheRead {
        snapshot: ProviderCacheRead::Ready { token, len, sha256 },
    } = state.operate(begin(1)).await.unwrap()
    else {
        panic!("no cache snapshot");
    };
    assert_eq!(len, bytes.len() as u64);
    assert_eq!(sha256, <[u8; 32]>::from(Sha256::digest(&bytes)));
    std::fs::write(cache, b"post-snapshot replacement").unwrap();
    let Response::ProviderCacheChunk { chunk } = state
        .operate(SessionOperation::ReadProviderCache {
            token: token.clone(),
            offset: 0,
        })
        .await
        .unwrap()
    else {
        panic!("no cache chunk");
    };
    assert_eq!(
        chunk.bytes,
        vec![255; crate::provider_readback::CHUNK_BYTES]
    );
    assert!(!chunk.finished);
    assert!(
        state
            .operate(SessionOperation::ReadProviderCache {
                token: token.clone(),
                offset: 0
            })
            .await
            .is_err()
    );
    let Response::ProviderCacheChunk { chunk } = state
        .operate(SessionOperation::ReadProviderCache {
            token: token.clone(),
            offset: crate::provider_readback::CHUNK_BYTES as u64,
        })
        .await
        .unwrap()
    else {
        panic!("no final chunk");
    };
    assert_eq!(chunk.bytes, [255; 3]);
    assert!(chunk.finished);
    state
        .operate(SessionOperation::FinishProviderCacheRead {
            token: token.clone(),
        })
        .await
        .unwrap();
    assert!(
        state
            .operate(SessionOperation::ReadProviderCache { token, offset: len })
            .await
            .is_err()
    );
    let Response::ProviderCacheRead {
        snapshot: ProviderCacheRead::Ready { token, .. },
    } = state.operate(begin(1)).await.unwrap()
    else {
        panic!("no second snapshot");
    };
    state.release().await.unwrap();
    assert!(
        state
            .operate(SessionOperation::ReadProviderCache { token, offset: 0 })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn readback_dispatch_declared_absence_is_distinct_from_invalid_provider_or_budget() {
    let (mut state, _root, cache) = fixture(b"cache");
    std::fs::remove_file(cache).unwrap();
    assert!(matches!(
        state.operate(begin(1)).await.unwrap(),
        Response::ProviderCacheRead {
            snapshot: ProviderCacheRead::Absent
        }
    ));
    assert!(matches!(
        state
            .operate(SessionOperation::BeginProviderCacheRead {
                revision: 1,
                kind: ProviderKind::Rule,
                name: "../../private-controller".into(),
            })
            .await,
        Err(ServiceErrorCode::InvalidAsset)
    ));
    assert!(matches!(
        state
            .operate(SessionOperation::BeginProviderCacheRead {
                revision: 1,
                kind: ProviderKind::Proxy,
                name: "r".into(),
            })
            .await,
        Err(ServiceErrorCode::InvalidAsset)
    ));
    state.release().await.unwrap();
}

#[tokio::test]
async fn readback_cancelled_connection_keeps_state_gate_until_its_copy_worker_finishes() {
    let (mut state, _root, _cache) = fixture(b"immutable snapshot");
    let peer = crate::platform::current_identity().unwrap();
    state.installation =
        InstalledMetadata::new("aa".repeat(32), "bb".repeat(32), peer.user().to_owned()).unwrap();
    let proof = state
        .authority
        .acquire(peer.clone(), Instant::now())
        .unwrap();
    let (gate, entered, release) = crate::runtime::ReadbackCopyGate::new();
    state
        .active
        .as_mut()
        .unwrap()
        .runtime
        .set_readback_gate(gate);
    let shared = Arc::new(Mutex::new(state));
    let (mut client, server) = tokio::io::duplex(4096);
    let serving = tokio::spawn(connection(server, peer, shared.clone()));
    write_frame(
        &mut client,
        &Request::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert!(matches!(
        read_frame::<_, Response>(&mut client, Duration::from_secs(3))
            .await
            .unwrap(),
        Response::Hello { .. }
    ));
    write_frame(
        &mut client,
        &Request::Session {
            proof,
            sequence: 1,
            operation: begin(1),
        },
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), entered)
        .await
        .unwrap()
        .unwrap();
    serving.abort();
    assert!(serving.await.unwrap_err().is_cancelled());
    assert!(
        shared.try_lock().is_err(),
        "cancelled waiter released state while its file worker was live"
    );
    release.send(()).unwrap();
    let mut state = tokio::time::timeout(Duration::from_secs(3), shared.lock())
        .await
        .unwrap();
    // The reply was lost, but its sole snapshot remains bounded by the original deadline.
    state
        .readback
        .expire(1, Instant::now() + Duration::from_secs(16));
    assert!(
        state
            .readback
            .reserve(1, Instant::now() + Duration::from_secs(16))
            .is_err()
    );
    state.release().await.unwrap();
}

#[tokio::test]
async fn readback_cancelled_queued_connection_has_no_admission_or_copy_effects() {
    let (mut state, _root, _cache) = fixture(b"untouched cache");
    let peer = crate::platform::current_identity().unwrap();
    state.installation =
        InstalledMetadata::new("aa".repeat(32), "bb".repeat(32), peer.user().to_owned()).unwrap();
    let proof = state
        .authority
        .acquire(peer.clone(), Instant::now())
        .unwrap();
    let (gate, mut entered, _release) = crate::runtime::ReadbackCopyGate::new();
    state
        .active
        .as_mut()
        .unwrap()
        .runtime
        .set_readback_gate(gate);
    let shared = Arc::new(Mutex::new(state));
    let (mut client, server) = tokio::io::duplex(4096);
    let serving = tokio::spawn(connection(server, peer, shared.clone()));
    write_frame(
        &mut client,
        &Request::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    let _: Response = read_frame(&mut client, Duration::from_secs(3))
        .await
        .unwrap();
    let mut held = shared.lock().await;
    write_frame(
        &mut client,
        &Request::Session {
            proof,
            sequence: 1,
            operation: begin(1),
        },
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    tokio::task::yield_now().await;
    serving.abort();
    assert!(serving.await.unwrap_err().is_cancelled());
    assert_eq!(held.authority.next_sequence(), Some(1));
    assert!(matches!(
        entered.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    held.release().await.unwrap();
}
