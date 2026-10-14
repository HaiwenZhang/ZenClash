//! Isolated preference-document paths shared by storage and observation regressions.

use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) fn test_path(name: &str) -> PathBuf {
    let sequence = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "zenclash-preferences-{name}-{}-{sequence}.json",
        std::process::id()
    ))
}

/// Positive assertions retry transient coordinator contention; stale proofs still time out.
pub(crate) fn publish_when_available<T>(
    observation: &super::AppPreferencesObservation,
    mut publish: impl FnMut(&super::AppPreferences) -> T,
) -> T {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(value) = observation.publish_if_current(&mut publish) {
            return value;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "preference observation never became eligible"
        );
        std::thread::yield_now();
    }
}
