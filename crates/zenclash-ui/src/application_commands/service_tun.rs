//! TUN handover commands and confirmation of retained native runtime commits.

use super::{ApplicationCommands, recovery_record::ServiceRuntimeFacts, service_failure_message};
use zenclash_core::{ServiceTunOutcome, ServiceTunRequest};

impl ApplicationCommands {
    pub(crate) fn request_service_tun(&self) -> Result<ServiceTunRequest, String> {
        let manager = self
            .service_manager
            .as_ref()
            .ok_or_else(|| zenclash_i18n::text("core_page.service.unknown"))?;
        if self.session.runtime_descriptor().backend() == zenclash_core::CoreRuntimeBackend::Local
            && let Some(preparation) = self.service_local_recovery()
            && let Some(recovery) = preparation.recovery()
            && recovery.runtime().generation() == self.session.generation()
        {
            return manager
                .request_enable_tun_after_recovery(recovery.runtime())
                .map_err(service_failure_message);
        }
        manager
            .request_enable_tun()
            .map_err(service_failure_message)
    }

    pub(crate) async fn enable_service_tun(
        &self,
        request: ServiceTunRequest,
    ) -> Result<ServiceTunOutcome, String> {
        let manager = self
            .service_manager
            .clone()
            .ok_or_else(|| zenclash_i18n::text("core_page.service.unknown"))?;
        let commands = self.clone();
        // Navigation can discard its waiter, but not acceptance of a durable
        // core receipt in the shared application recovery record.
        tokio::spawn(async move {
            let outcome = manager
                .enable_tun(request)
                .await
                .map_err(service_failure_message)?;
            if let Some(core) = outcome.core() {
                commands.recovery_record.accept_service_runtime(
                    ServiceRuntimeFacts::new(
                        core,
                        outcome.commit_pending(),
                        outcome.recovery_warning(),
                    ),
                    commands.session.generation(),
                );
            }
            Ok(outcome)
        })
        .await
        .map_err(|_| zenclash_i18n::text("core_page.service.pending"))?
    }

    pub(crate) async fn confirm_service_runtime(&self, version: u64) -> Result<(), String> {
        // The retained outcome identifies a durable profile commit. Ordinary mode
        // and settings changes can advance CoreSession without replacing that commit.
        // Native reconciliation verifies the service revision; only a newer outcome
        // may supersede this confirmation token.
        if self.pending_finalization() != Some(version) {
            return Err(zenclash_i18n::text("profiles.recovery.confirm_stale"));
        }
        self.client()
            .reconcile_service_runtime()
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "profiles.recovery.confirm_failed",
                    &[("error", error.to_string())],
                )
            })?;
        if !self.recovery_record.confirm_service_commit(version) {
            return Err(zenclash_i18n::text("profiles.recovery.confirm_stale"));
        }
        Ok(())
    }
}
