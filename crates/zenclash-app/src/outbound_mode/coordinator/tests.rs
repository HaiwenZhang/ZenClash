use super::*;

#[test]
fn unsynchronized_state_sends_an_initially_displayed_mode() {
    let mut state = ModeState::new(OutboundMode::Rule, false);

    assert_eq!(
        state.submit(OutboundMode::Rule),
        Submission::Start(OutboundMode::Rule)
    );
}

#[test]
fn busy_state_keeps_only_the_latest_intent() {
    let mut state = ModeState::new(OutboundMode::Rule, true);
    let _ = state.submit(OutboundMode::Global);
    let _ = state.submit(OutboundMode::Direct);
    let _ = state.submit(OutboundMode::Rule);

    assert_eq!(state.pending, Some(OutboundMode::Rule));
}

#[test]
fn failed_update_restores_the_confirmed_mode() {
    let mut state = ModeState::new(OutboundMode::Rule, true);
    let _ = state.submit(OutboundMode::Global);

    let next = state.complete(OutboundMode::Global, false);

    assert_eq!((next, state.displayed), (None, OutboundMode::Rule));
}

#[test]
fn successful_update_starts_the_latest_pending_mode() {
    let mut state = ModeState::new(OutboundMode::Rule, true);
    let _ = state.submit(OutboundMode::Global);
    let _ = state.submit(OutboundMode::Direct);

    assert_eq!(
        state.complete(OutboundMode::Global, true),
        Some(OutboundMode::Direct)
    );
}

#[test]
fn stale_runtime_snapshot_cannot_replace_a_newer_intent() {
    let mut state = ModeState::new(OutboundMode::Rule, true);
    let intent_generation = state.intent_generation;
    let _ = state.submit(OutboundMode::Direct);

    state.synchronize(OutboundMode::Global, intent_generation);

    assert_eq!(state.displayed, OutboundMode::Direct);
}

#[test]
fn successful_completion_advances_revision_when_the_displayed_mode_is_unchanged() {
    let mut state = ModeState::new(OutboundMode::Rule, true);
    let _ = state.submit(OutboundMode::Global);
    let pending_revision = state.notification_revision;

    let _ = state.complete(OutboundMode::Global, true);

    assert!(state.notification_revision > pending_revision);
}

#[test]
fn coordinator_subscription_observes_a_synchronized_mode_change() {
    let coordinator = OutboundModeCoordinator::new_unsynchronized(OutboundMode::Rule);
    let updates = coordinator.subscribe();

    coordinator.synchronize(OutboundMode::Global, 0);

    assert!(updates.has_changed().unwrap());
}

#[test]
fn reaffirming_the_confirmed_mode_publishes_cleared_error_state() {
    let coordinator = OutboundModeCoordinator::new_unsynchronized(OutboundMode::Rule);
    coordinator.synchronize(OutboundMode::Rule, 0);
    coordinator.update_state(|state| {
        state.error = Some("rejected global mode".into());
        state.notification_revision += 1;
    });
    let updates = coordinator.subscribe();

    assert_eq!(
        coordinator.update_state(|state| state.submit(OutboundMode::Rule)),
        Submission::Unchanged
    );
    assert_eq!(coordinator.error(), None);
    assert!(updates.has_changed().unwrap());
}

#[tokio::test]
async fn conflicting_mode_rolls_back_the_display_and_publishes_localized_feedback() {
    let root = std::env::temp_dir().join(format!(
        "zenclash-mode-conflict-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let profile = root.join("profile.yaml");
    let yaml_override = root.join("mode.yaml");
    std::fs::write(&profile, "mode: rule\nrules: [MATCH,DIRECT]\n").unwrap();
    std::fs::write(&yaml_override, "mode: rule\n").unwrap();
    let controlled = ControlledConfigStore::new(root.join("controlled"));
    let session = CoreSession::open_with_config(
        zenclash_core::CoreKind::Mihomo,
        zenclash_core::MihomoClient::new(zenclash_core::MihomoEndpoint::new(
            "http://127.0.0.1:1",
            "",
        ))
        .unwrap(),
        Some(profile),
        vec![yaml_override],
    )
    .unwrap();
    let coordinator = OutboundModeCoordinator::new_unsynchronized(OutboundMode::Rule);
    coordinator.synchronize(OutboundMode::Rule, 0);
    let mut updates = coordinator.subscribe();
    assert!(coordinator.request(
        OutboundMode::Global,
        &session,
        &controlled,
        &tokio::runtime::Handle::current(),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while coordinator.is_pending() {
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();

    assert_eq!(coordinator.displayed(), OutboundMode::Rule);
    assert_eq!(
        coordinator.error(),
        Some(zenclash_i18n::text_with(
            "mode.errors.override_conflict",
            &[("requested", "global".into()), ("effective", "rule".into())],
        ))
    );
    assert_eq!(session.generation(), 0);
    std::fs::remove_dir_all(root).unwrap();
}
