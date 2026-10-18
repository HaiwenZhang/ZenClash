//! Owns generation admission, traffic history and revision publication together.

use std::{
    collections::VecDeque,
    sync::atomic::{AtomicU64, Ordering},
};

use parking_lot::RwLock;
use tokio::sync::watch;

use super::{
    TimedTrafficSample, TrafficFrame, TrafficMonitor, TrafficSample, TrafficSamples,
    TrafficSnapshot, initial_samples, now_ms,
};

pub(super) struct TrafficState {
    snapshot: RwLock<TrafficSnapshot>,
    samples: RwLock<TrafficSamples>,
    expected_generation: AtomicU64,
    generation: watch::Sender<u64>,
    revision: watch::Sender<u64>,
}

impl TrafficState {
    pub(super) fn new() -> Self {
        Self {
            snapshot: RwLock::new(TrafficSnapshot::default()),
            samples: RwLock::new(initial_samples()),
            expected_generation: AtomicU64::new(0),
            generation: watch::channel(0).0,
            revision: watch::channel(0).0,
        }
    }

    pub(super) fn subscribe_generation(&self) -> watch::Receiver<u64> {
        self.generation.subscribe()
    }

    pub(super) fn record_frame(&self, frame: TrafficFrame, generation: u64) -> bool {
        if self.expected_generation.load(Ordering::Acquire) != generation {
            return false;
        }
        #[cfg(test)]
        crate::stream_publication_test_support::pause_at(
            crate::stream_publication_test_support::PublicationPhase::BeforeStateLock,
        );
        // This lock gates generation changes and their associated chart samples.
        let mut snapshot = self.snapshot.write();
        if self.expected_generation.load(Ordering::Acquire) != generation {
            return false;
        }
        let mut samples = self.samples.write();
        snapshot.generation = generation;
        snapshot.upload = frame.up;
        snapshot.download = frame.down;
        snapshot.connected = true;
        snapshot.updated_at_ms = now_ms();
        snapshot.last_error = None;
        #[cfg(test)]
        crate::stream_publication_test_support::pause_at(
            crate::stream_publication_test_support::PublicationPhase::BeforeBufferAppend,
        );
        samples.record(
            snapshot.updated_at_ms,
            TrafficSample {
                upload: frame.up,
                download: frame.down,
            },
        );
        drop(samples);
        drop(snapshot);
        self.advance_revision();
        true
    }

    pub(super) fn record_connection(
        &self,
        connected: bool,
        error: Option<String>,
        generation: u64,
    ) -> bool {
        if self.expected_generation.load(Ordering::Acquire) != generation {
            return false;
        }
        #[cfg(test)]
        crate::stream_publication_test_support::pause_at(
            crate::stream_publication_test_support::PublicationPhase::BeforeStateLock,
        );
        let mut snapshot = self.snapshot.write();
        if self.expected_generation.load(Ordering::Acquire) != generation {
            return false;
        }
        let previous_generation = snapshot.generation;
        let previous_connected = snapshot.connected;
        let previous_error = snapshot.last_error.clone();
        snapshot.generation = generation;
        snapshot.connected = connected;
        if connected && error.is_none() {
            snapshot.last_error = None;
        }
        if let Some(error) = error {
            snapshot.last_error = Some(error);
        }
        let changed = snapshot.generation != previous_generation
            || snapshot.connected != previous_connected
            || snapshot.last_error != previous_error;
        drop(snapshot);
        if changed {
            self.advance_revision();
        }
        true
    }

    fn advance_revision(&self) {
        self.revision
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

impl TrafficMonitor {
    /// Returns a cheap point-in-time copy of the latest traffic state.
    #[must_use]
    pub fn snapshot(&self) -> TrafficSnapshot {
        self.state.snapshot.read().clone()
    }

    /// Returns the short ordered frame history used by compact traffic displays.
    ///
    /// Frames are appended by the WebSocket reader, not by UI polling timers,
    /// so repeated renders cannot create duplicate or contradictory samples.
    #[must_use]
    pub fn samples(&self) -> VecDeque<TrafficSample> {
        self.state.samples.read().recent.clone()
    }

    /// Returns bounded real stream history, including frames collected while hidden.
    ///
    /// Historical rates survive configuration changes and reconnects. Queued frames
    /// from superseded generations are rejected before entering this history.
    #[must_use]
    pub fn timed_samples(&self) -> VecDeque<TimedTrafficSample> {
        self.state.samples.read().timed.clone()
    }

    /// Returns a monotonic revision for traffic frames and connection-state changes.
    #[must_use]
    pub fn revision(&self) -> u64 {
        *self.state.revision.borrow()
    }

    /// Subscribes to traffic frames and connection-state changes.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.state.revision.subscribe()
    }

    /// Changes the accepted core generation and reconnects the WebSocket.
    ///
    /// Frames already queued by the older socket are rejected before they can
    /// update the snapshot or chart samples.
    pub fn synchronize_generation(&self, generation: u64) {
        let state = &self.state;
        let mut snapshot = state.snapshot.write();
        if state.expected_generation.swap(generation, Ordering::AcqRel) == generation {
            return;
        }
        snapshot.connected = false;
        state.generation.send_replace(generation);
        drop(snapshot);
        state.advance_revision();
    }
}

#[cfg(test)]
mod tests;
