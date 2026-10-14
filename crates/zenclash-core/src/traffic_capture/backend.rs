//! Adapts existing core and native-proxy owners to capture operations.

use super::{
    CaptureBackend, CaptureBackendSnapshot, CaptureFuture, TrafficCaptureError, TrafficCaptureTask,
    task_failure,
};
use crate::core_session::RuntimeConfigurationRevision;
use crate::{
    ControlledConfigStore, CoreSession, EffectiveConfigIntent, SystemProxySession,
    SystemProxySessionSnapshot, YamlOverrideStore,
};
use parking_lot::RwLock;
use std::{path::PathBuf, pin::Pin, sync::Arc};

pub(super) struct ProductionCaptureBackend {
    core_session: CoreSession,
    controlled_config: ControlledConfigStore,
    system_proxy: Option<SystemProxySession>,
    active_profile: Arc<RwLock<Option<PathBuf>>>,
}

impl ProductionCaptureBackend {
    pub(super) fn new(
        core_session: CoreSession,
        controlled_config: ControlledConfigStore,
        system_proxy: Option<SystemProxySession>,
        active_profile: Arc<RwLock<Option<PathBuf>>>,
    ) -> Self {
        Self {
            core_session,
            controlled_config,
            system_proxy,
            active_profile,
        }
    }
}

impl CaptureBackend for ProductionCaptureBackend {
    fn controlled_store(&self) -> Option<ControlledConfigStore> {
        Some(self.controlled_config.clone())
    }

    fn owns_service(&self, service: &Arc<crate::service::ServiceSession>) -> bool {
        self.core_session
            .client()
            .service_client()
            .is_some_and(|current| Arc::ptr_eq(&current, service))
    }
    fn validate_service_tun_request(
        &self,
        expected_runtime: RuntimeConfigurationRevision,
    ) -> Result<(), crate::CoreSessionError> {
        self.core_session.ensure_running_operations_allowed()?;
        if self.core_session.runtime_descriptor().binding_generation()
            != expected_runtime.binding_generation
            || self.core_session.generation() != expected_runtime.configuration_generation
        {
            return Err(crate::MihomoError::StaleBinding.into());
        }
        Ok(())
    }

    fn enable_service_tun(
        &self,
        service: Option<(Arc<crate::service::ServiceSession>, PathBuf)>,
        expected_runtime: RuntimeConfigurationRevision,
        recovery_bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
        prepared_config: Option<Arc<crate::core_session::service_tun::PreparedServiceTun>>,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        crate::core_session::service_tun::ServiceTunRuntimeOutcome,
                        crate::CoreSessionError,
                    >,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let profile = self.active_profile.read().clone();
            let controlled = prepared_config
                .as_ref()
                .and_then(|prepared| prepared.authorized_store.clone())
                .unwrap_or_else(|| self.controlled_config.clone());
            self.core_session
                .enable_service_tun_admitted(
                    &controlled,
                    service,
                    expected_runtime,
                    profile,
                    recovery_bundle,
                    prepared_config,
                )
                .await
        })
    }

    fn snapshot(&self) -> CaptureFuture<'_, CaptureBackendSnapshot> {
        Box::pin(async move {
            let core = self.core_session.snapshot();
            let offline = self.core_session.is_offline_recovery();
            let config = if offline {
                Err(TrafficCaptureError::OfflineRecovery)
            } else {
                self.core_session
                    .client()
                    .runtime_config()
                    .await
                    .map_err(TrafficCaptureError::from)
            };
            let system_proxy = read_system_proxy(self.system_proxy.clone()).await;
            let (tun, system_proxy_port) = match config {
                Ok(config) => {
                    let tun = crate::operational_status::observe_runtime_tun(
                        &self.core_session,
                        core.kind,
                        config.clone(),
                    )
                    .await;
                    (
                        tun.map_err(TrafficCaptureError::ObservationUnavailable),
                        config.system_proxy_port(),
                    )
                }
                Err(error) => (Err(error), None),
            };
            Ok(CaptureBackendSnapshot {
                system_proxy,
                tun,
                system_proxy_port,
                core_available: !offline && (!core.managed || core.running == Some(true)),
            })
        })
    }

    fn resync_runtime_profile(&self) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            let profile = self
                .active_profile
                .read()
                .clone()
                .ok_or(TrafficCaptureError::MissingProfile)?;
            let overrides =
                tokio::task::spawn_blocking(|| YamlOverrideStore::discover()?.load_enabled_paths())
                    .await
                    .map_err(|source| task_failure(TrafficCaptureTask::LoadOverrides, source))?
                    .map_err(TrafficCaptureError::from)?;
            self.core_session
                .apply(
                    &self.controlled_config,
                    EffectiveConfigIntent::ActivateProfile { profile, overrides },
                )
                .await
                .map(|_| ())
                .map_err(TrafficCaptureError::from)
        })
    }

    fn set_system_proxy(&self, enabled: bool, port: u16) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            let session = self
                .system_proxy
                .clone()
                .ok_or(TrafficCaptureError::SystemProxyUnavailable)?;
            tokio::task::spawn_blocking(move || session.set_enabled(enabled, port))
                .await
                .map_err(|source| task_failure(TrafficCaptureTask::ApplySystemProxy, source))?
                .map(|_| ())
                .map_err(TrafficCaptureError::from)
        })
    }

    fn set_tun(&self, enabled: bool) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            let profile = self
                .active_profile
                .read()
                .clone()
                .ok_or(TrafficCaptureError::MissingProfile)?;
            let overrides =
                tokio::task::spawn_blocking(|| YamlOverrideStore::discover()?.load_enabled_paths())
                    .await
                    .map_err(|source| task_failure(TrafficCaptureTask::LoadOverrides, source))?
                    .map_err(TrafficCaptureError::from)?;
            let patch = if enabled {
                serde_json::json!({"tun": {"enable": true}})
            } else {
                serde_json::json!({"tun": {"enable": false}})
            };
            self.core_session
                .apply(
                    &self.controlled_config,
                    EffectiveConfigIntent::Patch {
                        profile,
                        patch,
                        overrides,
                    },
                )
                .await
                .map(|_| ())
                .map_err(TrafficCaptureError::from)
        })
    }

    fn ensure_tun_permission(&self) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            self.core_session
                .ensure_tun_permission()
                .await
                .map_err(TrafficCaptureError::from)
        })
    }

    fn close_owned_connections(&self) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            let client = self
                .core_session
                .client()
                .pin_binding()
                .map_err(TrafficCaptureError::from)?;
            if client.owned_core().is_none() || self.core_session.snapshot().running == Some(false)
            {
                return Ok(());
            }
            client
                .close_all_connections()
                .await
                .map_err(TrafficCaptureError::from)
        })
    }

    fn reconcile(&self) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            let Some(session) = self.system_proxy.clone() else {
                return Ok(());
            };
            let core = self.core_session.snapshot();
            let running = (!core.managed || core.running == Some(true))
                && !self.core_session.is_offline_recovery()
                && !self.core_session.is_shutting_down();
            let port = if running {
                self.core_session
                    .client()
                    .runtime_config()
                    .await
                    .map_err(TrafficCaptureError::from)?
                    .system_proxy_port()
            } else {
                None
            };
            tokio::task::spawn_blocking(move || session.reconcile(running, port))
                .await
                .map_err(|source| task_failure(TrafficCaptureTask::ReconcileSystemProxy, source))?
                .map(|_| ())
                .map_err(TrafficCaptureError::from)
        })
    }

    fn release_owned(&self) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            let Some(session) = self.system_proxy.clone() else {
                return Ok(());
            };
            let result = tokio::task::spawn_blocking(move || session.release_owned())
                .await
                .map_err(|source| task_failure(TrafficCaptureTask::ReleaseSystemProxy, source))?;
            match result {
                Ok(_) => Ok(()),
                Err(crate::SystemProxySessionError::ReleasePersistence(error)) => {
                    tracing::warn!(%error, "owned system proxy released but ownership metadata could not be cleared");
                    Ok(())
                }
                Err(error) => Err(error.into()),
            }
        })
    }
}

async fn read_system_proxy(
    session: Option<SystemProxySession>,
) -> Result<SystemProxySessionSnapshot, TrafficCaptureError> {
    let session = session.ok_or(TrafficCaptureError::SystemProxyUnavailable)?;
    tokio::task::spawn_blocking(move || session.snapshot())
        .await
        .map_err(|source| task_failure(TrafficCaptureTask::ReadSystemProxy, source))?
        .map_err(TrafficCaptureError::from)
}
