use std::{path::PathBuf, sync::Arc};

use parking_lot::Mutex;
use tokio::sync::watch;
use zenclash_core::{ControlledConfigStore, MihomoClient, YamlOverrideStore};

use super::sidebar::OutboundMode;

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
    pub(crate) fn new_unsynchronized(initial: OutboundMode) -> Self {
        let (updates, _) = watch::channel(0);
        Self {
            shared: Arc::new(ModeShared {
                state: Mutex::new(ModeState::new(initial, false)),
                updates,
            }),
        }
    }

    pub(crate) fn displayed(&self) -> OutboundMode {
        self.shared.state.lock().displayed
    }

    pub(crate) fn generation(&self) -> u64 {
        self.shared.state.lock().generation
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.shared.state.lock().in_flight.is_some()
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.shared.updates.subscribe()
    }

    pub(crate) fn synchronize(&self, mode: OutboundMode, generation: u64) {
        self.update_state(|state| state.synchronize(mode, generation));
    }

    pub(crate) fn request(
        &self,
        mode: OutboundMode,
        client: &MihomoClient,
        controlled: Option<(ControlledConfigStore, PathBuf)>,
        runtime: &tokio::runtime::Handle,
    ) -> bool {
        let submission = self.update_state(|state| state.submit(mode));
        match submission {
            Submission::Unchanged => false,
            Submission::Queued => true,
            Submission::Start(mode) => {
                let state = self.clone();
                let client = client.clone();
                runtime.spawn(async move {
                    state.drive(client, controlled, mode).await;
                });
                true
            }
        }
    }

    async fn drive(
        self,
        client: MihomoClient,
        controlled: Option<(ControlledConfigStore, PathBuf)>,
        mut mode: OutboundMode,
    ) {
        loop {
            let result = match &controlled {
                Some((controlled, profile)) => match load_managed_overrides().await {
                    Ok(overrides) => controlled
                        .apply_mode_update_with_overrides(
                            &client,
                            profile,
                            mode.api_value(),
                            overrides,
                        )
                        .await
                        .map_err(|error| error.to_string()),
                    Err(error) => Err(error),
                },
                None => client
                    .set_mode(mode.api_value())
                    .await
                    .map_err(|error| error.to_string()),
            };
            if let Err(error) = &result {
                tracing::warn!(%error, mode = mode.api_value(), "failed to update core outbound mode");
            }
            let Some(next) = self.update_state(|state| state.complete(mode, result.is_ok())) else {
                break;
            };
            mode = next;
        }
    }

    fn update_state<T>(&self, update: impl FnOnce(&mut ModeState) -> T) -> T {
        let mut state = self.shared.state.lock();
        let previous_revision = state.revision;
        let result = update(&mut state);
        if state.revision != previous_revision {
            self.shared.updates.send_replace(state.revision);
        }
        result
    }
}

async fn load_managed_overrides() -> Result<Vec<PathBuf>, String> {
    tokio::task::spawn_blocking(|| YamlOverrideStore::discover()?.load_enabled_paths())
        .await
        .map_err(|error| {
            zenclash_i18n::text_with(
                "profiles.errors.override_read_task",
                &[("error", error.to_string())],
            )
        })?
        .map_err(|error| error.to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Submission {
    Unchanged,
    Queued,
    Start(OutboundMode),
}

#[derive(Clone, Copy, Debug)]
struct ModeState {
    displayed: OutboundMode,
    confirmed: OutboundMode,
    in_flight: Option<OutboundMode>,
    pending: Option<OutboundMode>,
    synchronized: bool,
    generation: u64,
    revision: u64,
}

impl ModeState {
    const fn new(initial: OutboundMode, synchronized: bool) -> Self {
        Self {
            displayed: initial,
            confirmed: initial,
            in_flight: None,
            pending: None,
            synchronized,
            generation: 0,
            revision: 0,
        }
    }

    fn submit(&mut self, mode: OutboundMode) -> Submission {
        if self.displayed == mode && (self.in_flight.is_some() || self.synchronized) {
            return Submission::Unchanged;
        }
        self.displayed = mode;
        self.generation = self.generation.wrapping_add(1);
        self.revision = self.revision.wrapping_add(1);
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
        self.revision = self.revision.wrapping_add(1);
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

    fn synchronize(&mut self, mode: OutboundMode, generation: u64) {
        if self.in_flight.is_some() || self.generation != generation {
            return;
        }
        self.confirmed = mode;
        self.synchronized = true;
        self.set_displayed(mode);
    }

    fn set_displayed(&mut self, mode: OutboundMode) {
        if self.displayed != mode {
            self.displayed = mode;
            self.revision = self.revision.wrapping_add(1);
        }
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
        let generation = state.generation;
        let _ = state.submit(OutboundMode::Direct);

        state.synchronize(OutboundMode::Global, generation);

        assert_eq!(state.displayed, OutboundMode::Direct);
    }

    #[test]
    fn successful_completion_advances_revision_when_the_displayed_mode_is_unchanged() {
        let mut state = ModeState::new(OutboundMode::Rule, true);
        let _ = state.submit(OutboundMode::Global);
        let pending_revision = state.revision;

        let _ = state.complete(OutboundMode::Global, true);

        assert!(state.revision > pending_revision);
    }

    #[test]
    fn coordinator_subscription_observes_a_synchronized_mode_change() {
        let coordinator = OutboundModeCoordinator::new_unsynchronized(OutboundMode::Rule);
        let updates = coordinator.subscribe();

        coordinator.synchronize(OutboundMode::Global, 0);

        assert!(updates.has_changed().unwrap());
    }
}
