//! Adapts capture outcomes to the existing core recovery loop without erasing operation errors.

use crate::{CaptureOutcome, TrafficCaptureError, TrafficCaptureSession};
use std::{future::Future, pin::Pin};

pub(super) type CoreRecoveryHookFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), TrafficCaptureError>> + Send + 'a>>;

pub(super) trait CoreRecoveryCapture: Send + Sync {
    fn release_owned(&self) -> CoreRecoveryHookFuture<'_>;
    fn reconcile(&self) -> CoreRecoveryHookFuture<'_>;
}

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
