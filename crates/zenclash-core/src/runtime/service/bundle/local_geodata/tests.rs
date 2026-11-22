use super::*;
use std::fs;

#[test]
fn geodata_previous_file_respects_remaining_backup_budget() {
    let home = super::super::test_support::TestHome::new();
    let path = home.0.join("GeoIP.dat");
    fs::write(&path, [1, 2]).unwrap();
    assert!(read_previous(&path, 1).is_err());
    assert_eq!(read_previous(&path, 2).unwrap(), Some(vec![1, 2]));
    fs::write(&path, []).unwrap();
    assert_eq!(read_previous(&path, 0).unwrap(), Some(vec![]));
}

fn bundle(files: &[(&str, &[u8])]) -> Arc<ServiceRuntimeBundle> {
    Arc::new(ServiceRuntimeBundle {
        yaml: "mode: rule\n".into(),
        assets: files
            .iter()
            .map(|(name, bytes)| RuntimeAsset {
                path: format!("assets/geodata/{name}"),
                bytes: Arc::from(*bytes),
            })
            .collect(),
    })
}

#[tokio::test]
async fn geodata_recovery_rejects_uncovered_home_without_panicking_or_writing() {
    let home = super::super::test_support::TestHome::new();
    let ordinary_home = home.0.join("ordinary");
    fs::create_dir(&ordinary_home).unwrap();
    fs::write(ordinary_home.join("GeoIP.dat"), b"original").unwrap();
    let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
    let lease = store.acquire_write_lease().await.unwrap();
    let mutation = store.lock_service_tun_mutation().await;
    let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = called.clone();
    let result = bundle(&[("GeoIP.dat", b"replacement")])
        .with_local_geodata_admitted(
            &store,
            ordinary_home.clone(),
            lease,
            mutation,
            move |_| async move {
                observed.store(true, std::sync::atomic::Ordering::Release);
                Ok(())
            },
        )
        .await;
    assert!(result.is_err());
    assert!(!called.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(
        fs::read(ordinary_home.join("GeoIP.dat")).unwrap(),
        b"original"
    );
}

#[tokio::test]
async fn geodata_recovery_published_stopped_owner_starts_and_session_shutdown_reaps_it() {
    let fixture = crate::test_support::session::OwnedCoreFixture::new("geodata-new-owner").await;
    fixture.process.stop_async().await.unwrap();
    let process =
        crate::ManagedCoreProcess::prepare_stopped(fixture.process.launch_config().clone());
    assert!(process.snapshot().pid.is_none());
    let client = crate::MihomoClient::from_process(fixture.process.clone()).unwrap();
    let session = crate::CoreSession::open(crate::CoreKind::Mihomo, client.clone()).unwrap();
    let mutation = client.lock_runtime_binding().await.unwrap();
    let mutation = client
        .publish_prepared_process(process.clone(), mutation)
        .await
        .unwrap();
    let home = process.launch_config().home_dir.clone();
    let store = crate::ControlledConfigStore::new(home.parent().unwrap());
    let next = process.clone();
    bundle(&[])
        .with_local_geodata(&store, home, move |recovery| async move {
            let _mutation = mutation;
            recovery
                .restart_local_process(&next, std::time::Duration::from_secs(2), None)
                .await
        })
        .await
        .unwrap();
    assert!(process.snapshot().pid.is_some());
    assert!(fixture.process.snapshot().pid.is_none());
    assert_eq!(
        client.runtime_descriptor().binary(),
        Some(process.launch_config().binary.as_path())
    );
    session.shutdown().await.unwrap();
    assert!(process.snapshot().pid.is_none());
}

#[tokio::test]
async fn geodata_recovery_published_stopped_owner_failure_keeps_owner_and_rolls_back_files() {
    let fixture =
        crate::test_support::session::OwnedCoreFixture::new("geodata-new-owner-failure").await;
    fixture.process.stop_async().await.unwrap();
    let mut launch = fixture.process.launch_config().clone();
    launch.binary = launch.home_dir.join("missing-mihomo");
    let process = crate::ManagedCoreProcess::prepare_stopped(launch);
    let client = crate::MihomoClient::from_process(fixture.process.clone()).unwrap();
    let session = crate::CoreSession::open(crate::CoreKind::Mihomo, client.clone()).unwrap();
    let mutation = client.lock_runtime_binding().await.unwrap();
    let mutation = client
        .publish_prepared_process(process.clone(), mutation)
        .await
        .unwrap();
    let home = process.launch_config().home_dir.clone();
    fs::write(home.join("GeoIP.dat"), b"old geoip").unwrap();
    let store = crate::ControlledConfigStore::new(home.parent().unwrap());
    let next = process.clone();
    assert!(
        bundle(&[("GeoIP.dat", b"new geoip")])
            .with_local_geodata(&store, home.clone(), move |recovery| async move {
                let _mutation = mutation;
                recovery
                    .restart_local_process(&next, std::time::Duration::from_millis(200), None)
                    .await
            })
            .await
            .is_err()
    );
    assert!(process.snapshot().pid.is_none());
    assert_eq!(fs::read(home.join("GeoIP.dat")).unwrap(), b"old geoip");
    assert_eq!(
        client.runtime_descriptor().binary(),
        Some(process.launch_config().binary.as_path())
    );
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn geodata_recovery_expired_authority_cannot_start_a_child_or_acquire_a_new_lease() {
    let fixture =
        crate::test_support::session::OwnedCoreFixture::new("geodata-expired-authority").await;
    fixture.process.stop_async().await.unwrap();
    let home = fixture.process.launch_config().home_dir.clone();
    let store = crate::ControlledConfigStore::new(home.parent().unwrap());
    let recovery = bundle(&[])
        .with_local_geodata(&store, home, |recovery| async { Ok(recovery) })
        .await
        .unwrap();
    assert!(
        recovery
            .restart_local_process(
                &fixture.process,
                std::time::Duration::from_millis(200),
                None
            )
            .await
            .is_err()
    );
    assert!(fixture.process.snapshot().pid.is_none());
    fs::write(store.runtime_path(), "tun:\n  enable: true\n").unwrap();
    let previous_cache = fs::read(store.runtime_path()).unwrap();
    let previous_patch = fs::read(store.root().join("override.yaml")).ok();
    assert!(
        recovery
            .persist_local_payload("tun:\n  enable: false\n".to_owned())
            .await
            .is_err()
    );
    assert_eq!(fs::read(store.runtime_path()).unwrap(), previous_cache);
    assert_eq!(
        fs::read(store.root().join("override.yaml")).ok(),
        previous_patch
    );
}

#[tokio::test]
async fn geodata_recovery_enabled_tun_rejects_start_and_restores_named_files() {
    let fixture =
        crate::test_support::session::OwnedCoreFixture::new("geodata-tun-rejection").await;
    fixture.process.stop_async().await.unwrap();
    let home = fixture.process.launch_config().home_dir.clone();
    fs::write(home.join("GeoIP.dat"), b"old geoip").unwrap();
    fs::write(
        &fixture.process.launch_config().config_file,
        "tun:\n  enable: true\n",
    )
    .unwrap();
    let store = crate::ControlledConfigStore::new(home.parent().unwrap());
    let process = fixture.process.clone();
    assert!(
        bundle(&[("GeoIP.dat", b"new geoip")])
            .with_local_geodata(&store, home.clone(), move |recovery| async move {
                recovery
                    .restart_local_process(&process, std::time::Duration::from_millis(200), None)
                    .await
            })
            .await
            .is_err()
    );
    assert!(fixture.process.snapshot().pid.is_none());
    assert_eq!(fs::read(home.join("GeoIP.dat")).unwrap(), b"old geoip");
}

#[tokio::test]
async fn geodata_recovery_restart_does_not_deadlock_behind_waiting_restore() {
    use std::{
        sync::atomic::{AtomicBool, Ordering},
        time::Duration,
    };
    let fixture =
        crate::test_support::session::OwnedCoreFixture::new("geodata-restart-authority").await;
    fixture.process.stop_async().await.unwrap();
    let home = fixture.process.launch_config().home_dir.clone();
    fs::write(home.join("GeoIP.dat"), b"old geoip").unwrap();
    let store = crate::ControlledConfigStore::new(home.parent().unwrap());
    let process = fixture.process.clone();
    let restarted = Arc::new(AtomicBool::new(false));
    let observed = restarted.clone();
    let restore_home = home.clone();
    let (restore_sender, restore_receiver) = tokio::sync::oneshot::channel();
    let result = bundle(&[("GeoIP.dat", b"new geoip")])
        .with_local_geodata(&store, home.clone(), move |recovery| async move {
            let restore_path = restore_home.clone();
            let restore = tokio::task::spawn_blocking(move || {
                crate::platform::data_coordinator::DataWriteLease::exclusive([restore_path])
            });
            restore_sender.send(restore).unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                while !crate::platform::data_coordinator::has_waiting_restore(&restore_home) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let start = tokio::time::timeout(
                Duration::from_secs(2),
                recovery.restart_local_process(&process, Duration::from_secs(1), None),
            )
            .await;
            observed.store(matches!(start, Ok(Ok(()))), Ordering::SeqCst);
            tokio::time::timeout(
                Duration::from_secs(2),
                recovery.persist_local_payload("tun:\n  enable: false\n".to_owned()),
            )
            .await
            .expect("local persistence queued behind its own waiting restore")
            .unwrap();
            process.stop_async().await.unwrap();
            Err::<(), _>(invalid("Local recovery rejected after confirmed stop"))
        })
        .await;
    assert!(result.is_err());
    let restore = restore_receiver.await.unwrap();
    let lease = tokio::time::timeout(Duration::from_secs(3), restore)
        .await
        .unwrap()
        .unwrap();
    drop(lease);
    assert_eq!(fs::read(home.join("GeoIP.dat")).unwrap(), b"old geoip");
    assert!(
        restarted.load(Ordering::SeqCst),
        "local restart queued behind a restore waiting for its own outer lease"
    );
}

#[tokio::test]
async fn geodata_recovery_success_preserves_mmdb_selection_and_unrelated_files() {
    let home = super::super::test_support::TestHome::new();
    fs::write(home.0.join("geoip.metadb"), b"old selected").unwrap();
    fs::write(home.0.join("unrelated"), b"untouched").unwrap();
    let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
    let selected = home.0.join("geoip.metadb");
    bundle(&[("country.mmdb", b"new selected"), ("ASN.mmdb", b"new asn")])
        .with_local_geodata(&store, home.0.clone(), move |_| async move {
            assert_eq!(fs::read(selected).unwrap(), b"new selected");
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        fs::read(home.0.join("geoip.metadb")).unwrap(),
        b"new selected"
    );
    assert!(!home.0.join("country.mmdb").exists());
    assert_eq!(fs::read(home.0.join("ASN.mmdb")).unwrap(), b"new asn");
    assert_eq!(fs::read(home.0.join("unrelated")).unwrap(), b"untouched");
}

#[tokio::test]
async fn geodata_recovery_failure_restores_prior_bytes_and_prior_absence() {
    let home = super::super::test_support::TestHome::new();
    fs::write(home.0.join("GeoIP.dat"), b"old geoip").unwrap();
    let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
    let result = bundle(&[("GeoIP.dat", b"new geoip"), ("geosite.dat", b"new sites")])
        .with_local_geodata(&store, home.0.clone(), |_| async {
            Err::<(), _>(invalid("Local start rejected"))
        })
        .await;
    assert!(result.is_err());
    assert_eq!(fs::read(home.0.join("GeoIP.dat")).unwrap(), b"old geoip");
    assert!(!home.0.join("geosite.dat").exists());
}

#[tokio::test]
async fn geodata_recovery_preflight_rejects_bad_later_destination_before_any_write() {
    let home = super::super::test_support::TestHome::new();
    fs::write(home.0.join("GeoIP.dat"), b"old geoip").unwrap();
    fs::create_dir(home.0.join("ASN.mmdb")).unwrap();
    let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
    assert!(
        bundle(&[("GeoIP.dat", b"new geoip"), ("ASN.mmdb", b"new asn")])
            .with_local_geodata::<(), _, _>(&store, home.0.clone(), |_| async {
                panic!("invalid preflight must not start completion")
            })
            .await
            .is_err()
    );
    assert_eq!(fs::read(home.0.join("GeoIP.dat")).unwrap(), b"old geoip");
}

#[tokio::test]
async fn geodata_recovery_cancelled_waiter_retains_gate_until_completion_and_rollback() {
    let home = super::super::test_support::TestHome::new();
    fs::write(home.0.join("GeoIP.dat"), b"old geoip").unwrap();
    let store = crate::ControlledConfigStore::new(home.0.join("controlled"));
    let owned_store = store.clone();
    let path = home.0.clone();
    let (entered, entering) = tokio::sync::oneshot::channel();
    let (resume, resumed) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn(async move {
        bundle(&[("GeoIP.dat", b"new geoip")])
            .with_local_geodata(&owned_store, path, move |_| async move {
                entered.send(()).unwrap();
                resumed.await.unwrap();
                Err::<(), _>(invalid("Local start rejected"))
            })
            .await
    });
    entering.await.unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert_eq!(fs::read(home.0.join("GeoIP.dat")).unwrap(), b"new geoip");
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(30),
            store.lock_service_tun_mutation()
        )
        .await
        .is_err()
    );
    resume.send(()).unwrap();
    let _gate = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        store.lock_service_tun_mutation(),
    )
    .await
    .unwrap();
    assert_eq!(fs::read(home.0.join("GeoIP.dat")).unwrap(), b"old geoip");
}
