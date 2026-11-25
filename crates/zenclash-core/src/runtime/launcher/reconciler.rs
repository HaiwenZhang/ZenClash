//! Latest-desired-intent reconciler with monotonic revision tracking.

use std::sync::Arc;

use parking_lot::Mutex;

use super::{
    CoreLaunchDescriptor, CoreProcessLauncher, CoreProcessLease, LeftoverLeaseTracker,
    leftover::{
        ensure_reclaimed_before_start_blocking, reclaim_all_blocking,
        stop_lease_or_register_leftover,
    },
};
use crate::mihomo::errors::{MihomoError, MihomoResult};

/// Desired lifecycle target state driven by [`IntentReconciler`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesiredCoreIntent {
    /// Managed core should be running with the supplied launch descriptor.
    Running(Box<CoreLaunchDescriptor>),
    /// Managed core should be stopped while keeping the reconciler open for future intents.
    Stopped,
    /// Managed core should be stopped permanently; subsequent non-closed intents are rejected.
    Closed,
}

impl DesiredCoreIntent {
    /// Builds a [`DesiredCoreIntent::Running`] intent from a launch descriptor.
    #[must_use]
    pub fn running(descriptor: CoreLaunchDescriptor) -> Self {
        Self::Running(Box::new(descriptor))
    }

    /// Reports whether this intent requests a running managed core.
    #[must_use]
    pub const fn is_running(&self) -> bool {
        matches!(self, Self::Running(_))
    }

    /// Reports whether this intent requests a stopped core while remaining open.
    #[must_use]
    pub const fn is_stopped(&self) -> bool {
        matches!(self, Self::Stopped)
    }

    /// Reports whether this intent permanently closes the reconciler.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        matches!(self, Self::Closed)
    }
}

/// Result of reconciling a [`DesiredCoreIntent`] revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// This revision executed a runtime transition and became the committed state.
    Applied {
        /// Monotonic revision applied by the reconciler.
        revision: u64,
    },
    /// The runtime already satisfied the desired intent, so no redundant transition ran.
    Coalesced {
        /// Monotonic revision satisfied without a transition.
        revision: u64,
    },
    /// A newer desired intent arrived before this revision could commit.
    Superseded {
        /// Older revision that was overtaken.
        revision: u64,
        /// Latest monotonic revision that superseded it.
        latest_revision: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CommittedCoreState {
    Stopped,
    Running(Box<CoreLaunchDescriptor>),
    Closed,
}

struct ReconcilerState {
    latest_revision: u64,
    applied_revision: u64,
    desired: DesiredCoreIntent,
    closed_requested: bool,
    committed: CommittedCoreState,
    active_lease: Option<Arc<dyn CoreProcessLease>>,
}

/// Single-worker latest-desired-intent reconciler with monotonic `revision` tracking
/// and integrated [`LeftoverLeaseTracker`] gating.
///
/// Instead of executing every queued start/stop/restart command serially, callers
/// submit desired lifecycle targets (`Running | Stopped | Closed`). Rapid intermediate
/// intents are coalesced or superseded before wasting launches, and any managed lease
/// whose exit cannot be confirmed is registered in [`LeftoverLeaseTracker`] to block
/// subsequent `Running` launches until exit is confirmed.
#[derive(Clone)]
pub struct IntentReconciler {
    worker_gate: Arc<tokio::sync::Mutex<()>>,
    state: Arc<Mutex<ReconcilerState>>,
    leftovers: LeftoverLeaseTracker,
}

impl std::fmt::Debug for IntentReconciler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (latest_revision, applied_revision, closed, running) = {
            let state = self.state.lock();
            (
                state.latest_revision,
                state.applied_revision,
                state.closed_requested,
                state.active_lease.is_some(),
            )
        };
        formatter
            .debug_struct("IntentReconciler")
            .field("latest_revision", &latest_revision)
            .field("applied_revision", &applied_revision)
            .field("closed", &closed)
            .field("has_active_lease", &running)
            .field("pending_leftovers", &self.leftovers.pending_count())
            .finish()
    }
}

impl Default for IntentReconciler {
    fn default() -> Self {
        Self::new(LeftoverLeaseTracker::new())
    }
}

impl IntentReconciler {
    /// Creates a new reconciler initially in the `Stopped` state, sharing the
    /// provided [`LeftoverLeaseTracker`].
    #[must_use]
    pub fn new(leftovers: LeftoverLeaseTracker) -> Self {
        Self {
            worker_gate: Arc::new(tokio::sync::Mutex::new(())),
            state: Arc::new(Mutex::new(ReconcilerState {
                latest_revision: 0,
                applied_revision: 0,
                desired: DesiredCoreIntent::Stopped,
                closed_requested: false,
                committed: CommittedCoreState::Stopped,
                active_lease: None,
            })),
            leftovers,
        }
    }

    /// Returns the latest monotonically assigned desired-intent revision.
    #[must_use]
    pub fn latest_revision(&self) -> u64 {
        self.state.lock().latest_revision
    }

    /// Returns the most recent revision confirmed as applied or coalesced.
    #[must_use]
    pub fn applied_revision(&self) -> u64 {
        self.state.lock().applied_revision
    }

    /// Returns a clone of the latest desired intent recorded by this reconciler.
    #[must_use]
    pub fn desired_intent(&self) -> DesiredCoreIntent {
        self.state.lock().desired.clone()
    }

    /// Returns the currently committed active process lease, if any.
    #[must_use]
    pub fn active_lease(&self) -> Option<Arc<dyn CoreProcessLease>> {
        self.state.lock().active_lease.clone()
    }

    /// Reports whether permanent shutdown (`Closed`) has been requested.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.state.lock().closed_requested
    }

    /// Returns the [`LeftoverLeaseTracker`] integrated with this reconciler.
    #[must_use]
    pub const fn leftovers(&self) -> &LeftoverLeaseTracker {
        &self.leftovers
    }

    /// Records a new latest desired intent and returns its monotonically increasing
    /// revision without waiting for the worker gate.
    ///
    /// # Errors
    ///
    /// Returns an error if [`DesiredCoreIntent::Closed`] has already been submitted
    /// and a non-closed intent is requested.
    pub fn submit(&self, intent: DesiredCoreIntent) -> MihomoResult<u64> {
        let mut state = self.state.lock();
        if state.closed_requested && !intent.is_closed() {
            return Err(MihomoError::Process(
                "core intent reconciler is closed; refusing new runtime intent".into(),
            ));
        }
        if intent.is_closed() {
            state.closed_requested = true;
        }
        state.latest_revision = state.latest_revision.saturating_add(1);
        state.desired = intent;
        Ok(state.latest_revision)
    }

    /// Submits `intent` as a new revision and advances the reconciler for that revision.
    ///
    /// Returns:
    /// - [`ReconcileOutcome::Applied`] when this revision executed the state transition;
    /// - [`ReconcileOutcome::Coalesced`] when the runtime already satisfied `intent`;
    /// - [`ReconcileOutcome::Superseded`] when a newer revision overtook this one.
    ///
    /// # Errors
    ///
    /// Returns an error if the reconciler is already closed for non-closed intents,
    /// if an existing or leftover lease cannot be confirmed exited before starting,
    /// or if the launcher fails to start the core process.
    pub async fn reconcile(
        &self,
        intent: DesiredCoreIntent,
        launcher: &dyn CoreProcessLauncher,
    ) -> MihomoResult<ReconcileOutcome> {
        let revision = self.submit(intent)?;
        self.reconcile_revision(Some(revision), launcher).await
    }

    /// Executes one single-worker reconciliation step toward whichever desired intent
    /// revision is currently latest.
    ///
    /// # Errors
    ///
    /// Returns an error if stopping an existing lease, reclaiming leftovers, or
    /// launching the target core fails.
    pub async fn reconcile_latest(
        &self,
        launcher: &dyn CoreProcessLauncher,
    ) -> MihomoResult<ReconcileOutcome> {
        self.reconcile_revision(None, launcher).await
    }

    pub(super) async fn reconcile_revision(
        &self,
        pinned_revision: Option<u64>,
        launcher: &dyn CoreProcessLauncher,
    ) -> MihomoResult<ReconcileOutcome> {
        let _worker = self.worker_gate.lock().await;

        let (target_revision, target_intent, current_committed, current_lease) = {
            let state = self.state.lock();
            if let Some(revision) = pinned_revision
                && state.latest_revision > revision
            {
                return Ok(ReconcileOutcome::Superseded {
                    revision,
                    latest_revision: state.latest_revision,
                });
            }
            (
                pinned_revision.unwrap_or(state.latest_revision),
                state.desired.clone(),
                state.committed.clone(),
                state.active_lease.clone(),
            )
        };

        match target_intent {
            DesiredCoreIntent::Stopped | DesiredCoreIntent::Closed => {
                let target_committed = if target_intent.is_closed() {
                    CommittedCoreState::Closed
                } else {
                    CommittedCoreState::Stopped
                };
                self.reconcile_stop_or_close(
                    target_revision,
                    target_committed,
                    current_committed,
                    current_lease,
                )
                .await
            }
            DesiredCoreIntent::Running(descriptor) => {
                self.reconcile_running(
                    target_revision,
                    descriptor,
                    current_committed,
                    current_lease,
                    launcher,
                )
                .await
            }
        }
    }

    async fn reconcile_stop_or_close(
        &self,
        target_revision: u64,
        target_committed: CommittedCoreState,
        current_committed: CommittedCoreState,
        current_lease: Option<Arc<dyn CoreProcessLease>>,
    ) -> MihomoResult<ReconcileOutcome> {
        let mut did_work = current_committed != target_committed;

        if let Some(lease) = current_lease {
            did_work = true;
            {
                let mut state = self.state.lock();
                state.active_lease = None;
                if state.committed != CommittedCoreState::Closed {
                    state.committed = CommittedCoreState::Stopped;
                }
            }
            stop_lease_or_register_leftover(&self.leftovers, lease).await?;
        }

        if self.leftovers.has_pending() {
            did_work = true;
            reclaim_all_blocking(&self.leftovers).await?;
        }

        let mut state = self.state.lock();
        state.committed = target_committed;
        if state.latest_revision > target_revision {
            return Ok(ReconcileOutcome::Superseded {
                revision: target_revision,
                latest_revision: state.latest_revision,
            });
        }
        state.applied_revision = target_revision;
        if did_work {
            Ok(ReconcileOutcome::Applied {
                revision: target_revision,
            })
        } else {
            Ok(ReconcileOutcome::Coalesced {
                revision: target_revision,
            })
        }
    }

    async fn reconcile_running(
        &self,
        target_revision: u64,
        descriptor: Box<CoreLaunchDescriptor>,
        current_committed: CommittedCoreState,
        current_lease: Option<Arc<dyn CoreProcessLease>>,
        launcher: &dyn CoreProcessLauncher,
    ) -> MihomoResult<ReconcileOutcome> {
        let already_running_same = current_committed
            == CommittedCoreState::Running(descriptor.clone())
            && current_lease
                .as_ref()
                .is_some_and(|lease| lease.is_running())
            && !self.leftovers.has_pending();

        if already_running_same {
            let mut state = self.state.lock();
            if state.latest_revision > target_revision {
                return Ok(ReconcileOutcome::Superseded {
                    revision: target_revision,
                    latest_revision: state.latest_revision,
                });
            }
            state.applied_revision = target_revision;
            return Ok(ReconcileOutcome::Coalesced {
                revision: target_revision,
            });
        }

        if let Some(old_lease) = current_lease {
            {
                let mut state = self.state.lock();
                state.active_lease = None;
                if state.committed != CommittedCoreState::Closed {
                    state.committed = CommittedCoreState::Stopped;
                }
            }
            stop_lease_or_register_leftover(&self.leftovers, old_lease).await?;
        }

        ensure_reclaimed_before_start_blocking(&self.leftovers).await?;

        if let Some(latest_revision) = self.check_superseded(target_revision) {
            return Ok(ReconcileOutcome::Superseded {
                revision: target_revision,
                latest_revision,
            });
        }

        let new_lease = launcher.launch(&descriptor).await?;

        let superseded_by = {
            let mut state = self.state.lock();
            if state.latest_revision > target_revision || state.closed_requested {
                Some(state.latest_revision)
            } else {
                state.active_lease = Some(Arc::clone(&new_lease));
                state.committed = CommittedCoreState::Running(descriptor);
                state.applied_revision = target_revision;
                None
            }
        };

        if let Some(latest_revision) = superseded_by {
            let _ = stop_lease_or_register_leftover(&self.leftovers, new_lease).await;
            return Ok(ReconcileOutcome::Superseded {
                revision: target_revision,
                latest_revision,
            });
        }

        Ok(ReconcileOutcome::Applied {
            revision: target_revision,
        })
    }

    fn check_superseded(&self, target_revision: u64) -> Option<u64> {
        let state = self.state.lock();
        (state.latest_revision > target_revision || state.closed_requested)
            .then_some(state.latest_revision)
    }
}
