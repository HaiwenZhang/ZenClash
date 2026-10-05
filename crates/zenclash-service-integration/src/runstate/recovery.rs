// Forked from Clash Verge Rev core/service.rs, adapted 2026-10-05.
// GPL-3.0-only; see NOTICE.md and UPSTREAM.json.
use super::{OwnerRecoveryReason, RunStateEnv, RunStateStore, ServiceHealth};

/// Native proxy cleanup policy copied from the upstream owner monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnerRecoveryPolicy {
    /// Whether this owner may attempt to clear its native system proxy.
    pub reset_system_proxy: bool,
}

/// The macOS proxy is machine-wide and only the helper may write it, for the session it still
/// honours; a displaced or unreachable owner must leave it alone.
pub const fn owner_recovery_policy(
    reason: OwnerRecoveryReason,
    is_macos: bool,
) -> OwnerRecoveryPolicy {
    OwnerRecoveryPolicy {
        reset_system_proxy: !is_macos || matches!(reason, OwnerRecoveryReason::SameOwnerFailure),
    }
}

/// Records a sustained control-channel failure without misclassifying displacement.
pub fn mark_service_unavailable_after_owner_loss<E: RunStateEnv>(
    store: &RunStateStore<E>,
    reason: OwnerRecoveryReason,
) {
    if matches!(reason, OwnerRecoveryReason::TransportFailure) {
        store.observe(ServiceHealth::Unavailable(
            "service control IPC unavailable after sustained transport failure".to_owned(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_clears_proxy_only_for_same_owner_failure() {
        for reason in [
            OwnerRecoveryReason::Displaced,
            OwnerRecoveryReason::SameOwnerFailure,
            OwnerRecoveryReason::TransportFailure,
        ] {
            assert_eq!(
                owner_recovery_policy(reason, true).reset_system_proxy,
                reason == OwnerRecoveryReason::SameOwnerFailure
            );
        }
    }

    #[test]
    fn windows_and_linux_may_release_their_owned_proxy() {
        for reason in [
            OwnerRecoveryReason::Displaced,
            OwnerRecoveryReason::SameOwnerFailure,
            OwnerRecoveryReason::TransportFailure,
        ] {
            assert!(owner_recovery_policy(reason, false).reset_system_proxy);
        }
    }
    #[test]
    fn only_transport_owner_loss_marks_cached_readiness_unavailable() {
        for reason in [
            OwnerRecoveryReason::Displaced,
            OwnerRecoveryReason::SameOwnerFailure,
        ] {
            let store = RunStateStore::new(super::super::FakeEnv::default());
            store.observe(ServiceHealth::Ready);
            mark_service_unavailable_after_owner_loss(&store, reason);
            assert!(store.state().service_usable());
            assert_eq!(store.state().health, ServiceHealth::Ready);
        }
        let store = RunStateStore::new(super::super::FakeEnv::default());
        store.observe(ServiceHealth::Ready);
        mark_service_unavailable_after_owner_loss(&store, OwnerRecoveryReason::TransportFailure);
        assert!(!store.state().service_usable());
        assert!(
            matches!(store.state().health, ServiceHealth::Unavailable(reason) if reason.contains("service control IPC unavailable"))
        );
    }
}
