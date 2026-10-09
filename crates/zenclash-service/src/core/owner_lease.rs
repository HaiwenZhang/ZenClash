//! Monotonic ownership lifetime, serialized by the server's lifecycle lock.

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::time::Instant;

struct OwnerLease {
    generation: u64,
    expires_at: Instant,
}

impl OwnerLease {
    fn new(generation: u64, now: Instant) -> Self {
        Self {
            generation,
            expires_at: now + crate::OWNER_SESSION_LEASE_TIMEOUT,
        }
    }

    fn is_expired(&self, generation: u64, now: Instant) -> bool {
        self.generation != generation || now >= self.expires_at
    }
}

static LEASE: Lazy<Mutex<Option<OwnerLease>>> = Lazy::new(|| Mutex::new(None));

pub(super) fn reset(generation: Option<u64>) {
    *LEASE.lock() = generation.map(|generation| OwnerLease::new(generation, Instant::now()));
}

/// A restarted IPC listener preserves its lease; a restarted daemon gives the GUI one renewal window.
pub(super) fn initialize(generation: Option<u64>) {
    let mut lease = LEASE.lock();
    match generation {
        Some(generation) if lease.as_ref().is_some_and(|lease| lease.generation == generation) => {}
        generation => *lease = generation.map(|generation| OwnerLease::new(generation, Instant::now())),
    }
}

pub(super) fn is_expired(generation: u64) -> bool {
    LEASE
        .lock()
        .as_ref()
        .is_none_or(|lease| lease.is_expired(generation, Instant::now()))
}

/// Also used after an admitted mutation completes: server work must not consume the GUI's renewal window.
pub(super) fn renew(generation: u64) -> bool {
    let mut lease = LEASE.lock();
    let Some(current) = lease.as_mut().filter(|lease| lease.generation == generation) else {
        return false;
    };
    current.expires_at = Instant::now() + crate::OWNER_SESSION_LEASE_TIMEOUT;
    true
}

#[cfg(test)]
mod tests {
    use super::OwnerLease;
    use std::time::{Duration, Instant};

    #[test]
    fn ownership_expires_at_the_deadline_and_rejects_other_generations() {
        let now = Instant::now();
        let lease = OwnerLease::new(7, now);
        assert!(!lease.is_expired(7, lease.expires_at - Duration::from_nanos(1)));
        assert!(lease.is_expired(7, lease.expires_at));
        assert!(lease.is_expired(8, now));
    }
}
