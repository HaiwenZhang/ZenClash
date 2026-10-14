use super::*;

mod snapshot;
pub(super) use snapshot::*;

pub(super) trait NativeProxyBackend: Send + Sync + std::fmt::Debug {
    fn detect_service(&self) -> MihomoResult<String>;
    fn capture(&self, target: NativeProxyTarget<'_>) -> MihomoResult<NativeProxySnapshot>;
    fn display_summary(&self, observed: &NativeProxySnapshot) -> MihomoResult<SystemProxySummary> {
        Ok(observed.summary().clone())
    }
    fn apply(
        &self,
        expected: &NativeProxySnapshot,
        change: &NativeProxyChange<'_>,
    ) -> NativeWriteAttempt;
    fn restore(
        &self,
        expected: &NativeProxySnapshot,
        previous: &NativeProxySnapshot,
    ) -> NativeWriteAttempt;
}

struct ManualProxySettings {
    host: String,
    port: std::num::NonZeroU16,
    bypass: Vec<String>,
}
impl ManualProxySettings {
    fn new(settings: &SystemProxySettings, port: u16) -> MihomoResult<Self> {
        Ok(Self {
            host: normalize_system_proxy_host(&settings.host)?,
            port: std::num::NonZeroU16::new(port).ok_or_else(|| {
                invalid_setting(
                    SystemProxySetting::Port,
                    SystemProxyValidationError::ZeroPort,
                )
            })?,
            bypass: normalize_system_proxy_bypass(&settings.bypass)?,
        })
    }
}

#[derive(Debug)]
pub(super) struct PlatformProxyBackend;

impl NativeProxyBackend for PlatformProxyBackend {
    fn detect_service(&self) -> MihomoResult<String> {
        platform::detect()
    }
    fn capture(&self, target: NativeProxyTarget<'_>) -> MihomoResult<NativeProxySnapshot> {
        platform::capture(target)
    }
    fn apply(
        &self,
        expected: &NativeProxySnapshot,
        change: &NativeProxyChange<'_>,
    ) -> NativeWriteAttempt {
        platform::apply(expected, change)
    }
    fn restore(
        &self,
        expected: &NativeProxySnapshot,
        previous: &NativeProxySnapshot,
    ) -> NativeWriteAttempt {
        platform::restore(expected, previous)
    }
}

#[derive(Clone, Debug)]
pub(super) struct NativeAcceptedOwnership {
    claim: SystemProxyOwnership,
    snapshot: NativeProxySnapshot,
}

#[derive(Clone, Debug)]
pub(super) struct NativeRecovery {
    entries: Vec<NativeTransactionEntry>,
}

#[derive(Clone, Debug)]
struct NativeTransactionEntry {
    previous: NativeProxySnapshot,
    witness: NativeMutationWitness,
    observed: Option<NativeProxySnapshot>,
    attempted_restore: bool,
}

impl NativeTransactionEntry {
    fn new(previous: NativeProxySnapshot) -> Self {
        Self {
            witness: NativeMutationWitness::new(&previous),
            previous,
            observed: None,
            attempted_restore: false,
        }
    }
    fn accept_attempt(&mut self, attempt: NativeWriteAttempt) -> MihomoResult<NativeProxySnapshot> {
        self.witness.append(attempt.witness);
        let actual = attempt.result?;
        if !self.witness.permits(&actual)? {
            return Err(verification_failure());
        }
        self.observed = Some(actual.clone());
        Ok(actual)
    }
    fn permits_retry(&self, actual: &NativeProxySnapshot) -> MihomoResult<bool> {
        if self.previous.same_state(actual)? {
            return Ok(true);
        }
        self.observed
            .as_ref()
            .map_or(Ok(false), |observed| observed.same_state(actual))
    }
}

#[must_use]
pub(super) struct NativeProxyTransaction<'a> {
    controller: &'a SystemProxyController,
    entries: Vec<NativeTransactionEntry>,
    candidate: Option<pac::RunningPacServer>,
    pub(super) ownership: Option<SystemProxyOwnership>,
}

impl NativeProxyTransaction<'_> {
    pub(super) fn commit(self) {
        *self.controller.accepted_native.lock() = self.ownership.as_ref().and_then(|claim| {
            self.entries
                .last()
                .and_then(|entry| entry.observed.clone())
                .map(|snapshot| NativeAcceptedOwnership {
                    claim: claim.clone(),
                    snapshot,
                })
        });
        if let Some(candidate) = self.candidate {
            self.controller.pac_server.commit(candidate);
        } else {
            self.controller.pac_server.stop();
        }
    }

    fn change(
        &mut self,
        index: usize,
        change: &NativeProxyChange<'_>,
    ) -> MihomoResult<NativeProxySnapshot> {
        let entry = &mut self.entries[index];
        let attempt = self.controller.native.apply(&entry.previous, change);
        entry.accept_attempt(attempt)
    }

    pub(super) fn rollback(mut self, cause: SystemProxyTransactionCause) -> SystemProxyError {
        let restored = self.controller.restore_entries(&mut self.entries, false);
        let recovery_error = match restored {
            Ok(()) => {
                return SystemProxyError::RecoveryConfirmed {
                    cause: Box::new(cause),
                };
            }
            Err(error) => error,
        };
        let released = self.entries.iter_mut().try_for_each(|entry| {
            let actual = self
                .controller
                .native
                .capture(NativeProxyTarget::Captured(&entry.previous))?;
            if actual.same_state(&entry.previous)? || !actual.summary().active() {
                return Ok(());
            }
            if !entry.witness.permits(&actual)? {
                return Err(ownership_changed());
            }
            let attempt = self
                .controller
                .native
                .apply(&actual, &NativeProxyChange::Disable);
            let verified = entry.accept_attempt(attempt)?;
            if verified.summary().active() {
                return Err(verification_failure());
            }
            Ok(())
        });
        // Stored preferences may already equal the original while native Apply
        // or refresh failed. Keep the candidate until recovery is confirmed.
        if let Some(candidate) = self.candidate {
            self.controller.pac_server.retain_for_recovery(candidate);
        }
        for entry in &mut self.entries {
            // A failed read or an unexpected field gives no authority. In
            // particular, do not trust a whole observation just because an
            // earlier preflight happened to pass.
            entry.observed = self
                .controller
                .native
                .capture(NativeProxyTarget::Captured(&entry.previous))
                .ok()
                .filter(|actual| entry.witness.permits(actual).unwrap_or(false));
        }
        *self.controller.recovery.lock() = Some(NativeRecovery {
            entries: self.entries,
        });
        SystemProxyError::RecoveryUnconfirmed {
            cause: Box::new(cause),
            recovery: Box::new(recovery_error),
            release_failure: released.err().map(Box::new),
        }
    }
}

impl SystemProxyController {
    pub(super) fn current_native_claim(
        &self,
        persisted: Option<&SystemProxyOwnership>,
    ) -> Option<SystemProxyOwnership> {
        self.accepted_native
            .lock()
            .as_ref()
            .map(|accepted| accepted.claim.clone())
            .or_else(|| persisted.cloned())
    }

    pub(super) fn claim_targets_snapshot(
        &self,
        claim: &SystemProxyOwnership,
        actual: &NativeProxySnapshot,
    ) -> bool {
        self.accepted_native
            .lock()
            .as_ref()
            .filter(|accepted| accepted.claim == *claim)
            .map_or_else(
                || ownership_service(claim) == actual.summary().service,
                |accepted| accepted.snapshot.same_target(actual),
            )
    }

    pub(super) fn claim_matches_native(
        &self,
        claim: &SystemProxyOwnership,
        actual: &NativeProxySnapshot,
    ) -> MihomoResult<bool> {
        let accepted = self.accepted_native.lock();
        if let Some(accepted) = accepted
            .as_ref()
            .filter(|accepted| accepted.claim == *claim)
        {
            return Ok(
                accepted.snapshot.same_target(actual) && accepted.snapshot.same_state(actual)?
            );
        }
        Ok(
            system_proxy_summary_matches_claim(&actual.summary().service, actual.summary(), claim)
                && actual.legacy_release_is_safe(),
        )
    }

    fn capture_claimed_target(
        &self,
        claim: &SystemProxyOwnership,
    ) -> MihomoResult<NativeProxySnapshot> {
        let captured = self
            .accepted_native
            .lock()
            .as_ref()
            .filter(|accepted| accepted.claim == *claim)
            .map(|accepted| accepted.snapshot.clone());
        match captured {
            Some(captured) => self.native.capture(NativeProxyTarget::Captured(&captured)),
            None => self
                .native
                .capture(NativeProxyTarget::Service(ownership_service(claim))),
        }
    }

    fn restore_entries(
        &self,
        entries: &mut [NativeTransactionEntry],
        retry: bool,
    ) -> MihomoResult<()> {
        for entry in entries.iter_mut() {
            entry.attempted_restore = false;
        }
        let current = entries
            .iter()
            .map(|entry| {
                let actual = self
                    .native
                    .capture(NativeProxyTarget::Captured(&entry.previous))?;
                let permitted = if retry {
                    entry.permits_retry(&actual)?
                } else {
                    entry.witness.permits(&actual)?
                };
                if !permitted {
                    return Err(ownership_changed());
                }
                Ok(actual)
            })
            .collect::<MihomoResult<Vec<_>>>()?;
        // Every target passed before any restore. The adapter checks again
        // immediately before its actual writes (inside the macOS native lock).
        for (entry, actual) in entries.iter_mut().zip(current).rev() {
            if !entry.witness.has_steps() && entry.previous.same_state(&actual)? {
                continue;
            }
            let attempt = self.native.restore(&actual, &entry.previous);
            entry.attempted_restore = attempt.witness.has_steps();
            let verified = entry.accept_attempt(attempt)?;
            if !entry.previous.same_state(&verified)? {
                return Err(verification_failure());
            }
        }
        Ok(())
    }

    pub(super) fn recover_native(&self) -> MihomoResult<()> {
        let Some(mut recovery) = self.recovery.lock().clone() else {
            return Ok(());
        };
        if let Err(error) = self.restore_entries(&mut recovery.entries, true) {
            for entry in &mut recovery.entries {
                if !entry.attempted_restore {
                    continue;
                }
                entry.observed = self
                    .native
                    .capture(NativeProxyTarget::Captured(&entry.previous))
                    .ok()
                    .filter(|actual| entry.witness.permits(actual).unwrap_or(false));
            }
            *self.recovery.lock() = Some(recovery);
            return Err(error);
        }
        let keep_running = self.pac_server.status().is_some_and(|running| {
            recovery.entries.iter().any(|entry| {
                let previous = entry.previous.summary();
                previous.pac().is_enabled() && previous.pac().url() == running.url
            })
        });
        self.pac_server.discard_retained();
        if !keep_running {
            self.pac_server.stop();
        }
        *self.recovery.lock() = None;
        Ok(())
    }

    pub(super) fn stage_native<'a>(
        &'a self,
        service: String,
        configuration: SystemProxyConfiguration<'_>,
        previous_ownership: Option<&SystemProxyOwnership>,
    ) -> MihomoResult<NativeProxyTransaction<'a>> {
        if self.recovery.lock().is_some() {
            return Err(pending_recovery());
        }
        let enabled = configuration.is_enabled();
        let manual = match &configuration.kind {
            SystemProxyConfigurationKind::Enabled { settings, port }
                if settings.mode == SystemProxyMode::Manual =>
            {
                Some(ManualProxySettings::new(settings, *port)?)
            }
            _ => None,
        };
        let actual = self.native.capture(NativeProxyTarget::Service(&service))?;
        let candidate = match &configuration.kind {
            SystemProxyConfigurationKind::Enabled { settings, port }
                if settings.mode == SystemProxyMode::Pac =>
            {
                Some(
                    self.pac_server
                        .prepare(&settings.host, &settings.pac_script, *port)?,
                )
            }
            _ => None,
        };
        let ownership = if !enabled {
            None
        } else if let Some(candidate) = &candidate {
            Some(SystemProxyOwnership::Pac {
                service: service.clone(),
                url: candidate.status().url.clone(),
            })
        } else {
            Some(SystemProxyOwnership::Manual {
                service: service.clone(),
                host: manual
                    .as_ref()
                    .ok_or_else(verification_failure)?
                    .host
                    .clone(),
                port: manual.as_ref().ok_or_else(verification_failure)?.port.get(),
                bypass: manual
                    .as_ref()
                    .ok_or_else(verification_failure)?
                    .bypass
                    .clone(),
            })
        };
        let mut transaction = NativeProxyTransaction {
            controller: self,
            entries: Vec::new(),
            candidate,
            ownership,
        };
        if let Some(ownership) = previous_ownership {
            let previous = self.capture_claimed_target(ownership)?;
            if !previous.same_target(&actual) {
                if system_proxy_summary_matches_claim(
                    ownership_service(ownership),
                    previous.summary(),
                    ownership,
                ) {
                    if !self.claim_matches_native(ownership, &previous)? {
                        return Err(ownership_changed());
                    }
                    transaction
                        .entries
                        .push(NativeTransactionEntry::new(previous));
                } else if status_references_owned_endpoint(previous.summary(), ownership) {
                    return Err(ownership_changed());
                }
            }
        }
        transaction
            .entries
            .push(NativeTransactionEntry::new(actual));
        // Complete preflight includes the destination before disabling the old target.
        for entry in &transaction.entries {
            if !self
                .native
                .capture(NativeProxyTarget::Captured(&entry.previous))?
                .same_state(&entry.previous)?
            {
                return Err(ownership_changed());
            }
        }
        let result = (|| {
            let index = transaction.entries.len() - 1;
            if index > 0 {
                let released = transaction.change(0, &NativeProxyChange::Disable)?;
                if released.summary().active() {
                    return Err(verification_failure());
                }
            }
            let candidate_url = transaction
                .candidate
                .as_ref()
                .map(|candidate| candidate.status().url.clone());
            let change = if !enabled {
                NativeProxyChange::Disable
            } else if let Some(url) = &candidate_url {
                NativeProxyChange::Pac { url }
            } else {
                NativeProxyChange::Manual {
                    host: &manual.as_ref().ok_or_else(verification_failure)?.host,
                    port: manual.as_ref().ok_or_else(verification_failure)?.port.get(),
                    bypass: &manual.as_ref().ok_or_else(verification_failure)?.bypass,
                }
            };
            let verified = transaction.change(index, &change)?;
            if (!enabled && verified.summary().active())
                || transaction.ownership.as_ref().is_some_and(|ownership| {
                    !system_proxy_summary_matches_claim(&service, verified.summary(), ownership)
                })
            {
                return Err(verification_failure());
            }
            Ok(())
        })();
        match result {
            Ok(()) => Ok(transaction),
            Err(error) => Err(transaction.rollback(error.into()).into()),
        }
    }
}

#[cfg(test)]
mod tests;

impl SystemProxyOperation<'_> {
    pub(super) fn stage_release(
        &self,
        ownership: Option<&SystemProxyOwnership>,
    ) -> MihomoResult<Option<NativeProxyTransaction<'_>>> {
        self.controller.recover_native()?;
        let Some(ownership) = ownership else {
            return Ok(None);
        };
        let actual = self.controller.capture_claimed_target(ownership)?;
        if !self.controller.claim_matches_native(ownership, &actual)? {
            if status_references_owned_endpoint(actual.summary(), ownership)
                || (actual.summary().pac().is_enabled()
                    && self
                        .controller
                        .pac_server
                        .owns_url(actual.summary().pac().url()))
            {
                return Err(ownership_changed());
            }
            self.controller.pac_server.stop();
            return Ok(None);
        }
        // Carry exactly the snapshot that granted release authority. Recapturing
        // here would authorize a foreign replacement as a fresh baseline.
        let mut transaction = NativeProxyTransaction {
            controller: self.controller,
            entries: vec![NativeTransactionEntry::new(actual)],
            candidate: None,
            ownership: None,
        };
        let result = transaction
            .change(0, &NativeProxyChange::Disable)
            .and_then(|actual| {
                if actual.summary().active() {
                    Err(verification_failure())
                } else {
                    Ok(())
                }
            });
        match result {
            Ok(()) => Ok(Some(transaction)),
            Err(error) => Err(transaction.rollback(error.into()).into()),
        }
    }
}

fn status_references_owned_endpoint(
    actual: &SystemProxySummary,
    ownership: &SystemProxyOwnership,
) -> bool {
    match ownership {
        SystemProxyOwnership::Pac { url, .. } => {
            actual.pac().is_enabled() && actual.pac().url() == *url
        }
        SystemProxyOwnership::Manual { host, port, .. } => {
            (actual.http().is_enabled()
                && actual.http().host() == *host
                && actual.http().port() == *port)
                || (actual.https().is_enabled()
                    && actual.https().host() == *host
                    && actual.https().port() == *port)
        }
    }
}

pub(super) fn ownership_service(ownership: &SystemProxyOwnership) -> &str {
    match ownership {
        SystemProxyOwnership::Manual { service, .. }
        | SystemProxyOwnership::Pac { service, .. } => service,
    }
}
