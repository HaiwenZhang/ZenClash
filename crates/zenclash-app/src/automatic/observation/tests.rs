use zenclash_core::NetworkReachability;

use super::*;
use crate::automatic::SourceRevisionKey;

#[test]
fn unstable_network_and_unknown_observations_do_not_confirm_link_loss() {
    let now = Instant::now();
    let mut state = EdgeStableObservation::default();
    assert!(!state.observe(NetworkReachability::Offline, now));
    assert!(!state.observe(NetworkReachability::Unknown, now));
    assert!(!state.observe(NetworkReachability::Offline, now));
    assert!(state.observe(NetworkReachability::Offline, now));
}

#[test]
fn edge_stable_observation_does_not_refire_after_commit_until_state_changes() {
    let now = Instant::now();
    let mut state = EdgeStableObservation::default();
    assert!(!state.observe(NetworkReachability::Online, now));
    assert!(state.observe(NetworkReachability::Online, now));
    state.mark_committed(NetworkReachability::Online);

    for offset in 1..=5 {
        assert!(!state.observe(
            NetworkReachability::Online,
            now + Duration::from_secs(offset)
        ));
    }

    // Single-tick Offline glitch followed by Online must not refire Online.
    assert!(!state.observe(NetworkReachability::Offline, now + Duration::from_secs(6)));
    assert!(!state.observe(NetworkReachability::Online, now + Duration::from_secs(7)));
    assert!(!state.observe(NetworkReachability::Online, now + Duration::from_secs(8)));

    // Two consecutive Offline observations trigger suspension once.
    assert!(!state.observe(NetworkReachability::Offline, now + Duration::from_secs(9)));
    assert!(state.observe(NetworkReachability::Offline, now + Duration::from_secs(10)));
    state.mark_committed(NetworkReachability::Offline);
    assert!(!state.observe(NetworkReachability::Offline, now + Duration::from_secs(11)));

    // Returning to Online requires two stable ticks and fires once.
    assert!(!state.observe(NetworkReachability::Online, now + Duration::from_secs(12)));
    assert!(state.observe(NetworkReachability::Online, now + Duration::from_secs(13)));
}

#[test]
fn edge_stable_observation_retries_failed_state_with_exponential_backoff() {
    let start = Instant::now();
    let mut state = EdgeStableObservation::default();
    assert!(!state.observe(NetworkReachability::Offline, start));
    assert!(state.observe(NetworkReachability::Offline, start));

    state.record_failure(start);
    assert!(!state.observe(NetworkReachability::Offline, start + Duration::from_secs(1)));
    assert!(state.observe(NetworkReachability::Offline, start + Duration::from_secs(2)));

    let second_attempt = start + Duration::from_secs(2);
    state.record_failure(second_attempt);
    assert!(!state.observe(
        NetworkReachability::Offline,
        second_attempt + Duration::from_secs(3)
    ));
    assert!(state.observe(
        NetworkReachability::Offline,
        second_attempt + Duration::from_secs(4)
    ));

    // State change clears backoff immediately.
    let switch = second_attempt + Duration::from_secs(4);
    state.record_failure(switch);
    assert!(!state.observe(NetworkReachability::Online, switch + Duration::from_secs(1)));
    assert!(state.observe(NetworkReachability::Online, switch + Duration::from_secs(2)));
    state.mark_committed(NetworkReachability::Online);
    assert!(!state.observe(NetworkReachability::Online, switch + Duration::from_secs(3)));
}

#[test]
fn repeated_saves_debounce_until_the_latest_revision_is_stable() {
    let first = SourceRevisionKey {
        source_profile: "first.yaml".into(),
        digest: [1; 32],
    };
    let second = SourceRevisionKey {
        source_profile: "first.yaml".into(),
        digest: [2; 32],
    };
    let other = SourceRevisionKey {
        source_profile: "other.yaml".into(),
        digest: [2; 32],
    };
    let mut state = StableObservation::<Option<SourceRevisionKey>>::default();
    assert!(!state.observe(Some(first)));
    assert!(!state.observe(Some(second.clone())));
    assert!(state.observe(Some(second.clone())));
    assert!(!state.observe(Some(other.clone())));
    assert!(state.observe(Some(other)));
    assert!(!state.observe(None));
    assert!(!state.observe(Some(second)));
}
