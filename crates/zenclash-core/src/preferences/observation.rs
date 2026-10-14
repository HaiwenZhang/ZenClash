//! Publication eligibility tied to the preferences store's existing transaction owner.

use super::{AppPreferences, AppPreferencesResult, AppPreferencesStore};
use parking_lot::Mutex;
use std::{
    collections::HashMap,
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

struct RevisionOwner {
    transaction: Weak<Mutex<()>>,
    revision: Weak<AtomicU64>,
}

pub(super) fn shared_revision(transaction: &Arc<Mutex<()>>) -> Arc<AtomicU64> {
    static REVISIONS: OnceLock<Mutex<HashMap<usize, RevisionOwner>>> = OnceLock::new();
    let mut revisions = REVISIONS.get_or_init(Mutex::default).lock();
    revisions.retain(|_, owner| {
        owner.transaction.strong_count() > 0 && owner.revision.strong_count() > 0
    });
    let key = Arc::as_ptr(transaction) as usize;
    if let Some(owner) = revisions.get(&key)
        && Weak::ptr_eq(&owner.transaction, &Arc::downgrade(transaction))
        && let Some(revision) = owner.revision.upgrade()
    {
        return revision;
    }
    let revision = Arc::new(AtomicU64::new(0));
    revisions.insert(
        key,
        RevisionOwner {
            transaction: Arc::downgrade(transaction),
            revision: Arc::downgrade(&revision),
        },
    );
    revision
}

/// Persisted preferences and eligibility for memory-only publication of those exact facts.
/// This retains the actual store transaction owner, without a data-write lease or authority.
#[derive(Clone, Debug)]
pub struct AppPreferencesObservation {
    preferences: AppPreferences,
    transaction: Arc<Mutex<()>>,
    revision: Arc<AtomicU64>,
    generation: u64,
    scopes: Vec<std::path::PathBuf>,
    path: std::path::PathBuf,
}

impl AppPreferencesObservation {
    /// Returns the original document location for a fresh read after stale publication is rejected.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Returns the persisted snapshot captured by this observation.
    #[must_use]
    pub fn preferences(&self) -> &AppPreferences {
        &self.preferences
    }

    /// Checks actual store ownership; matching counters or values do not establish ownership.
    #[must_use]
    pub fn belongs_to(&self, store: &AppPreferencesStore) -> bool {
        Arc::ptr_eq(&self.transaction, &store.transaction)
            && Arc::ptr_eq(&self.revision, &store.revision)
    }

    /// Publishes memory-only facts if no store write or whole-root replacement superseded them.
    /// Returns None without waiting for an active writer. The callback must not acquire store,
    /// session or data locks, perform I/O, or update persistent preferences.
    pub fn publish_if_current<T>(&self, publish: impl FnOnce(&AppPreferences) -> T) -> Option<T> {
        crate::data_coordinator::publish_if_data_available(&self.scopes, || {
            let _transaction = self.transaction.try_lock()?;
            (self.revision.load(Ordering::Acquire) == self.generation)
                .then(|| publish(&self.preferences))
        })
        .flatten()
    }
}

impl AppPreferencesStore {
    /// Reads a snapshot with its original store revision for delayed memory publication.
    /// Run on a blocking worker; waits for pending root restores before reading.
    ///
    /// # Errors
    /// Returns filesystem, decoding, size or validation errors without changing the file.
    pub fn observe(&self) -> AppPreferencesResult<AppPreferencesObservation> {
        let _read_scope = self.write_access.acquire();
        let _transaction = self.transaction.lock();
        Ok(AppPreferencesObservation {
            preferences: self.load_unlocked()?,
            transaction: self.transaction.clone(),
            revision: self.revision.clone(),
            generation: self.revision.load(Ordering::Acquire),
            scopes: _read_scope.scopes().to_vec(),
            path: self.path.clone(),
        })
    }

    pub(crate) fn invalidate_observations(&self) {
        let _transaction = self.transaction.lock();
        self.revision.fetch_add(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests;
