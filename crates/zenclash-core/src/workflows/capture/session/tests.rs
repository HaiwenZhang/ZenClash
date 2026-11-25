use std::{collections::VecDeque, sync::Mutex};

use crate::{CapabilityState, SystemProxyOwnershipState, SystemProxyStatus};

use super::*;

mod maintenance_recovery;
mod plans;
mod rollback;
mod service_tun;

struct BlockedWrite {
    entered: tokio::sync::oneshot::Sender<()>,
    release: Arc<(Mutex<bool>, std::sync::Condvar)>,
}

impl BlockedWrite {
    fn wait(self) {
        let _ = self.entered.send(());
        let (released, wake) = &*self.release;
        let (_guard, timeout) = wake
            .wait_timeout_while(
                released.lock().unwrap(),
                std::time::Duration::from_secs(5),
                |released| !*released,
            )
            .unwrap();
        assert!(
            !timeout.timed_out(),
            "native write fixture was never released"
        );
    }
}

struct ReleaseWrite(Arc<(Mutex<bool>, std::sync::Condvar)>);

impl Drop for ReleaseWrite {
    fn drop(&mut self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}

#[derive(Clone)]
pub(super) struct FakeBackend {
    state: Arc<Mutex<FakeState>>,
    blocked_write: Arc<Mutex<Option<BlockedWrite>>>,
    blocked_handover: Arc<Mutex<Option<BlockedWrite>>>,
    service_reply:
        Arc<Mutex<Option<crate::runtime::session::service_tun::ServiceTunRuntimeOutcome>>>,
    service_error: Arc<Mutex<Option<crate::CoreSessionError>>>,
    stop_unowned_error: Arc<Mutex<Option<crate::runtime::service::ipc::ServiceCallError>>>,
}

struct FakeState {
    system_proxy: SystemProxySessionSnapshot,
    system_proxy_port: Option<u16>,
    resynced_system_proxy_port: Option<u16>,
    tun_configured: bool,
    ignore_tun_writes: bool,
    core_available: bool,
    open_connections: usize,
    permission_error: Option<String>,
    proxy_failure: Option<crate::SystemProxySessionError>,
    permission_requests: usize,
    failures: VecDeque<&'static str>,
    operations: Vec<String>,
}

impl FakeBackend {
    #[cfg(all(test, feature = "service-ipc-tests"))]
    pub(super) fn unowned_for_test() -> Self {
        Self::new(false, SystemProxyOwnershipState::Unowned, false)
    }

    fn new(system_active: bool, ownership: SystemProxyOwnershipState, tun: bool) -> Self {
        Self {
            blocked_write: Arc::default(),
            blocked_handover: Arc::default(),
            service_reply: Arc::default(),
            service_error: Arc::default(),
            stop_unowned_error: Arc::default(),
            state: Arc::new(Mutex::new(FakeState {
                system_proxy: SystemProxySessionSnapshot {
                    intent_enabled: system_active,
                    actual: SystemProxyStatus {
                        enabled: system_active,
                        secure_enabled: system_active,
                        port: if system_active { 7890 } else { 0 },
                        secure_port: if system_active { 7890 } else { 0 },
                        ..SystemProxyStatus::default()
                    },
                    ownership,
                },
                system_proxy_port: Some(7890),
                resynced_system_proxy_port: Some(7890),
                tun_configured: tun,
                ignore_tun_writes: false,
                core_available: true,
                open_connections: 0,
                permission_error: None,
                proxy_failure: None,
                permission_requests: 0,
                failures: VecDeque::new(),
                operations: Vec::new(),
            })),
        }
    }

    fn fail_next(&self, operation: &'static str) {
        self.state.lock().unwrap().failures.push_back(operation);
    }

    fn operations(&self) -> Vec<String> {
        self.state.lock().unwrap().operations.clone()
    }

    fn set_core_available(&self, available: bool) {
        self.state.lock().unwrap().core_available = available;
    }

    fn permission_requests(&self) -> usize {
        self.state.lock().unwrap().permission_requests
    }

    fn should_fail(state: &mut FakeState, operation: &str) -> Result<(), TrafficCaptureError> {
        if state
            .failures
            .front()
            .is_some_and(|next| *next == operation)
        {
            state.failures.pop_front();
            Err(TrafficCaptureError::Controller(crate::MihomoError::Api {
                status: 503,
                message: format!("{operation} failed"),
            }))
        } else {
            Ok(())
        }
    }

    fn write_proxy(&self, enabled: bool, port: u16) -> Result<(), TrafficCaptureError> {
        let mut state = self.state.lock().unwrap();
        let operation = format!("system-proxy:{enabled}");
        state.operations.push(operation.clone());
        if let Some(error) = state.proxy_failure.take() {
            return Err(error.into());
        }
        Self::should_fail(&mut state, &operation)?;
        state.system_proxy.intent_enabled = enabled;
        state.system_proxy.actual.enabled = enabled;
        state.system_proxy.actual.secure_enabled = enabled;
        state.system_proxy.actual.port = if enabled { port } else { 0 };
        state.system_proxy.actual.secure_port = if enabled { port } else { 0 };
        state.system_proxy.ownership = if enabled {
            SystemProxyOwnershipState::Owned
        } else {
            SystemProxyOwnershipState::Unowned
        };
        Ok(())
    }
}

impl CaptureBackend for FakeBackend {
    fn stop_unowned_service(
        &self,
        _service: Arc<crate::runtime::service::ipc::ServiceSession>,
    ) -> Pin<
        Box<dyn Future<Output = Result<(), crate::runtime::service::ipc::ServiceCallError>> + Send>,
    > {
        self.state
            .lock()
            .unwrap()
            .operations
            .push("stop-unowned-service".into());
        let error = self.stop_unowned_error.lock().unwrap().take();
        Box::pin(async move {
            match error {
                Some(error) => Err(error),
                None => Ok(()),
            }
        })
    }

    fn enable_service_tun(
        &self,
        _service: Option<(Arc<crate::runtime::service::ipc::ServiceSession>, PathBuf)>,
        _expected_runtime: RuntimeConfigurationRevision,
        _recovery_bundle: Option<Arc<crate::ServiceRuntimeBundle>>,
        _prepared_config: Option<Arc<crate::runtime::session::service_tun::PreparedServiceTun>>,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        crate::runtime::session::service_tun::ServiceTunRuntimeOutcome,
                        crate::CoreSessionError,
                    >,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let blocked = self.blocked_handover.lock().unwrap().take();
            if let Some(blocked) = blocked {
                tokio::task::spawn_blocking(move || blocked.wait())
                    .await
                    .unwrap();
            }
            self.state
                .lock()
                .unwrap()
                .operations
                .push("service-handover".into());
            if let Some(error) = self.service_error.lock().unwrap().take() {
                return Err(error);
            }
            Ok(self.service_reply.lock().unwrap().take().unwrap_or(
                crate::runtime::session::service_tun::ServiceTunRuntimeOutcome {
                    saved: Some(crate::CoreApplyOutcome {
                        kind: crate::CoreApplyKind::Patched,
                        generation: 1,
                    }),
                    commit_pending: false,
                    recovery_warning: None,
                    failure: None,
                    restored: false,
                },
            ))
        })
    }

    fn snapshot(&self) -> CaptureFuture<'_, CaptureBackendSnapshot> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            if state
                .operations
                .iter()
                .any(|operation| operation == "service-handover")
            {
                Self::should_fail(&mut state, "capture-readback")?;
            }
            Ok(CaptureBackendSnapshot {
                system_proxy: Ok(state.system_proxy.clone()),
                tun: Ok(TunCaptureStatus {
                    requested: state.tun_configured,
                    configured: state.tun_configured,
                    permission: CapabilityState::Active,
                    runtime: crate::TunRuntimeObservation {
                        device_name: Some("test-tun".into()),
                        device: if state.tun_configured {
                            CapabilityState::Active
                        } else {
                            CapabilityState::Inactive
                        },
                        route: if state.tun_configured {
                            CapabilityState::Unknown
                        } else {
                            CapabilityState::Inactive
                        },
                        detail: "test observation".into(),
                    },
                    observed: if state.tun_configured {
                        CapabilityState::Unknown
                    } else {
                        CapabilityState::Inactive
                    },
                }),
                system_proxy_port: state.system_proxy_port,
                core_available: state.core_available,
            })
        })
    }

    fn resync_runtime_profile(&self) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.operations.push("resync-runtime-profile".into());
            state.system_proxy_port = state.resynced_system_proxy_port;
            Ok(())
        })
    }

    fn set_system_proxy(&self, enabled: bool, port: u16) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            let blocked = self.blocked_write.lock().unwrap().take();
            if let Some(blocked) = blocked {
                let backend = self.clone();
                return tokio::task::spawn_blocking(move || {
                    blocked.wait();
                    backend.write_proxy(enabled, port)
                })
                .await
                .unwrap();
            }
            self.write_proxy(enabled, port)
        })
    }

    fn set_tun(&self, enabled: bool) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            let operation = format!("tun:{enabled}");
            state.operations.push(operation.clone());
            Self::should_fail(&mut state, &operation)?;
            if !state.ignore_tun_writes {
                state.tun_configured = enabled;
            }
            Ok(())
        })
    }

    fn ensure_tun_permission(&self) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.permission_requests += 1;
            state.permission_error.clone().map_or(Ok(()), |message| {
                Err(TrafficCaptureError::Controller(
                    crate::MihomoError::InvalidInput(message),
                ))
            })
        })
    }

    fn close_owned_connections(&self) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.operations.push("close-connections".into());
            Self::should_fail(&mut state, "close-connections")?;
            state.open_connections = 0;
            Ok(())
        })
    }

    fn reconcile(&self) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.operations.push("reconcile".into());
            if state.system_proxy.intent_enabled && !state.core_available {
                if state.system_proxy.ownership == SystemProxyOwnershipState::Owned {
                    state.system_proxy.actual = SystemProxyStatus::default();
                    state.system_proxy.ownership = SystemProxyOwnershipState::Unowned;
                }
            } else if state.system_proxy.intent_enabled
                && !state.system_proxy.actual.active()
                && state.system_proxy.ownership == SystemProxyOwnershipState::Unowned
            {
                state.system_proxy.actual = SystemProxyStatus {
                    enabled: true,
                    secure_enabled: true,
                    port: 7890,
                    secure_port: 7890,
                    ..SystemProxyStatus::default()
                };
                state.system_proxy.ownership = SystemProxyOwnershipState::Owned;
            }
            Ok(())
        })
    }

    fn release_owned(&self) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.operations.push("release-owned".into());
            Self::should_fail(&mut state, "release-owned")?;
            if state.system_proxy.ownership == SystemProxyOwnershipState::Owned {
                state.system_proxy.actual = SystemProxyStatus::default();
            }
            state.system_proxy.ownership = SystemProxyOwnershipState::Unowned;
            Ok(())
        })
    }
}
