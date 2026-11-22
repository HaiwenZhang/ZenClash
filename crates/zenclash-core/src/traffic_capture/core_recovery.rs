//! Bridges [`TrafficCaptureSession`] into [`CoreSession`] recovery and automatic
//! network hooks so `traffic_capture -> core_session` remains strictly one-way.

use std::sync::Arc;

use tokio::runtime::Handle;

use super::{CaptureOutcome, TrafficCaptureError, TrafficCaptureSession};
use crate::{
    CoreSession, CoreSessionError,
    core_session::{CoreRecoveryCapture, CoreRecoveryHookFuture},
};

impl CoreRecoveryCapture for TrafficCaptureSession {
    fn release_owned(&self) -> CoreRecoveryHookFuture<'_> {
        Box::pin(async move { require_reconciled(self.release_owned().await?) })
    }

    fn reconcile(&self) -> CoreRecoveryHookFuture<'_> {
        Box::pin(async move { require_reconciled(self.reconcile().await?) })
    }
}

fn require_reconciled(outcome: CaptureOutcome) -> Result<(), TrafficCaptureError> {
    match outcome {
        CaptureOutcome::ReconcileNeeded { failure, .. } => {
            Err(TrafficCaptureError::ReconciliationRequired { detail: failure })
        }
        _ => Ok(()),
    }
}

impl CoreSession {
    /// Stops the actual owned core after link loss while retaining its accepted runtime.
    ///
    /// # Errors
    /// Returns capture release, stale admission, native Stop confirmation or shutdown errors.
    pub async fn suspend_for_network(
        &self,
        capture: &TrafficCaptureSession,
    ) -> Result<bool, CoreSessionError> {
        self.suspend_for_network_with_capture(Arc::new(capture.clone()))
            .await
    }

    /// Resumes the accepted runtime after link loss and restores capture intent.
    ///
    /// # Errors
    /// Returns restart confirmation, capture, stale admission or shutdown failures.
    pub async fn resume_after_network(
        &self,
        capture: &TrafficCaptureSession,
    ) -> Result<bool, CoreSessionError> {
        self.resume_after_network_with_capture(Arc::new(capture.clone()))
            .await
    }

    /// Starts managed-core supervision with capture release and restoration hooks.
    ///
    /// On an unexpected exit, owned native capture is released before restart;
    /// after controller readiness succeeds, persistent intent is reconciled.
    /// Returns false for an external core or when supervision was already started.
    #[must_use]
    pub fn start_supervisor_with_capture(
        &self,
        runtime: &Handle,
        capture: TrafficCaptureSession,
    ) -> bool {
        self.start_supervisor_with_recovery_capture(runtime, Arc::new(capture))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ControlledConfigStore, CoreKind, CoreSession, MihomoClient, MihomoEndpoint};

    #[tokio::test]
    async fn a_capture_reconciliation_requirement_survives_the_core_recovery_seam() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let root = loop {
            let path = std::env::temp_dir().join(format!(
                "zenclash-capture-hook-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => break path,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("cannot reserve fixture directory: {error}"),
            }
        };
        let core = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::new(MihomoEndpoint::local_ipc(&root).unwrap()).unwrap(),
        )
        .unwrap();
        let proxy = crate::SystemProxySession::new(
            crate::AppPreferencesStore::new(root.join("preferences.json")),
            crate::SystemProxyController::default(),
        );
        let capture = TrafficCaptureSession::new(
            core,
            ControlledConfigStore::new(std::env::temp_dir()),
            Some(proxy),
            None,
        );
        let error = CoreRecoveryCapture::reconcile(&capture).await.unwrap_err();
        assert!(
            matches!(error, TrafficCaptureError::ReconciliationRequired { detail } if !detail.is_empty())
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
