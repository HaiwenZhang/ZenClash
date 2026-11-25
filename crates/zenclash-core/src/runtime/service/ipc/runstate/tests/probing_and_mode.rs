use super::super::*;
use super::with_env;

fn ready_reply() -> ServiceVersionReply {
    ServiceVersionReply {
        core: Some(zenclash_service::CoreAvailability::Ready),
        code: 0,
        message: "ok".to_owned(),
        protocol: Some(zenclash_service::ProtocolInfo::current()),
    }
}

#[tokio::test]
async fn probing_a_ready_service_records_it() {
    let store = with_env(FakeEnv::new().service_ready());

    let state = store
        .probe()
        .await
        .expect("a ready service should probe cleanly");

    assert!(state.service_usable());
    assert_eq!(store.state().health, ServiceHealth::Ready);
}

#[tokio::test]
async fn probing_an_incompatible_service_records_a_version_mismatch() {
    let store = with_env(FakeEnv::new().service_version_mismatch());

    let error = store
        .probe()
        .await
        .expect_err("an incompatible service should fail");

    assert!(
        error.to_string().contains("protocol mismatch"),
        "error should explain the mismatch: {error}"
    );
    assert_eq!(store.state().health, ServiceHealth::VersionMismatch);
}

#[tokio::test]
async fn awaiting_readiness_retries_a_silent_service_then_accepts_its_reply() {
    let store = with_env(FakeEnv::new().replying(vec![
        Err("ipc path not ready".to_owned()),
        Ok(ready_reply()),
    ]));

    let state = store
        .await_ready(2, Duration::ZERO)
        .await
        .expect("the second attempt should succeed");

    assert!(state.service_usable());
    assert_eq!(
        store.env.probe_count(),
        2,
        "the first attempt should have been retried"
    );
}

#[tokio::test]
async fn awaiting_readiness_gives_up_after_the_last_attempt() {
    let store = with_env(FakeEnv::new().service_unreachable());
    store.observe(ServiceHealth::Ready);

    let error = store
        .await_ready(3, Duration::ZERO)
        .await
        .expect_err("a silent service should never become ready");

    assert!(matches!(error, ReadyWaitError::Unreachable(_)));
    assert_eq!(
        store.env.probe_count(),
        3,
        "every attempt should have been used"
    );
    assert_eq!(
        store.state().health,
        ServiceHealth::Ready,
        "silence is the caller's to interpret, not an observation"
    );
}

#[tokio::test]
async fn awaiting_readiness_stops_at_the_first_readable_reply() {
    // Retrying an incompatible version would only delay telling the user to reinstall.
    let store = with_env(FakeEnv::new().service_version_mismatch());

    let error = store
        .await_ready(20, Duration::ZERO)
        .await
        .expect_err("an incompatible service is not ready");

    assert!(matches!(error, ReadyWaitError::Rejected(_)));
    assert_eq!(store.env.probe_count(), 1, "a readable reply ends the wait");
    assert_eq!(store.state().health, ServiceHealth::VersionMismatch);
}

#[tokio::test]
async fn awaiting_readiness_with_no_attempts_is_unreachable_not_ready() {
    let store = with_env(FakeEnv::new().service_ready());

    let error = store
        .await_ready(0, Duration::ZERO)
        .await
        .expect_err("no attempts cannot confirm anything");

    assert!(matches!(error, ReadyWaitError::Unreachable(_)));
    assert_eq!(store.env.probe_count(), 0);
}

#[tokio::test]
async fn a_transport_failure_does_not_overwrite_confirmed_health() {
    // A Service that is restarting must not be downgraded to "unavailable" on one
    // missed probe — the last confirmed observation stands until we hear otherwise.
    let store = with_env(FakeEnv::new().service_unreachable());
    store.observe(ServiceHealth::Ready);

    let error = store
        .probe()
        .await
        .expect_err("an unreachable service should fail");

    assert!(
        error.to_string().contains("service readiness probe failed"),
        "error should name the failed probe: {error}"
    );
    assert_eq!(store.state().health, ServiceHealth::Ready);
}

#[tokio::test]
async fn detection_without_platform_evidence_is_not_installed() {
    let store = with_env(FakeEnv::new().with_evidence(false));

    assert_eq!(
        store.detect_service_health().await,
        ServiceHealth::NotInstalled
    );
}

#[tokio::test]
async fn detection_skips_the_probe_when_there_is_no_evidence() {
    let env = FakeEnv::new().with_evidence(false);
    let store = with_env(env);

    store.detect_service_health().await;

    assert_eq!(
        store.env.probe_count(),
        0,
        "no evidence means nothing to probe"
    );
}

#[tokio::test]
async fn detection_reports_a_registered_but_unreachable_service_as_unavailable() {
    let store = with_env(FakeEnv::new().service_unreachable());

    let health = store.detect_service_health().await;

    assert!(
        matches!(health, ServiceHealth::Unavailable(_)),
        "registered but silent is unavailable, not absent: {health:?}"
    );
}

#[tokio::test]
async fn detection_treats_an_uninspectable_registry_as_unavailable() {
    let store = with_env(FakeEnv::new().evidence_unavailable());

    let health = store.detect_service_health().await;

    assert!(
        matches!(health, ServiceHealth::Unavailable(_)),
        "an uninspectable registry must not be reported as absent: {health:?}"
    );
}

#[tokio::test]
async fn detection_reports_a_healthy_service_as_ready() {
    let store = with_env(FakeEnv::new().service_ready());

    assert_eq!(store.detect_service_health().await, ServiceHealth::Ready);
}

#[test]
fn running_mode_round_trips() {
    let store = with_env(FakeEnv::new());
    assert_eq!(*store.mode_arc(), RunningMode::NotRunning);

    store.core_started(RunningMode::Service);

    assert_eq!(*store.mode_arc(), RunningMode::Service);
    assert_eq!(store.state().mode, RunningMode::Service);
}

#[test]
fn starting_the_core_opens_pac_and_mirrors_the_mode() {
    for mode in [RunningMode::Service, RunningMode::Sidecar] {
        let store = with_env(FakeEnv::new());

        store.core_started(mode);

        assert_eq!(
            store.env.pac_available(),
            Some(true),
            "{mode} should open PAC"
        );
        assert_eq!(store.env.published_modes(), vec![mode]);
    }
}

#[test]
fn stopping_the_core_closes_pac_and_mirrors_the_mode() {
    let store = with_env(FakeEnv::new());
    store.core_started(RunningMode::Sidecar);

    store.core_stopped();

    assert_eq!(store.state().mode, RunningMode::NotRunning);
    assert_eq!(store.env.pac_available(), Some(false));
    assert_eq!(
        store.env.published_modes(),
        vec![RunningMode::Sidecar, RunningMode::NotRunning]
    );
}

#[test]
fn a_start_attempt_closes_pac_without_disturbing_the_mode() {
    // A handover must not hand out a PAC script for a proxy port between owners, but the
    // Core we are replacing is still the one that is running.
    let store = with_env(FakeEnv::new());
    store.core_started(RunningMode::Sidecar);

    store.core_starting();

    assert_eq!(store.env.pac_available(), Some(false));
    assert_eq!(store.state().mode, RunningMode::Sidecar);
    assert_eq!(
        store.env.published_modes(),
        vec![RunningMode::Sidecar],
        "a start attempt is not a mode change"
    );
}

#[test]
fn an_abandoned_start_attempt_reopens_pac_for_the_core_that_kept_running() {
    // The regression this pins: a proxy-port candidate rejected before anything was
    // stopped. `core_starting` closed PAC, the Running Mode never moved, and so nothing
    // would ever re-derive PAC — the Core kept serving while its PAC endpoint stayed shut.
    for mode in [RunningMode::Service, RunningMode::Sidecar] {
        let store = with_env(FakeEnv::new());
        store.core_started(mode);
        store.core_starting();
        assert_eq!(store.env.pac_available(), Some(false));

        store.core_start_settled();

        assert_eq!(store.env.pac_available(), Some(true), "{mode} kept serving");
        assert_eq!(
            store.state().mode,
            mode,
            "an abandoned start is not a mode change"
        );
    }
}

#[test]
fn a_settled_start_leaves_pac_shut_when_no_core_is_running() {
    let store = with_env(FakeEnv::new());
    store.core_starting();

    store.core_start_settled();

    assert_eq!(store.env.pac_available(), Some(false));
}

#[test]
fn settling_a_start_is_idempotent_and_never_contradicts_the_mode() {
    // Callers pair this with `core_starting` via a guard that runs on every path, so it
    // also runs after a start that already opened PAC itself.
    let store = with_env(FakeEnv::new());
    store.core_starting();
    store.core_started(RunningMode::Service);

    store.core_start_settled();
    store.core_start_settled();

    assert_eq!(store.env.pac_available(), Some(true));
}

#[test]
fn pac_availability_can_never_disagree_with_the_running_mode() {
    let store = with_env(FakeEnv::new());

    for mode in [
        RunningMode::Sidecar,
        RunningMode::NotRunning,
        RunningMode::Service,
        RunningMode::NotRunning,
    ] {
        store.core_started(mode);

        let running = !matches!(mode, RunningMode::NotRunning);
        assert_eq!(store.env.pac_available(), Some(running), "mode {mode}");
        assert_eq!(store.state().mode, mode);
    }
}

#[test]
fn every_change_is_published_not_just_mode_changes() {
    // The frontend is pushed to rather than polling, so a health change it would otherwise
    // wait up to a poll interval to notice has to be announced too.
    let store = with_env(FakeEnv::new());

    store.observe(ServiceHealth::NotInstalled);
    store.require_install_for_session().expect("absent service");
    store.core_started(RunningMode::Sidecar);

    let published = store.env.published();
    assert_eq!(published.len(), 3, "each change announces exactly once");
    assert_eq!(published[0].health, ServiceHealth::NotInstalled);
    assert_eq!(published[1].pending, Some(PendingAction::Install));
    assert_eq!(published[2].mode, RunningMode::Sidecar);
}

#[test]
fn a_no_op_change_announces_nothing() {
    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::Ready);
    let published = store.env.published().len();

    store.observe(ServiceHealth::Ready);

    assert_eq!(store.env.published().len(), published);
}

#[test]
fn starting_and_finishing_an_operation_are_both_published() {
    // op_in_flight is part of the snapshot, so the frontend must learn when it clears.
    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::Ready);
    let baseline = store.env.published().len();

    let guard = store.begin_operation().expect("slot should be free");
    drop(guard);

    let published = store.env.published();
    assert_eq!(published.len(), baseline + 2);
    assert!(published[baseline].op_in_flight, "the claim is announced");
    assert!(!published[baseline + 1].op_in_flight, "so is the release");
}

#[test]
fn service_health_survives_the_core_stopping() {
    // Stopping the Core says nothing about whether the Service is installed.
    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::Ready);
    store.core_started(RunningMode::Service);

    store.core_stopped();

    assert_eq!(store.state().health, ServiceHealth::Ready);
}

#[test]
fn tun_is_capable_when_elevated_even_with_no_service() {
    let store = with_env(FakeEnv::new().elevated());
    store.observe(ServiceHealth::NotInstalled);

    assert!(store.state().tun_capable());
}

#[test]
fn tun_is_capable_via_a_ready_service_without_elevation() {
    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::Ready);

    assert!(store.state().tun_capable());
}

#[test]
fn tun_is_not_capable_mid_operation_without_elevation() {
    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::Ready);
    let _guard = store.begin_operation().expect("slot should be free");

    assert!(!store.state().tun_capable());
}
