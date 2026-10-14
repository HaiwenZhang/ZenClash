//! One shared application recovery record; callers submit facts instead of editing state.

use std::sync::Arc;

use parking_lot::Mutex;
use zenclash_core::{
    CaptureOutcome, CoreApplyOutcome, CoreSession, ProfileApplyOutcome, ProfileRecovery,
    ServiceMaintenancePreparation, ServiceTunOutcome,
};

/// Display and confirmation facts derived from an accepted service runtime receipt.
pub(super) struct ServiceRuntimeFacts {
    generation: u64,
    commit_pending: bool,
    warning: Option<String>,
}

impl ServiceRuntimeFacts {
    pub(super) fn new(
        core: &CoreApplyOutcome,
        commit_pending: bool,
        warning: Option<&str>,
    ) -> Self {
        Self {
            generation: core.generation,
            commit_pending,
            warning: warning.map(str::to_owned),
        }
    }

    pub(super) fn from_profile_or_config(outcome: &ServiceTunOutcome) -> Option<Self> {
        let core = outcome.core()?;
        let capture_warning = match outcome.capture() {
            CaptureOutcome::RolledBack { failure, .. }
            | CaptureOutcome::ReconcileNeeded { failure, .. } => Some(failure.as_str()),
            _ => None,
        };
        let warning = match (outcome.recovery_warning(), capture_warning) {
            (Some(first), Some(second)) => Some(format!("{first}; {second}")),
            (first, second) => first.or(second).map(str::to_owned),
        };
        Some(Self {
            generation: core.generation,
            commit_pending: outcome.commit_pending(),
            warning,
        })
    }
}

/// Clones retain the original record, including native recovery resources.
#[derive(Clone, Default)]
pub(super) struct ApplicationRecoveryRecord {
    state: Arc<Mutex<RecoveryState>>,
}

#[derive(Default)]
struct RecoveryState {
    last_accepted_generation: Option<u64>,
    retained_profile_outcome: Option<Arc<ProfileApplyOutcome>>,
    unresolved_profile_outcome: Option<Arc<ProfileApplyOutcome>>,
    pending_service_commit: Option<u64>,
    service_recovery_warning: Option<String>,
    maintenance_preparation: Option<Arc<ServiceMaintenancePreparation>>,
    local_recovery_preparation: Option<Arc<ServiceMaintenancePreparation>>,
}

impl RecoveryState {
    fn accept_new_generation(&mut self, generation: u64) -> bool {
        if self
            .last_accepted_generation
            .is_some_and(|accepted| generation <= accepted)
        {
            return false;
        }
        self.last_accepted_generation = Some(generation);
        true
    }

    fn clear_runtime_recovery(&mut self) {
        self.unresolved_profile_outcome = None;
        self.retained_profile_outcome = None;
        self.pending_service_commit = None;
        self.service_recovery_warning = None;
    }

    fn accept_runtime(&mut self, generation: u64) {
        if self.accept_new_generation(generation) {
            self.clear_runtime_recovery();
        }
    }

    fn pending_service_commit(&self) -> Option<u64> {
        match self.unresolved_profile_outcome.as_deref() {
            Some(ProfileApplyOutcome::CommittedButRuntimeUnknown {
                runtime_version, ..
            }) => Some(*runtime_version),
            _ => self.pending_service_commit,
        }
    }

    fn accept_profile(&mut self, outcome: &Arc<ProfileApplyOutcome>) -> Option<u64> {
        let generation = profile_runtime_generation(outcome);
        if let Some(generation) = generation
            && !self.accept_new_generation(generation)
        {
            return Some(generation);
        }
        match outcome.as_ref() {
            ProfileApplyOutcome::Applied { .. } => self.clear_runtime_recovery(),
            ProfileApplyOutcome::RolledBack { .. } => {
                self.unresolved_profile_outcome = None;
                self.retained_profile_outcome = Some(Arc::clone(outcome));
            }
            ProfileApplyOutcome::RuntimeUnknown { .. }
            | ProfileApplyOutcome::PersistedButRuntimeUnknown { .. }
            | ProfileApplyOutcome::CommittedButRuntimeUnknown { .. } => {
                self.unresolved_profile_outcome = Some(Arc::clone(outcome));
                self.retained_profile_outcome = Some(Arc::clone(outcome));
            }
            ProfileApplyOutcome::Rejected { .. } => {
                self.retained_profile_outcome = Some(Arc::clone(outcome))
            }
            ProfileApplyOutcome::Stored { .. } => {}
        }
        generation
    }

    fn supplement_profile(&mut self, facts: ServiceRuntimeFacts, current_generation: u64) {
        if current_generation == facts.generation
            && self.last_accepted_generation == Some(facts.generation)
        {
            // Two facts from the same transaction: never replace its durable source receipt.
            self.pending_service_commit = facts.commit_pending.then_some(facts.generation);
            self.service_recovery_warning = facts.warning;
        }
    }

    fn accept_service_runtime(&mut self, facts: ServiceRuntimeFacts, current_generation: u64) {
        if current_generation == facts.generation && self.accept_new_generation(facts.generation) {
            self.clear_runtime_recovery();
            self.pending_service_commit = facts.commit_pending.then_some(facts.generation);
            self.service_recovery_warning = facts.warning;
        }
    }
}

fn profile_runtime_generation(outcome: &ProfileApplyOutcome) -> Option<u64> {
    match outcome {
        ProfileApplyOutcome::Applied {
            runtime_version, ..
        }
        | ProfileApplyOutcome::RolledBack {
            runtime_version, ..
        }
        | ProfileApplyOutcome::RuntimeUnknown {
            runtime_version, ..
        }
        | ProfileApplyOutcome::PersistedButRuntimeUnknown {
            runtime_version, ..
        }
        | ProfileApplyOutcome::CommittedButRuntimeUnknown {
            runtime_version, ..
        } => Some(*runtime_version),
        ProfileApplyOutcome::Stored { .. } | ProfileApplyOutcome::Rejected { .. } => None,
    }
}

impl ApplicationRecoveryRecord {
    pub(super) fn accept_profile_completion(
        &self,
        outcome: &Arc<ProfileApplyOutcome>,
        service: Option<ServiceRuntimeFacts>,
        session: &CoreSession,
    ) {
        // This is a freshness sample, not runtime transition authority. Read it
        // without the record lock, as the original service-completion path did.
        let current_generation = service.as_ref().map(|_| session.generation());
        let mut state = self.state.lock();
        let generation = state.accept_profile(outcome);
        if let Some(service) = service
            && generation == Some(service.generation)
            && let Some(current_generation) = current_generation
        {
            state.supplement_profile(service, current_generation);
        }
    }

    pub(super) fn accept_runtime(&self, core: &CoreApplyOutcome) {
        self.state.lock().accept_runtime(core.generation);
    }

    pub(super) fn accept_service_runtime(
        &self,
        facts: ServiceRuntimeFacts,
        current_generation: u64,
    ) {
        self.state
            .lock()
            .accept_service_runtime(facts, current_generation);
    }

    pub(super) fn accept_initialization(
        &self,
        saved: Option<&CoreApplyOutcome>,
        commit_pending: bool,
        current_generation: u64,
    ) {
        if let Some(saved) = saved {
            self.accept_service_runtime(
                ServiceRuntimeFacts::new(saved, commit_pending, None),
                current_generation,
            );
        }
    }

    pub(super) fn latest_profile_recovery(&self) -> Option<ProfileRecovery> {
        match self.state.lock().unresolved_profile_outcome.as_deref() {
            Some(
                ProfileApplyOutcome::RuntimeUnknown { recovery, .. }
                | ProfileApplyOutcome::PersistedButRuntimeUnknown { recovery, .. },
            ) => Some(recovery.clone()),
            _ => None,
        }
    }

    pub(super) fn pending_service_commit(&self) -> Option<u64> {
        self.state.lock().pending_service_commit()
    }

    pub(super) fn service_recovery_warning(&self) -> Option<String> {
        self.state.lock().service_recovery_warning.clone()
    }

    pub(super) fn confirm_service_commit(&self, generation: u64) -> bool {
        let mut state = self.state.lock();
        if state.pending_service_commit() != Some(generation) {
            return false;
        }
        state.unresolved_profile_outcome = None;
        state.retained_profile_outcome = None;
        state.pending_service_commit = None;
        // Native commit confirmation does not establish independent cleanup success.
        true
    }

    pub(super) fn accept_maintenance_preparation(
        &self,
        preparation: Arc<ServiceMaintenancePreparation>,
        session: &CoreSession,
    ) {
        let mut state = self.state.lock();
        if let Some(recovery) = preparation.recovery() {
            if recovery.runtime().ready() && session.generation() == recovery.runtime().generation()
            {
                state.accept_runtime(recovery.runtime().generation());
            }
            state.local_recovery_preparation = Some(preparation.clone());
        }
        state.maintenance_preparation = Some(preparation);
    }

    pub(super) fn maintenance_preparation(&self) -> Option<Arc<ServiceMaintenancePreparation>> {
        self.state.lock().maintenance_preparation.clone()
    }

    pub(super) fn local_recovery_preparation(&self) -> Option<Arc<ServiceMaintenancePreparation>> {
        self.state.lock().local_recovery_preparation.clone()
    }

    pub(super) fn is_latest_maintenance_preparation(
        &self,
        preparation: &Arc<ServiceMaintenancePreparation>,
    ) -> bool {
        self.state
            .lock()
            .maintenance_preparation
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, preparation))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_local_recovery_retires_old_service_confirmation_without_losing_newer_results() {
        let record = ApplicationRecoveryRecord::default();
        record.accept_service_runtime(
            ServiceRuntimeFacts {
                generation: 7,
                commit_pending: true,
                warning: Some("old service commit uncertainty".into()),
            },
            7,
        );
        record.accept_runtime(&CoreApplyOutcome {
            kind: zenclash_core::CoreApplyKind::Restarted,
            generation: 8,
        });
        assert_eq!(record.state.lock().last_accepted_generation, Some(8));
        assert!(record.pending_service_commit().is_none());
        assert!(record.service_recovery_warning().is_none());
        record.accept_service_runtime(
            ServiceRuntimeFacts {
                generation: 9,
                commit_pending: true,
                warning: None,
            },
            9,
        );
        record.accept_runtime(&CoreApplyOutcome {
            kind: zenclash_core::CoreApplyKind::Restarted,
            generation: 8,
        });
        assert_eq!(record.pending_service_commit(), Some(9));
    }
}
