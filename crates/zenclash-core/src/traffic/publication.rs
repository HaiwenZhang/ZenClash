//! Generation transitions and coherent traffic snapshot/sample publication.

use std::{
    collections::VecDeque,
    sync::atomic::{AtomicU64, Ordering},
};

use parking_lot::RwLock;

use super::{
    LIVE_TRAFFIC_SAMPLE_COUNT, TrafficFrame, TrafficMonitor, TrafficRevision, TrafficSample,
    TrafficSnapshot, now_ms,
};

impl TrafficMonitor {
    /// Changes the accepted core generation and reconnects the WebSocket.
    ///
    /// Frames already queued by the older socket are rejected before they can
    /// update the snapshot or chart samples.
    pub fn synchronize_generation(&self, generation: u64) {
        let mut snapshot = self.snapshot.write();
        if self.expected_generation.swap(generation, Ordering::AcqRel) == generation {
            return;
        }
        snapshot.connected = false;
        self.generation.send_replace(generation);
        drop(snapshot);
        self.revision.advance();
    }
}

pub(super) fn update_frame_for_generation(
    snapshot: &RwLock<TrafficSnapshot>,
    samples: &RwLock<VecDeque<TrafficSample>>,
    frame: TrafficFrame,
    generation: u64,
    expected_generation: &AtomicU64,
    revision: &TrafficRevision,
) -> bool {
    if expected_generation.load(Ordering::Acquire) != generation {
        return false;
    }
    #[cfg(test)]
    crate::stream_publication_test_support::pause_at(
        crate::stream_publication_test_support::PublicationPhase::BeforeStateLock,
    );
    // The snapshot lock also gates generation changes and the associated samples.
    let mut snapshot = snapshot.write();
    if expected_generation.load(Ordering::Acquire) != generation {
        return false;
    }
    let mut samples = samples.write();
    update_frame(&mut snapshot, &mut samples, frame, generation);
    drop(samples);
    drop(snapshot);
    revision.advance();
    true
}

pub(super) fn update_frame(
    snapshot: &mut TrafficSnapshot,
    samples: &mut VecDeque<TrafficSample>,
    frame: TrafficFrame,
    generation: u64,
) {
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
    if samples.len() >= LIVE_TRAFFIC_SAMPLE_COUNT {
        samples.pop_front();
    }
    samples.push_back(TrafficSample {
        upload: frame.up,
        download: frame.down,
    });
}

pub(super) fn update_connection_for_generation(
    snapshot: &RwLock<TrafficSnapshot>,
    connected: bool,
    error: Option<String>,
    generation: u64,
    expected_generation: &AtomicU64,
    revision: &TrafficRevision,
) -> bool {
    if expected_generation.load(Ordering::Acquire) != generation {
        return false;
    }
    #[cfg(test)]
    crate::stream_publication_test_support::pause_at(
        crate::stream_publication_test_support::PublicationPhase::BeforeStateLock,
    );
    let mut snapshot = snapshot.write();
    if expected_generation.load(Ordering::Acquire) != generation {
        return false;
    }
    let changed = update_connection(&mut snapshot, connected, error, generation);
    drop(snapshot);
    if changed {
        revision.advance();
    }
    true
}

pub(super) fn update_connection(
    snapshot: &mut TrafficSnapshot,
    connected: bool,
    error: Option<String>,
    generation: u64,
) -> bool {
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
    snapshot.generation != previous_generation
        || snapshot.connected != previous_connected
        || snapshot.last_error != previous_error
}

#[cfg(test)]
mod tests;
