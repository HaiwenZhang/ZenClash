//! Debounced and edge-triggered state observations for automatic runtime loops.

use std::time::{Duration, Instant};

const INITIAL_RETRY_BACKOFF: Duration = Duration::from_secs(2);
const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(30);

/// Requires two consecutive identical observations before confirming a value.
#[derive(Debug)]
pub struct StableObservation<T> {
    candidate: Option<T>,
}

impl<T> Default for StableObservation<T> {
    fn default() -> Self {
        Self { candidate: None }
    }
}

impl<T: PartialEq> StableObservation<T> {
    /// Creates an empty observation window.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `value` and returns `true` only when it matches the immediately
    /// preceding observation.
    pub fn observe(&mut self, value: T) -> bool {
        let stable = self.candidate.as_ref() == Some(&value);
        self.candidate = Some(value);
        stable
    }
}

/// Edge-triggered stable observation with exponential failure backoff.
///
/// Fires only when two consecutive samples agree on a value different from the
/// last committed state, or when a failed transition reaches its retry window.
#[derive(Debug)]
pub struct EdgeStableObservation<T> {
    candidate: Option<T>,
    committed: Option<T>,
    retry_after: Option<Instant>,
    retry_backoff: Duration,
}

impl<T> Default for EdgeStableObservation<T> {
    fn default() -> Self {
        Self {
            candidate: None,
            committed: None,
            retry_after: None,
            retry_backoff: INITIAL_RETRY_BACKOFF,
        }
    }
}

impl<T: PartialEq> EdgeStableObservation<T> {
    /// Creates an empty edge-triggered observation tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `value` at timestamp `now` and returns `true` when the caller
    /// should execute a transition for this stable state.
    pub fn observe(&mut self, value: T, now: Instant) -> bool {
        if self.candidate.as_ref() != Some(&value) {
            self.candidate = Some(value);
            self.retry_after = None;
            self.retry_backoff = INITIAL_RETRY_BACKOFF;
            return false;
        }
        if self.committed.as_ref() == Some(&value) {
            return false;
        }
        if let Some(deadline) = self.retry_after
            && now < deadline
        {
            return false;
        }
        true
    }

    /// Marks `value` as successfully applied so repeated identical samples do
    /// not re-trigger transitions.
    pub fn mark_committed(&mut self, value: T) {
        self.committed = Some(value);
        self.retry_after = None;
        self.retry_backoff = INITIAL_RETRY_BACKOFF;
    }

    /// Schedules an exponentially backed-off retry window after a transition failure.
    pub fn record_failure(&mut self, now: Instant) {
        self.retry_after = Some(now + self.retry_backoff);
        self.retry_backoff = (self.retry_backoff * 2).min(MAX_RETRY_BACKOFF);
    }
}

#[cfg(test)]
mod tests;
