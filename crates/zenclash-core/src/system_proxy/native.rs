use super::*;

pub(super) trait NativeProxyBackend: Send + Sync + std::fmt::Debug {
    fn detect_service(&self) -> MihomoResult<String>;
    fn status(&self, service: &str) -> MihomoResult<SystemProxyStatus>;
    fn set_manual(
        &self,
        service: &str,
        enabled: bool,
        host: &str,
        port: u16,
        bypass: &[String],
    ) -> MihomoResult<()>;
    fn set_pac(&self, service: &str, url: &str) -> MihomoResult<()>;
    fn restore_snapshot(&self, previous: &SystemProxyStatus) -> MihomoResult<()>;
}

#[derive(Debug)]
pub(super) struct PlatformProxyBackend;

#[derive(Clone, Debug)]
pub(super) struct NativeRecovery {
    snapshots: Vec<RecoverySnapshot>,
    attempted: Option<SystemProxyOwnership>,
    previous_ownership: Option<SystemProxyOwnership>,
}

#[derive(Clone, Debug)]
struct RecoverySnapshot {
    previous: SystemProxyStatus,
    observed: Option<SystemProxyStatus>,
}

impl NativeProxyBackend for PlatformProxyBackend {
    fn restore_snapshot(&self, previous: &SystemProxyStatus) -> MihomoResult<()> {
        platform::restore_snapshot(previous)
    }

    fn detect_service(&self) -> MihomoResult<String> {
        SystemProxyManager::detect().map(|manager| manager.service)
    }

    fn status(&self, service: &str) -> MihomoResult<SystemProxyStatus> {
        SystemProxyManager {
            service: service.into(),
        }
        .status()
    }

    fn set_manual(
        &self,
        service: &str,
        enabled: bool,
        host: &str,
        port: u16,
        bypass: &[String],
    ) -> MihomoResult<()> {
        SystemProxyManager {
            service: service.into(),
        }
        .set_enabled_with_bypass(enabled, host, port, bypass)
    }

    fn set_pac(&self, service: &str, url: &str) -> MihomoResult<()> {
        SystemProxyManager {
            service: service.into(),
        }
        .set_pac_enabled(true, url)
    }
}

#[must_use]
pub(super) struct NativeProxyTransaction<'a> {
    controller: &'a SystemProxyController,
    previous: Vec<SystemProxyStatus>,
    candidate: Option<pac::RunningPacServer>,
    pub(super) ownership: Option<SystemProxyOwnership>,
    previous_ownership: Option<SystemProxyOwnership>,
}

impl NativeProxyTransaction<'_> {
    pub(super) fn commit(self) {
        if let Some(candidate) = self.candidate {
            self.controller.pac_server.commit(candidate);
        } else {
            self.controller.pac_server.stop();
        }
    }

    pub(super) fn rollback(self, error: &str) -> MihomoError {
        let permitted = self.previous.iter().try_for_each(|previous| {
            let actual = self.controller.native.status(&previous.service)?;
            if !self.controller.can_restore_native(
                previous,
                &actual,
                None,
                self.ownership.as_ref(),
                self.previous_ownership.as_ref(),
            ) {
                return Err(MihomoError::Process(zenclash_i18n::text(
                    "system_proxy.errors.pending_recovery",
                )));
            }
            Ok(())
        });
        let preflight_allowed = permitted.is_ok();
        let restored = permitted.and_then(|()| {
            self.previous
                .iter()
                .rev()
                .try_for_each(|status| self.controller.restore_native(status))
        });
        if restored.is_ok() {
            return MihomoError::Process(zenclash_i18n::text_with(
                "system_proxy.errors.recovered_failure",
                &[("error", error.into())],
            ));
        }
        let released = self.previous.iter().try_for_each(|previous| {
            let actual = self.controller.native.status(&previous.service)?;
            if actual == *previous || !actual.active() {
                return Ok(());
            }
            if !self.controller.can_restore_native(
                previous,
                &actual,
                None,
                self.ownership.as_ref(),
                self.previous_ownership.as_ref(),
            ) {
                return Err(MihomoError::Process(zenclash_i18n::text(
                    "system_proxy.errors.pending_recovery",
                )));
            }
            self.controller
                .native
                .set_manual(&previous.service, false, "", 0, &[])?;
            if self.controller.native.status(&previous.service)?.active() {
                return Err(MihomoError::Process(zenclash_i18n::text(
                    "system_proxy.errors.verification",
                )));
            }
            Ok(())
        });
        if released.is_err()
            && let Some(candidate) = self.candidate
        {
            self.controller.pac_server.retain_for_recovery(candidate);
        }
        // A safety disable is not restoration. Keep the original configuration
        // and any old PAC listener it needs until a verified retry succeeds.
        *self.controller.recovery.lock() = Some(NativeRecovery {
            snapshots: self
                .previous
                .into_iter()
                .map(|previous| {
                    let observed = self
                        .controller
                        .native
                        .status(&previous.service)
                        .ok()
                        .filter(|actual| {
                            preflight_allowed
                                || self.controller.can_restore_native(
                                    &previous,
                                    actual,
                                    None,
                                    self.ownership.as_ref(),
                                    self.previous_ownership.as_ref(),
                                )
                        });
                    RecoverySnapshot { observed, previous }
                })
                .collect(),
            attempted: self.ownership,
            previous_ownership: self.previous_ownership,
        });
        MihomoError::Process(zenclash_i18n::text_with(
            "system_proxy.errors.recovery_failure",
            &[
                ("error", error.into()),
                (
                    "recovery",
                    restored.expect_err("failed recovery").to_string(),
                ),
                (
                    "release",
                    released.map_or_else(|error| error.to_string(), |()| "OK".into()),
                ),
            ],
        ))
    }
}

impl SystemProxyController {
    fn restore_native(&self, previous: &SystemProxyStatus) -> MihomoResult<()> {
        self.native.restore_snapshot(previous)?;
        if self.native.status(&previous.service)? != *previous {
            return Err(MihomoError::Process(zenclash_i18n::text(
                "system_proxy.errors.verification",
            )));
        }
        Ok(())
    }

    fn can_restore_native(
        &self,
        previous: &SystemProxyStatus,
        actual: &SystemProxyStatus,
        observed: Option<&SystemProxyStatus>,
        attempted: Option<&SystemProxyOwnership>,
        previous_ownership: Option<&SystemProxyOwnership>,
    ) -> bool {
        if actual == previous {
            return true;
        }
        if let Some(observed) = observed {
            // Disabled endpoint caches and PAC URLs are external configuration
            // too. A retry may overwrite only the complete failure observation.
            if actual != observed {
                return false;
            }
            if !actual.active() {
                return true;
            }
        }
        let mut known_http = actual.server == previous.server && actual.port == previous.port;
        let mut known_https = actual.secure_server == previous.secure_server
            && actual.secure_port == previous.secure_port;
        let mut known_http_host = actual.server == previous.server;
        let mut known_http_port = actual.port == previous.port;
        let mut known_https_host = actual.secure_server == previous.secure_server;
        let mut known_https_port = actual.secure_port == previous.secure_port;
        let mut known_pac = actual.auto_url == previous.auto_url
            || self.pac_server.owns_url(&actual.auto_url)
            || (!actual.auto_enabled && actual.auto_url.is_empty());
        let mut known_bypass = actual.bypass == previous.bypass;
        for ownership in attempted.into_iter().chain(previous_ownership) {
            match ownership {
                SystemProxyOwnership::Manual {
                    service,
                    host,
                    port,
                    bypass,
                } if *service == actual.service => {
                    known_http |= actual.server == *host && actual.port == *port;
                    known_https |= actual.secure_server == *host && actual.secure_port == *port;
                    // GNOME writes host and port separately while mode is off.
                    known_http_host |= actual.server == *host;
                    known_http_port |= actual.port == *port;
                    known_https_host |= actual.secure_server == *host;
                    known_https_port |= actual.secure_port == *port;
                    known_bypass |= actual.bypass == *bypass;
                }
                SystemProxyOwnership::Pac { service, url } if *service == actual.service => {
                    known_pac |= actual.auto_url == *url;
                }
                _ => {}
            }
        }
        if observed.is_none()
            && !(known_http_host
                && known_http_port
                && known_https_host
                && known_https_port
                && known_pac
                && known_bypass)
        {
            return false;
        }
        (!actual.enabled || known_http)
            && (!actual.secure_enabled || known_https)
            && (!actual.auto_enabled || known_pac)
            && (!(actual.enabled || actual.secure_enabled) || known_bypass)
    }

    pub(super) fn recover_native(&self) -> MihomoResult<()> {
        let Some(mut recovery) = self.recovery.lock().clone() else {
            return Ok(());
        };
        // Validate every affected service before changing any of them.
        for snapshot in &recovery.snapshots {
            let actual = self.native.status(&snapshot.previous.service)?;
            if (snapshot.observed.is_none() && actual != snapshot.previous)
                || !self.can_restore_native(
                    &snapshot.previous,
                    &actual,
                    snapshot.observed.as_ref(),
                    recovery.attempted.as_ref(),
                    recovery.previous_ownership.as_ref(),
                )
            {
                return Err(MihomoError::Process(zenclash_i18n::text(
                    "system_proxy.errors.pending_recovery",
                )));
            }
        }
        for snapshot in recovery.snapshots.iter_mut().rev() {
            // Reapply even an equal snapshot: a failed native refresh or commit
            // cannot be declared recovered merely because its stored values match.
            if let Err(error) = self.restore_native(&snapshot.previous) {
                snapshot.observed = self.native.status(&snapshot.previous.service).ok();
                *self.recovery.lock() = Some(recovery);
                return Err(error);
            }
        }
        let keep_running = self.pac_server.status().is_some_and(|running| {
            recovery.snapshots.iter().any(|snapshot| {
                snapshot.previous.auto_enabled && snapshot.previous.auto_url == running.url
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
        enabled: bool,
        port: u16,
        settings: &SystemProxySettings,
        previous_ownership: Option<&SystemProxyOwnership>,
    ) -> MihomoResult<NativeProxyTransaction<'a>> {
        if self.recovery.lock().is_some() {
            return Err(MihomoError::Process(zenclash_i18n::text(
                "system_proxy.errors.pending_recovery",
            )));
        }
        let actual = self.native.status(&service)?;
        let candidate = if enabled && settings.mode == SystemProxyMode::Pac {
            Some(
                self.pac_server
                    .prepare(&settings.host, &settings.pac_script, port)?,
            )
        } else {
            None
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
                host: normalize_system_proxy_host(&settings.host)?,
                port,
                bypass: normalize_system_proxy_bypass(&settings.bypass)?,
            })
        };
        let mut transaction = NativeProxyTransaction {
            controller: self,
            previous: Vec::new(),
            candidate,
            ownership,
            previous_ownership: previous_ownership.cloned(),
        };
        if let Some(ownership) = previous_ownership {
            let old_service = ownership_service(ownership);
            if old_service != service {
                let previous = self.native.status(old_service)?;
                if system_proxy_status_matches_ownership(old_service, &previous, ownership) {
                    transaction.previous.push(previous);
                    let release = self
                        .native
                        .set_manual(old_service, false, "", 0, &[])
                        .and_then(|()| {
                            if self.native.status(old_service)?.active() {
                                return Err(MihomoError::Process(zenclash_i18n::text(
                                    "system_proxy.errors.verification",
                                )));
                            }
                            Ok(())
                        });
                    if let Err(error) = release {
                        return Err(transaction.rollback(&error.to_string()));
                    }
                } else if status_references_owned_endpoint(&previous, ownership) {
                    return Err(MihomoError::Process(zenclash_i18n::text(
                        "system_proxy.errors.pending_recovery",
                    )));
                }
            }
        }
        transaction.previous.push(actual);
        let result = (|| {
            if !enabled {
                self.native.set_manual(&service, false, "", 0, &[])?;
            } else if let Some(candidate) = &transaction.candidate {
                self.native.set_pac(&service, &candidate.status().url)?;
            } else {
                self.native
                    .set_manual(&service, true, &settings.host, port, &settings.bypass)?;
            }
            let actual = self.native.status(&service)?;
            if !enabled && actual.active() {
                return Err(MihomoError::Process(zenclash_i18n::text(
                    "system_proxy.errors.verification",
                )));
            }
            if transaction.ownership.as_ref().is_some_and(|ownership| {
                !system_proxy_status_matches_ownership(&service, &actual, ownership)
            }) {
                return Err(MihomoError::Process(zenclash_i18n::text(
                    "system_proxy.errors.verification",
                )));
            }
            Ok(())
        })();
        match result {
            Ok(()) => Ok(transaction),
            Err(error) => Err(transaction.rollback(&error.to_string())),
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
        let service = ownership_service(ownership);
        let actual = self.controller.native.status(service)?;
        if !system_proxy_status_matches_ownership(service, &actual, ownership) {
            if status_references_owned_endpoint(&actual, ownership)
                || (actual.auto_enabled && self.controller.pac_server.owns_url(&actual.auto_url))
            {
                return Err(MihomoError::Process(zenclash_i18n::text(
                    "system_proxy.errors.pending_recovery",
                )));
            }
            self.controller.pac_server.stop();
            return Ok(None);
        }
        let settings = SystemProxySettings {
            mode: SystemProxyMode::Manual,
            host: String::new(),
            bypass: Vec::new(),
            pac_script: String::new(),
        };
        self.controller
            .stage_native(service.into(), false, 0, &settings, None)
            .map(Some)
    }
}

fn status_references_owned_endpoint(
    actual: &SystemProxyStatus,
    ownership: &SystemProxyOwnership,
) -> bool {
    match ownership {
        SystemProxyOwnership::Pac { url, .. } => actual.auto_enabled && actual.auto_url == *url,
        SystemProxyOwnership::Manual { host, port, .. } => {
            (actual.enabled && actual.server == *host && actual.port == *port)
                || (actual.secure_enabled
                    && actual.secure_server == *host
                    && actual.secure_port == *port)
        }
    }
}

pub(super) fn ownership_service(ownership: &SystemProxyOwnership) -> &str {
    match ownership {
        SystemProxyOwnership::Manual { service, .. }
        | SystemProxyOwnership::Pac { service, .. } => service,
    }
}
