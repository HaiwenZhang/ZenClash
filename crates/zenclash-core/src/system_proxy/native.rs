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
}

#[derive(Debug)]
pub(super) struct PlatformProxyBackend;

#[derive(Clone, Debug)]
pub(super) struct NativeRecovery {
    services: Vec<String>,
    attempted: Option<SystemProxyOwnership>,
    previous: Option<SystemProxyOwnership>,
}

impl NativeProxyBackend for PlatformProxyBackend {
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
        let restored = self
            .previous
            .iter()
            .rev()
            .try_for_each(|status| self.controller.restore_native(status));
        if restored.is_ok() {
            return MihomoError::Process(zenclash_i18n::text_with(
                "system_proxy.errors.recovered_failure",
                &[("error", error.into())],
            ));
        }
        let released = self.previous.iter().try_for_each(|status| {
            self.controller
                .native
                .set_manual(&status.service, false, "", 0, &[])?;
            if self.controller.native.status(&status.service)?.active() {
                return Err(MihomoError::Process(zenclash_i18n::text(
                    "system_proxy.errors.verification",
                )));
            }
            Ok(())
        });
        if released.is_ok() {
            self.controller.pac_server.stop();
        } else {
            if let Some(candidate) = self.candidate {
                self.controller.pac_server.retain_for_recovery(candidate);
            }
            *self.controller.recovery.lock() = Some(NativeRecovery {
                services: self
                    .previous
                    .iter()
                    .map(|status| status.service.clone())
                    .collect(),
                attempted: self.ownership,
                previous: self.previous_ownership,
            });
        }
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
        if previous.auto_enabled {
            self.native.set_pac(&previous.service, &previous.auto_url)?;
        } else {
            self.native.set_manual(
                &previous.service,
                previous.enabled || previous.secure_enabled,
                &previous.server,
                previous.port,
                &previous.bypass,
            )?;
        }
        let actual = self.native.status(&previous.service)?;
        if actual.auto_enabled != previous.auto_enabled
            || actual.enabled != previous.enabled
            || actual.secure_enabled != previous.secure_enabled
            || (previous.auto_enabled && actual.auto_url != previous.auto_url)
            || (previous.enabled
                && (actual.server != previous.server || actual.port != previous.port))
            || (previous.secure_enabled
                && (actual.secure_server != previous.secure_server
                    || actual.secure_port != previous.secure_port))
            || ((previous.enabled || previous.secure_enabled) && actual.bypass != previous.bypass)
        {
            return Err(MihomoError::Process(zenclash_i18n::text(
                "system_proxy.errors.verification",
            )));
        }
        Ok(())
    }

    pub(super) fn recover_native(&self) -> MihomoResult<()> {
        let recovery = self.recovery.lock().clone();
        let Some(recovery) = recovery else {
            return Ok(());
        };
        for service in recovery.services {
            let actual = self.native.status(&service)?;
            let known_pac = actual.auto_enabled && self.pac_server.owns_url(&actual.auto_url);
            let ownerships = recovery
                .attempted
                .as_ref()
                .into_iter()
                .chain(recovery.previous.as_ref());
            let mut known_http = false;
            let mut known_https = false;
            for ownership in ownerships {
                if let SystemProxyOwnership::Manual {
                    service: owned_service,
                    host,
                    port,
                    ..
                } = ownership
                    && *owned_service == service
                {
                    known_http |= actual.enabled && actual.server == *host && actual.port == *port;
                    known_https |= actual.secure_enabled
                        && actual.secure_server == *host
                        && actual.secure_port == *port;
                }
            }
            if known_pac || known_http || known_https {
                if (actual.auto_enabled && !known_pac)
                    || (actual.enabled && !known_http)
                    || (actual.secure_enabled && !known_https)
                {
                    return Err(MihomoError::Process(zenclash_i18n::text(
                        "system_proxy.errors.pending_recovery",
                    )));
                }
                self.native.set_manual(&service, false, "", 0, &[])?;
                if self.native.status(&service)?.active() {
                    return Err(MihomoError::Process(zenclash_i18n::text(
                        "system_proxy.errors.verification",
                    )));
                }
            }
        }
        self.pac_server.stop();
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
