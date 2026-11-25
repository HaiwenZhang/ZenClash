//! Leftover managed-process lease tracking and blocking reclamation helpers.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use parking_lot::Mutex;

use super::CoreProcessLease;
use crate::mihomo::errors::{MihomoError, MihomoResult};

/// Record of an old managed core lease whose exit was not yet confirmed.
///
/// Subsequent start intents MUST reclaim all registered leftover leases before
/// spawning a replacement kernel, preventing dual-kernel coexistence.
#[derive(Clone)]
pub struct LeftoverLease {
    id: u64,
    last_pid: Option<u32>,
    lease: Arc<dyn CoreProcessLease>,
}

impl std::fmt::Debug for LeftoverLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LeftoverLease")
            .field("id", &self.id)
            .field("last_pid", &self.last_pid)
            .field("running", &self.lease.is_running())
            .finish()
    }
}

impl LeftoverLease {
    /// Returns the monotonically assigned leftover registration identifier.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Returns the process identifier observed when the leftover was registered.
    #[must_use]
    pub const fn last_pid(&self) -> Option<u32> {
        self.last_pid
    }

    /// Attempts to confirm exit or stop the underlying leftover process.
    ///
    /// # Errors
    ///
    /// Returns the stop failure when exit cannot yet be confirmed.
    pub fn try_reclaim(&self) -> MihomoResult<()> {
        if !self.lease.is_running() {
            return Ok(());
        }
        self.lease.stop_process()
    }
}

/// Thread-safe registry of unconfirmed-exit kernel leases that block new kernel launches
/// until successfully reclaimed.
#[derive(Clone, Default)]
pub struct LeftoverLeaseTracker {
    next_id: Arc<AtomicU64>,
    pending: Arc<Mutex<Vec<LeftoverLease>>>,
}

impl std::fmt::Debug for LeftoverLeaseTracker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LeftoverLeaseTracker")
            .field("pending_count", &self.pending_count())
            .finish()
    }
}

impl LeftoverLeaseTracker {
    /// Creates an empty leftover tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a lease whose termination was not confirmed, returning its tracking id.
    pub fn register(&self, lease: Arc<dyn CoreProcessLease>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let last_pid = lease.pid();
        self.pending.lock().push(LeftoverLease {
            id,
            last_pid,
            lease,
        });
        id
    }

    /// Reports whether any unconfirmed leftover lease is still pending reclamation.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        !self.pending.lock().is_empty()
    }

    /// Returns the number of currently pending leftover leases.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.lock().len()
    }

    /// Attempts to reclaim all pending leftover leases without holding the registry lock
    /// across potentially blocking `stop_process` calls.
    ///
    /// Successfully reclaimed leases are removed; any lease that still fails to confirm
    /// exit remains registered and causes this method to return an error.
    ///
    /// # Errors
    ///
    /// Returns an error if one or more leftover leases could not be confirmed stopped.
    pub fn reclaim_all(&self) -> MihomoResult<()> {
        let snapshot = {
            let guard = self.pending.lock();
            if guard.is_empty() {
                return Ok(());
            }
            guard.clone()
        };
        let mut reclaimed_ids = Vec::with_capacity(snapshot.len());
        let mut first_error = None;
        for entry in snapshot {
            match entry.try_reclaim() {
                Ok(()) => reclaimed_ids.push(entry.id),
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        if !reclaimed_ids.is_empty() {
            self.pending
                .lock()
                .retain(|entry| !reclaimed_ids.contains(&entry.id));
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Rejects starting a new kernel if any leftover lease remains unreclaimed after
    /// attempting cleanup.
    ///
    /// # Errors
    ///
    /// Returns an error if any leftover lease cannot be confirmed exited.
    pub fn ensure_reclaimed_before_start(&self) -> MihomoResult<()> {
        self.reclaim_all()?;
        if self.has_pending() {
            return Err(MihomoError::Process(
                "previous managed core lease is still active; cannot start a second core".into(),
            ));
        }
        Ok(())
    }
}

pub(super) async fn ensure_reclaimed_before_start_blocking(
    leftovers: &LeftoverLeaseTracker,
) -> MihomoResult<()> {
    if !leftovers.has_pending() {
        return Ok(());
    }
    let tracker = leftovers.clone();
    tokio::task::spawn_blocking(move || tracker.ensure_reclaimed_before_start())
        .await
        .map_err(|error| {
            MihomoError::Process(format!("leftover reclamation task failed: {error}"))
        })?
}

pub(super) async fn reclaim_all_blocking(leftovers: &LeftoverLeaseTracker) -> MihomoResult<()> {
    if !leftovers.has_pending() {
        return Ok(());
    }
    let tracker = leftovers.clone();
    tokio::task::spawn_blocking(move || tracker.reclaim_all())
        .await
        .map_err(|error| {
            MihomoError::Process(format!("leftover reclamation task failed: {error}"))
        })?
}

pub(super) async fn stop_lease_or_register_leftover(
    leftovers: &LeftoverLeaseTracker,
    lease: Arc<dyn CoreProcessLease>,
) -> MihomoResult<()> {
    let tracker = leftovers.clone();
    tokio::task::spawn_blocking(move || {
        if !lease.is_running() {
            return Ok(());
        }
        if let Err(error) = lease.stop_process() {
            tracker.register(lease);
            return Err(error);
        }
        Ok(())
    })
    .await
    .map_err(|error| MihomoError::Process(format!("managed core stop task failed: {error}")))?
}
