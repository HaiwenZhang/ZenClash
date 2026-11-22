use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::watch;
use zenclash_core::{ControlledConfigError, ControlledConfigStore, CoreSession, CoreSessionError};

use super::OutboundMode;

/// Serializes outbound-mode change requests and reconciles confirmed runtime
/// observations across the window, tray, and background sync loop.
#[derive(Clone, Debug)]
pub struct OutboundModeCoordinator {
    shared: Arc<ModeShared>,
}

#[derive(Debug)]
struct ModeShared {
    state: Mutex<ModeState>,
    updates: watch::Sender<u64>,
}

impl OutboundModeCoordinator {
    /// Creates a coordinator with an initial displayed mode not yet confirmed
    /// against the connected controller.
    #[must_use]
    pub fn new_unsynchronized(initial: OutboundMode) -> Self {
        let (updates, _) = watch::channel(0);
        Self {
            shared: Arc::new(ModeShared {
                state: Mutex::new(ModeState::new(initial, false)),
                updates,
            }),
        }
    }

    /// Returns the mode currently shown to the user (pending intent or last
    /// confirmed runtime mode).
    #[must_use]
    pub fn displayed(&self) -> OutboundMode {
        self.shared.state.lock().displayed
    }

    /// Returns the monotonic intent counter used to reject stale runtime reads.
    #[must_use]
    pub fn intent_generation(&self) -> u64 {
        self.shared.state.lock().intent_generation
    }

    /// Returns `true` while an outbound-mode transition is in flight.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.shared.state.lock().in_flight.is_some()
    }

    /// Returns the most recent localized mode-change failure, if any.
    #[must_use]
    pub fn error(&self) -> Option<String> {
        self.shared.state.lock().error.clone()
    }

    /// Subscribes to revision notifications emitted when displayed mode, pending
    /// state, or error feedback changes.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.shared.updates.subscribe()
    }

    /// Adopts a controller-observed outbound mode if no newer local intent has
    /// been submitted since `intent_generation`.
    pub fn synchronize(&self, mode: OutboundMode, intent_generation: u64) {
        self.update_state(|state| state.synchronize(mode, intent_generation));
    }

    /// Submits a user outbound-mode intent, coalescing overlapping requests so
    /// only the latest target is applied to the controller.
    ///
    /// Returns `true` when the intent was queued or started, and `false` when
    /// the mode was already confirmed with no change needed.
    #[must_use]
    pub fn request(
        &self,
        mode: OutboundMode,
        session: &CoreSession,
        controlled: &ControlledConfigStore,
        runtime: &tokio::runtime::Handle,
    ) -> bool {
        let submission = self.update_state(|state| state.submit(mode));
        match submission {
            Submission::Unchanged => false,
            Submission::Queued => true,
            Submission::Start(mode) => {
                let state = self.clone();
                let session = session.clone();
                let controlled = controlled.clone();
                runtime.spawn(async move {
                    state.drive(session, controlled, mode).await;
                });
                true
            }
        }
    }

    async fn drive(
        self,
        session: CoreSession,
        controlled: ControlledConfigStore,
        mut mode: OutboundMode,
    ) {
        loop {
            let result = session.set_mode(&controlled, mode.api_value()).await;
            if let Err(error) = &result {
                tracing::warn!(%error, mode = mode.api_value(), "failed to update core outbound mode");
            }
            let Some(next) = self.update_state(|state| {
                state.error = result.as_ref().err().map(mode_error_message);
                state.complete(mode, result.is_ok())
            }) else {
                break;
            };
            mode = next;
        }
    }

    fn update_state<T>(&self, update: impl FnOnce(&mut ModeState) -> T) -> T {
        let mut state = self.shared.state.lock();
        let previous_revision = state.notification_revision;
        let result = update(&mut state);
        if state.notification_revision != previous_revision {
            self.shared
                .updates
                .send_replace(state.notification_revision);
        }
        result
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Submission {
    Unchanged,
    Queued,
    Start(OutboundMode),
}

#[derive(Clone, Debug)]
struct ModeState {
    displayed: OutboundMode,
    confirmed: OutboundMode,
    in_flight: Option<OutboundMode>,
    pending: Option<OutboundMode>,
    synchronized: bool,
    intent_generation: u64,
    notification_revision: u64,
    error: Option<String>,
}

impl ModeState {
    const fn new(initial: OutboundMode, synchronized: bool) -> Self {
        Self {
            displayed: initial,
            confirmed: initial,
            in_flight: None,
            pending: None,
            synchronized,
            intent_generation: 0,
            notification_revision: 0,
            error: None,
        }
    }

    fn submit(&mut self, mode: OutboundMode) -> Submission {
        let cleared_error = self.error.take().is_some();
        if self.displayed == mode && (self.in_flight.is_some() || self.synchronized) {
            if cleared_error {
                self.notification_revision = self.notification_revision.wrapping_add(1);
            }
            return Submission::Unchanged;
        }
        self.displayed = mode;
        self.intent_generation = self.intent_generation.wrapping_add(1);
        self.notification_revision = self.notification_revision.wrapping_add(1);
        if self.in_flight.is_some() {
            self.pending = Some(mode);
            Submission::Queued
        } else {
            self.in_flight = Some(mode);
            Submission::Start(mode)
        }
    }

    fn complete(&mut self, mode: OutboundMode, succeeded: bool) -> Option<OutboundMode> {
        debug_assert_eq!(self.in_flight, Some(mode));
        self.in_flight = None;
        self.notification_revision = self.notification_revision.wrapping_add(1);
        if succeeded {
            self.confirmed = mode;
            self.synchronized = true;
        }

        if let Some(pending) = self.pending.take() {
            if self.synchronized && pending == self.confirmed {
                self.set_displayed(pending);
                return None;
            }
            self.in_flight = Some(pending);
            return Some(pending);
        }

        if !succeeded {
            self.set_displayed(self.confirmed);
        }
        None
    }

    fn synchronize(&mut self, mode: OutboundMode, intent_generation: u64) {
        if self.in_flight.is_some() || self.intent_generation != intent_generation {
            return;
        }
        self.confirmed = mode;
        self.synchronized = true;
        self.set_displayed(mode);
    }

    fn set_displayed(&mut self, mode: OutboundMode) {
        if self.displayed != mode {
            self.displayed = mode;
            self.notification_revision = self.notification_revision.wrapping_add(1);
        }
    }
}

fn mode_error_message(error: &CoreSessionError) -> String {
    match error {
        CoreSessionError::Config(ControlledConfigError::ModeOverrideConflict {
            requested,
            effective,
        }) => zenclash_i18n::text_with(
            "mode.errors.override_conflict",
            &[
                ("requested", requested.clone()),
                ("effective", effective.clone()),
            ],
        ),
        error => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
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
}
