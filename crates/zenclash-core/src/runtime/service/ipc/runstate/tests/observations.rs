use super::super::*;
use super::with_env;

#[tokio::test]
async fn a_reading_nothing_overtook_is_recorded() {
    let store = with_env(FakeEnv::service_ready(FakeEnv::new()));

    let health = store.observe_current_health().await;

    assert_eq!(health, ServiceHealth::Ready);
    assert_eq!(store.state().health, ServiceHealth::Ready);
}

#[test]
fn a_reading_overtaken_by_a_changed_observation_is_discarded() {
    // A slower earlier probe must not overwrite a newer Ready observation.
    let store = with_env(FakeEnv::new());
    let reservation = store.reserve_health_observation();
    store.observe(ServiceHealth::Ready);

    let recorded = store.commit_reserved_observation(ServiceHealth::NotInstalled, reservation);

    assert_eq!(
        recorded,
        ServiceHealth::Ready,
        "the newer observation is reported back"
    );
    assert_eq!(
        store.state().health,
        ServiceHealth::Ready,
        "and it is what stands"
    );
}

#[test]
fn a_reading_overtaken_by_an_unchanged_observation_is_also_discarded() {
    // Even an unchanged "still Ready" observation must invalidate an older probe.
    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::Ready);
    let reservation = store.reserve_health_observation();
    let generation = store.generation_count();

    store.observe(ServiceHealth::Ready);
    assert_eq!(
        store.generation_count(),
        generation,
        "nothing visible changed"
    );

    let recorded = store.commit_reserved_observation(ServiceHealth::NotInstalled, reservation);

    assert_eq!(recorded, ServiceHealth::Ready);
    assert_eq!(store.state().health, ServiceHealth::Ready);
}

#[test]
fn a_reading_overtaken_by_an_already_satisfied_sidecar_restore_is_discarded() {
    // Reasserting an existing allowance must stop an older probe from clearing it.
    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::VersionMismatch);
    store.accept_sidecar();
    let reservation = store.reserve_health_observation();

    assert!(
        !store.restore_sidecar_allowance(),
        "the allowance is already there"
    );

    store.commit_reserved_observation(ServiceHealth::VersionMismatch, reservation);

    assert!(
        store.state().sidecar_allowed,
        "the confirmed allowance survives"
    );
    assert!(!store.state().service_needs_attention());
}

#[test]
fn re_requesting_the_same_action_changes_nothing_visible_but_still_orders() {
    // The same request leaves `generation` unchanged but must invalidate an older probe.
    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::NotInstalled);
    store.require_install_for_session().expect("absent service");
    let generation = store.generation_count();
    let reservation = store.reserve_health_observation();

    assert!(
        !store.request_action(PendingAction::Install),
        "no Sidecar allowance was displaced"
    );
    assert_eq!(
        store.generation_count(),
        generation,
        "nothing visible changed"
    );

    store.commit_reserved_observation(ServiceHealth::NotInstalled, reservation);

    assert_eq!(store.state().pending, Some(PendingAction::Install));
}

#[test]
fn a_reading_overtaken_by_an_already_pending_install_is_discarded() {
    // Requiring an existing install request must stop an older probe from clearing it.
    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::NotInstalled);
    store.require_install_for_session().expect("absent service");
    let reservation = store.reserve_health_observation();

    store
        .require_install_for_session()
        .expect("already requested");

    store.commit_reserved_observation(ServiceHealth::NotInstalled, reservation);

    assert_eq!(store.state().pending, Some(PendingAction::Install));
    assert!(store.state().service_needs_attention());
}

#[test]
fn a_later_reading_supersedes_one_that_is_still_running() {
    // The last probe started wins regardless of completion order.
    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::Ready);
    let first = store.reserve_health_observation();
    let second = store.reserve_health_observation();

    assert_eq!(
        store.commit_reserved_observation(ServiceHealth::NotInstalled, first),
        ServiceHealth::Ready,
        "the earlier reading is already superseded"
    );
    assert_eq!(
        store.commit_reserved_observation(ServiceHealth::VersionMismatch, second),
        ServiceHealth::VersionMismatch
    );
    assert_eq!(store.state().health, ServiceHealth::VersionMismatch);
}

#[test]
fn withdrawing_a_sidecar_allowance_says_nothing_about_the_service() {
    // A failed Sidecar startup must not change Service health.
    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::NotInstalled);
    store.accept_sidecar();

    assert!(store.withdraw_sidecar_allowance());

    assert!(!store.state().sidecar_allowed);
    assert_eq!(
        store.state().health,
        ServiceHealth::NotInstalled,
        "health is untouched"
    );
    assert!(
        !store.state().service_needs_attention(),
        "an absent Service still asks nothing"
    );
}

#[test]
fn withdrawing_an_allowance_that_was_never_granted_changes_nothing() {
    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::NotInstalled);
    let generation = store.generation_count();

    assert!(!store.withdraw_sidecar_allowance());

    assert_eq!(store.generation_count(), generation);
}

#[test]
fn a_fresh_store_knows_nothing() {
    let store = with_env(FakeEnv::new());
    let state = store.state();

    assert_eq!(state.health, ServiceHealth::Unknown);
    assert_eq!(state.pending, None);
    assert!(!state.sidecar_allowed);
    assert_eq!(state.mode, RunningMode::NotRunning);
    assert!(!state.is_admin);
    assert!(!state.op_in_flight);
    assert!(!state.service_usable());
}

#[test]
fn elevation_comes_from_the_environment() {
    assert!(with_env(FakeEnv::new().elevated()).state().is_admin);
    assert!(!with_env(FakeEnv::new()).state().is_admin);
}

#[test]
fn repeated_identical_observations_do_not_bump_the_generation() {
    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::Ready);
    let generation = store.generation_count();

    store.observe(ServiceHealth::Ready);

    assert_eq!(store.generation_count(), generation);
}

#[test]
fn observing_clears_a_pending_request() {
    let store = with_env(FakeEnv::new());
    store.request_action(PendingAction::Install);
    store.observe(ServiceHealth::Ready);

    let state = store.state();
    assert_eq!(state.pending, None);
    assert!(state.service_usable());
}

#[test]
fn an_in_flight_operation_makes_a_ready_service_unusable() {
    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::Ready);
    assert!(store.state().service_usable());

    let guard = store.begin_operation().expect("slot should be free");
    assert!(store.state().service_ready());
    assert!(!store.state().service_usable());

    drop(guard);
    assert!(store.state().service_usable());
}

#[test]
fn only_one_privileged_operation_runs_at_a_time() {
    let store = with_env(FakeEnv::new());
    let guard = store.begin_operation().expect("slot should be free");

    assert!(store.begin_operation().is_err());

    drop(guard);
    assert!(store.begin_operation().is_ok());
}

#[test]
fn sidecar_may_be_allowed_when_the_service_is_absent_or_broken() {
    for health in [
        ServiceHealth::NotInstalled,
        ServiceHealth::VersionMismatch,
        ServiceHealth::Unavailable("boom".to_owned()),
    ] {
        let store = with_env(FakeEnv::new());
        store.observe(health.clone());

        store
            .allow_sidecar_for_session()
            .unwrap_or_else(|error| panic!("{health:?} should permit sidecar: {error}"));
        assert!(store.state().sidecar_allowed);
    }
}

#[test]
fn sidecar_may_be_allowed_after_an_install_was_requested() {
    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::NotInstalled);
    store
        .require_install_for_session()
        .expect("install may be required");
    assert_eq!(store.state().pending, Some(PendingAction::Install));

    store
        .allow_sidecar_for_session()
        .expect("install may be abandoned");

    let state = store.state();
    assert!(state.sidecar_allowed);
    assert_eq!(state.pending, None);
}

#[test]
fn sidecar_is_refused_when_the_service_is_usable_or_unknown() {
    for health in [ServiceHealth::Unknown, ServiceHealth::Ready] {
        let store = with_env(FakeEnv::new());
        store.observe(health.clone());

        assert!(
            store.allow_sidecar_for_session().is_err(),
            "{health:?} should refuse sidecar"
        );
    }
}

#[test]
fn sidecar_is_refused_during_a_privileged_operation() {
    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::NotInstalled);
    let _guard = store.begin_operation().expect("slot should be free");

    assert!(store.allow_sidecar_for_session().is_err());
}

#[test]
fn requiring_an_install_only_applies_to_an_absent_service() {
    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::NotInstalled);
    store.require_install_for_session().expect("absent service");
    assert_eq!(store.state().pending, Some(PendingAction::Install));

    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::Ready);
    store
        .require_install_for_session()
        .expect("no-op, not an error");
    assert_eq!(store.state().pending, None);
}

#[test]
fn requiring_an_install_is_refused_during_a_privileged_operation() {
    let store = with_env(FakeEnv::new());
    store.observe(ServiceHealth::NotInstalled);
    let _guard = store.begin_operation().expect("slot should be free");

    assert!(store.require_install_for_session().is_err());
}

#[tokio::test]
async fn settled_waits_for_an_operation_to_finish() {
    let store = Arc::new(with_env(FakeEnv::new()));
    store.observe(ServiceHealth::NotInstalled);
    let guard = store.begin_operation().expect("slot should be free");

    let waiter = {
        let store = Arc::clone(&store);
        tokio::spawn(async move { store.settled().await })
    };

    // The waiter cannot have finished while the operation is in flight.
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());

    store.observe(ServiceHealth::Ready);
    drop(guard);

    let settled = waiter.await.expect("waiter should not panic");
    assert!(settled.service_usable());
    assert!(!settled.op_in_flight);
}

// Multi-threaded on purpose, so operations really do start and finish underneath the reader.
// This is a guard, not a reproducer: the window it protects against was narrow enough that
// reverting the fix did not fail this test in six runs. What makes the invariant hold is
// that `settled` reads the slot once, inside the snapshot it returns; this fails if someone
// reintroduces a separate check before it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_settled_snapshot_never_describes_an_operation_in_flight() {
    let store = Arc::new(with_env(FakeEnv::new()));
    store.observe(ServiceHealth::Ready);

    for _ in 0..64 {
        let claimer = {
            let store = Arc::clone(&store);
            tokio::spawn(async move {
                let guard = store.begin_operation();
                tokio::task::yield_now().await;
                drop(guard);
            })
        };

        let settled = store.settled().await;
        assert!(
            !settled.op_in_flight,
            "a settled snapshot describes a settled state"
        );

        claimer.await.expect("the claimer should not panic");
    }
}
