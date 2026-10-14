use super::*;
use crate::service::runstate::{FakeEnv, RunStateStore, ServiceHealth};

#[tokio::test]
async fn residue_inspection_failure_is_published_as_unavailable_without_disabling_startup_tun() {
    let reason = "process inspection permission denied";
    let store = RunStateStore::new(
        FakeEnv::new().with_native_evidence_probe(Ok(false), Err(reason.into())),
    );
    let health = store.observe_current_health().await;
    assert!(matches!(&health,ServiceHealth::Unavailable(message)
            if message.contains("unregistered service residue") && message.contains(reason)));
    assert_eq!(store.state().health, health);
    assert!(!store.state().startup_tun_should_be_disabled(true));
    assert!(!store.state().service_usable());
    assert!(store.state().service_needs_attention());
    assert!(!store.state().op_in_flight);
    assert_eq!(store.env.residue_probe_count(), 1);
    assert_eq!(store.env.probe_count(), 0);
    assert!(store.env.privileged_actions().is_empty());
    assert_eq!(store.env.published().last().unwrap().health, health);
}

#[tokio::test]
async fn confirmed_absence_requires_successful_registration_and_residue_inspection() {
    let store = RunStateStore::new(FakeEnv::new().with_native_evidence_probe(Ok(false), Ok(())));
    assert_eq!(
        store.observe_current_health().await,
        ServiceHealth::NotInstalled
    );
    assert!(store.state().startup_tun_should_be_disabled(true));
    assert_eq!(store.env.residue_probe_count(), 1);
    assert_eq!(store.env.probe_count(), 0);
}

#[tokio::test]
async fn registered_evidence_skips_a_failing_residue_check_and_probes_the_service() {
    let store = RunStateStore::new(
        FakeEnv::new()
            .service_ready()
            .with_native_evidence_probe(Ok(true), Err("must not be inspected".into())),
    );
    assert_eq!(store.observe_current_health().await, ServiceHealth::Ready);
    assert_eq!(store.env.residue_probe_count(), 0);
    assert_eq!(store.env.probe_count(), 1);
}

#[tokio::test]
async fn a_registration_inspection_failure_never_starts_residue_or_version_probes() {
    let store = RunStateStore::new(
        FakeEnv::new()
            .with_native_evidence_probe(Err("registration inspection denied".into()), Ok(())),
    );
    assert!(
        matches!(store.observe_current_health().await,ServiceHealth::Unavailable(reason)
            if reason.contains("registration inspection denied"))
    );
    assert_eq!(store.env.residue_probe_count(), 0);
    assert_eq!(store.env.probe_count(), 0);
    assert!(!store.state().startup_tun_should_be_disabled(true));
}

#[tokio::test]
async fn an_opaque_local_occupancy_error_is_not_proof_of_service_absence() {
    let store = RunStateStore::new(
        FakeEnv::new()
            .with_native_evidence_probe(Ok(false), Err("another core owns execution".into())),
    );
    assert!(
        matches!(store.observe_current_health().await,ServiceHealth::Unavailable(reason)
            if reason.contains("another core owns execution"))
    );
    assert_eq!(store.env.residue_probe_count(), 1);
    assert!(!store.state().startup_tun_should_be_disabled(true));
}

#[tokio::test]
async fn native_evidence_failure_keeps_the_original_io_error_source() {
    let error = trusted_installation_from_native_probes(false, || async {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "native pid inspection denied",
        )
        .into())
    })
    .await
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(format!("{error:#}").contains("native pid inspection denied"));
}
