use super::*;

enum RuntimeSwitch {
    Local(Arc<MihomoProcess>),
    Service(Arc<zenclash_service::ServiceClient>, PathBuf),
    Direct(crate::MihomoEndpoint),
}

impl CoreSession {
    /// Reads prepared runtime identity without cloning the process owner or performing I/O.
    #[must_use]
    pub fn runtime_descriptor(&self) -> crate::CoreRuntimeDescriptor {
        self.client.runtime_descriptor()
    }

    /// Reads bounded stdout and stderr diagnostics from the actual local owner.
    /// Service logging remains owned by the native log monitor and returns no local output.
    ///
    /// # Errors
    /// Rejects a replaced binding or a failed background observation.
    pub async fn recent_runtime_logs(&self) -> crate::MihomoResult<Vec<String>> {
        let client = self.client.pin_binding()?;
        let logs = match client.owned_core() {
            Some(crate::owned_core::OwnedCore::Local(process)) => {
                tokio::task::spawn_blocking(move || process.recent_logs())
                    .await
                    .map_err(|error| MihomoError::Process(error.to_string()))?
            }
            _ => Vec::new(),
        };
        client.ensure_binding_current()?;
        Ok(logs)
    }

    /// Publishes a real local child to all shared clients after retiring the previous owner.
    ///
    /// # Errors
    /// Rejects shutdown, a different core kind, stale admission, or failed retirement.
    pub async fn switch_to_process(
        &self,
        process: Arc<MihomoProcess>,
    ) -> Result<u64, CoreSessionError> {
        self.switch_runtime(RuntimeSwitch::Local(process)).await
    }

    /// Publishes an authenticated Mihomo service owner using its ordinary resource source home.
    ///
    /// # Errors
    /// Rejects shutdown, unsupported cores, unresolved service transactions or failed retirement.
    pub async fn switch_to_service(
        &self,
        service: Arc<zenclash_service::ServiceClient>,
        source_home: PathBuf,
    ) -> Result<u64, CoreSessionError> {
        self.switch_runtime(RuntimeSwitch::Service(service, source_home))
            .await
    }

    /// Detaches managed ownership and publishes an external controller to every shared client.
    ///
    /// # Errors
    /// Rejects shutdown or an owner whose retirement cannot be confirmed.
    pub async fn switch_to_direct(
        &self,
        endpoint: crate::MihomoEndpoint,
    ) -> Result<u64, CoreSessionError> {
        self.switch_runtime(RuntimeSwitch::Direct(endpoint)).await
    }

    async fn switch_runtime(&self, target: RuntimeSwitch) -> Result<u64, CoreSessionError> {
        self.ensure_not_shutting_down()?;
        let session = self.clone();
        // Completion owns admission even when a caller drops its future during retirement.
        tokio::spawn(async move {
            let client = session.client.pin_binding()?;
            let mut scopes = client.write_scopes();
            if let RuntimeSwitch::Local(process) = &target {
                scopes.extend(process.write_scopes());
            }
            let _lease = DataWriteLease::shared_async(scopes)
                .await
                .map_err(|error| ControlledConfigError::Task(error.to_string()))?;
            let _transition = session.transition.clone().lock_owned().await;
            client.ensure_binding_current()?;
            session.ensure_not_shutting_down()?;
            let before = session.runtime_descriptor().binding_generation();
            let result = match target {
                RuntimeSwitch::Local(process) => client.switch_to_process(process).await,
                RuntimeSwitch::Service(service, source_home) => {
                    client.switch_to_service(service, source_home).await
                }
                RuntimeSwitch::Direct(endpoint) => client.switch_to_direct(endpoint).await,
            };
            if session.runtime_descriptor().binding_generation() != before {
                session.network_suspended.store(false, Ordering::Release);
                let mut lifecycle = session.lifecycle.write();
                *lifecycle = CoreLifecycleSnapshot::new(session.is_managed());
                if result.is_err() {
                    lifecycle.phase = CoreLifecyclePhase::Unknown;
                }
                drop(lifecycle);
                session.next_generation();
            }
            result?;
            Ok(session.generation())
        })
        .await
        .map_err(|error| ControlledConfigError::Task(error.to_string()))?
    }
}
