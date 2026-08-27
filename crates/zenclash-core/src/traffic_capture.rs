//! Transactional coordination of System Proxy and TUN capture plans.

use std::{
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use parking_lot::RwLock;
use thiserror::Error;

use crate::{
    CapabilityState, ControlledConfigStore, CoreKind, CoreMaintenanceIntent, CoreSession,
    EffectiveConfigIntent, Observation, RecoveryAction, SystemProxyOwnershipState,
    SystemProxySession, SystemProxySessionSnapshot, TunCaptureStatus, TunPermissionManager,
    YamlOverrideStore,
};

type CaptureFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// Mutually exclusive capture choice offered by ordinary UI surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapturePlan {
    /// Disable ZenClash-owned System Proxy and TUN capture.
    Off,
    /// Capture through the native desktop System Proxy.
    SystemProxy,
    /// Configure the runtime TUN path after checking platform permission.
    Tun,
}

/// Capture combination observed without normalizing pre-existing state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedCapturePlan {
    /// Both System Proxy and TUN are observed or configured off.
    Off,
    /// Native System Proxy is active while TUN is configured off.
    SystemProxy,
    /// TUN is configured on while native System Proxy is off.
    TunConfigured,
    /// System Proxy and TUN are both present; the state is preserved until an explicit choice.
    Advanced,
    /// One or more required observations are unavailable.
    Unknown,
}

/// Read-only capture state returned by [`TrafficCaptureSession`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrafficCaptureSnapshot {
    /// System Proxy intent, native readback, and ownership.
    pub system_proxy: Observation<SystemProxySessionSnapshot>,
    /// TUN configuration, permission, and runtime activation fact.
    pub tun: Observation<TunCaptureStatus>,
    /// Combination derived from trustworthy System Proxy and TUN values.
    pub observed_plan: ObservedCapturePlan,
    /// HTTP/Mixed listener available for native proxy capture.
    pub system_proxy_port: Option<u16>,
    /// Whether the current runtime core is available.
    pub core_available: bool,
}

impl TrafficCaptureSnapshot {
    fn from_backend(snapshot: CaptureBackendSnapshot) -> Self {
        let observed_at_ms = now_ms();
        let system_proxy = Observation::record(
            &Observation::Loading,
            snapshot.system_proxy,
            observed_at_ms,
            RecoveryAction::ReviewCapture,
        );
        let tun = Observation::record(
            &Observation::Loading,
            snapshot.tun,
            observed_at_ms,
            RecoveryAction::ReviewCapture,
        );
        let observed_plan = observed_capture_plan(&system_proxy, &tun);
        Self {
            system_proxy,
            tun,
            observed_plan,
            system_proxy_port: snapshot.system_proxy_port,
            core_available: snapshot.core_available,
        }
    }
}

/// Result of applying or releasing one capture plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// The requested plan was applied; the snapshot remains authoritative for activation facts.
    Applied {
        /// Explicit user plan.
        plan: CapturePlan,
        /// Readback after the operation.
        snapshot: TrafficCaptureSnapshot,
    },
    /// The requested plan already matched current state.
    Unchanged {
        /// Current capture snapshot.
        snapshot: TrafficCaptureSnapshot,
    },
    /// A later step failed and the earlier mutation was restored.
    RolledBack {
        /// Requested plan that did not complete.
        plan: CapturePlan,
        /// Original operation failure.
        failure: String,
        /// Readback after rollback.
        snapshot: TrafficCaptureSnapshot,
    },
    /// A partial operation or failed rollback requires explicit reconciliation.
    ReconcileNeeded {
        /// Requested plan, when the outcome followed an apply operation.
        plan: Option<CapturePlan>,
        /// Combined operation and rollback failure.
        failure: String,
        /// Best-effort readback after the uncertain transition.
        snapshot: TrafficCaptureSnapshot,
    },
}

impl CaptureOutcome {
    /// Returns the latest read-only capture snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &TrafficCaptureSnapshot {
        match self {
            Self::Applied { snapshot, .. }
            | Self::Unchanged { snapshot }
            | Self::RolledBack { snapshot, .. }
            | Self::ReconcileNeeded { snapshot, .. } => snapshot,
        }
    }
}

/// Failure before a capture transaction performed a recoverable partial mutation.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TrafficCaptureError {
    /// No active profile exists for a TUN configuration transition.
    #[error("没有可用于流量接管的活动 Profile")]
    MissingProfile,
    /// The selected plan would overwrite native proxy state owned by another application.
    #[error("系统代理由其他应用控制；请先在系统设置中处理后再切换接管方式")]
    ExternalSystemProxy,
    /// A runtime, platform, permission, or readback operation failed.
    #[error("流量接管操作失败：{0}")]
    Backend(String),
}

/// Deep module serializing capture plan changes and partial-failure recovery.
#[derive(Clone)]
pub struct TrafficCaptureSession {
    backend: Arc<dyn CaptureBackend>,
    profile: Arc<RwLock<Option<PathBuf>>>,
    operation: Arc<tokio::sync::Mutex<()>>,
}

impl TrafficCaptureSession {
    /// Creates a production capture session over the existing runtime and platform owners.
    #[must_use]
    pub fn new(
        core_session: CoreSession,
        controlled: ControlledConfigStore,
        system_proxy: Option<SystemProxySession>,
        tun_permissions: Option<TunPermissionManager>,
        profile: Option<PathBuf>,
    ) -> Self {
        let profile = Arc::new(RwLock::new(profile));
        let backend = Arc::new(ProductionCaptureBackend {
            core_session,
            controlled,
            system_proxy,
            tun_permissions,
            profile: profile.clone(),
        });
        Self {
            backend,
            profile,
            operation: Arc::default(),
        }
    }

    /// Updates the active profile used by subsequent TUN transitions.
    pub fn set_profile(&self, profile: Option<PathBuf>) {
        *self.profile.write() = profile;
    }

    /// Applies one ordinary capture plan with rollback after partial failure.
    ///
    /// # Errors
    ///
    /// Returns an error before partial mutation, including missing profiles,
    /// unavailable permissions, and external System Proxy ownership conflicts.
    pub async fn apply(&self, plan: CapturePlan) -> Result<CaptureOutcome, TrafficCaptureError> {
        let _operation = self.operation.lock().await;
        let before = self.snapshot_from_backend().await?;
        if plan_matches(plan, &before) {
            return Ok(CaptureOutcome::Unchanged { snapshot: before });
        }
        if plan != CapturePlan::SystemProxy && has_external_system_proxy(&before) {
            return Err(TrafficCaptureError::ExternalSystemProxy);
        }
        match plan {
            CapturePlan::Off => self.apply_off(before).await,
            CapturePlan::SystemProxy => self.apply_system_proxy(before).await,
            CapturePlan::Tun => self.apply_tun(before).await,
        }
    }

    /// Reconciles persistent System Proxy intent without normalizing an
    /// existing System Proxy + TUN advanced combination.
    ///
    /// # Errors
    ///
    /// Returns a backend error when no trustworthy snapshot can be produced.
    pub async fn reconcile(&self) -> Result<CaptureOutcome, TrafficCaptureError> {
        let _operation = self.operation.lock().await;
        if let Err(failure) = self.backend.reconcile().await {
            return Ok(CaptureOutcome::ReconcileNeeded {
                plan: None,
                snapshot: self.best_effort_snapshot().await,
                failure,
            });
        }
        Ok(CaptureOutcome::Unchanged {
            snapshot: self.snapshot_from_backend().await?,
        })
    }

    /// Releases only native state that still matches ZenClash ownership.
    ///
    /// # Errors
    ///
    /// Returns a backend error when release or readback fails.
    pub async fn release_owned(&self) -> Result<CaptureOutcome, TrafficCaptureError> {
        let _operation = self.operation.lock().await;
        if let Err(failure) = self.backend.release_owned().await {
            return Ok(CaptureOutcome::ReconcileNeeded {
                plan: None,
                snapshot: self.best_effort_snapshot().await,
                failure,
            });
        }
        Ok(CaptureOutcome::Unchanged {
            snapshot: self.snapshot_from_backend().await?,
        })
    }

    async fn apply_off(
        &self,
        before: TrafficCaptureSnapshot,
    ) -> Result<CaptureOutcome, TrafficCaptureError> {
        let system_was_owned = system_proxy_is_owned_and_active(&before);
        if system_proxy_has_intent_or_ownership(&before) {
            self.backend
                .set_system_proxy(false, 0)
                .await
                .map_err(TrafficCaptureError::Backend)?;
        }
        let tun_was_configured = tun_is_configured(&before);
        if tun_was_configured && let Err(failure) = self.backend.set_tun(false).await {
            let rollback = if system_was_owned {
                self.restore_system_proxy(&before).await
            } else {
                Ok(())
            };
            return Ok(self
                .failed_outcome(CapturePlan::Off, failure, rollback)
                .await);
        }
        self.applied(CapturePlan::Off).await
    }

    async fn apply_system_proxy(
        &self,
        before: TrafficCaptureSnapshot,
    ) -> Result<CaptureOutcome, TrafficCaptureError> {
        let port = before
            .system_proxy_port
            .ok_or_else(|| TrafficCaptureError::Backend("没有可用的 HTTP/Mixed 端口".into()))?;
        let tun_was_configured = tun_is_configured(&before);
        if tun_was_configured {
            self.backend
                .set_tun(false)
                .await
                .map_err(TrafficCaptureError::Backend)?;
        }
        if let Err(failure) = self.backend.set_system_proxy(true, port).await {
            let rollback = if tun_was_configured {
                self.backend.set_tun(true).await
            } else {
                Ok(())
            };
            return Ok(self
                .failed_outcome(CapturePlan::SystemProxy, failure, rollback)
                .await);
        }
        self.applied(CapturePlan::SystemProxy).await
    }

    async fn apply_tun(
        &self,
        before: TrafficCaptureSnapshot,
    ) -> Result<CaptureOutcome, TrafficCaptureError> {
        self.backend
            .ensure_tun_permission()
            .await
            .map_err(TrafficCaptureError::Backend)?;
        let tun_was_configured = tun_is_configured(&before);
        if !tun_was_configured {
            self.backend
                .set_tun(true)
                .await
                .map_err(TrafficCaptureError::Backend)?;
        }
        if system_proxy_has_intent_or_ownership(&before)
            && let Err(failure) = self.backend.set_system_proxy(false, 0).await
        {
            let rollback = if tun_was_configured {
                Ok(())
            } else {
                self.backend.set_tun(false).await
            };
            return Ok(self
                .failed_outcome(CapturePlan::Tun, failure, rollback)
                .await);
        }
        self.applied(CapturePlan::Tun).await
    }

    async fn restore_system_proxy(&self, before: &TrafficCaptureSnapshot) -> Result<(), String> {
        let port = before
            .system_proxy_port
            .or_else(|| {
                before
                    .system_proxy
                    .value()
                    .map(|snapshot| snapshot.actual.port)
                    .filter(|port| *port != 0)
            })
            .ok_or_else(|| "无法确定回滚 System Proxy 所需的端口".to_owned())?;
        self.backend.set_system_proxy(true, port).await
    }

    async fn applied(&self, plan: CapturePlan) -> Result<CaptureOutcome, TrafficCaptureError> {
        Ok(CaptureOutcome::Applied {
            plan,
            snapshot: self.snapshot_from_backend().await?,
        })
    }

    async fn failed_outcome(
        &self,
        plan: CapturePlan,
        failure: String,
        rollback: Result<(), String>,
    ) -> CaptureOutcome {
        let snapshot = self.best_effort_snapshot().await;
        match rollback {
            Ok(()) => CaptureOutcome::RolledBack {
                plan,
                failure,
                snapshot,
            },
            Err(rollback) => CaptureOutcome::ReconcileNeeded {
                plan: Some(plan),
                failure: format!("{failure}；回滚失败：{rollback}"),
                snapshot,
            },
        }
    }

    async fn snapshot_from_backend(&self) -> Result<TrafficCaptureSnapshot, TrafficCaptureError> {
        self.backend
            .snapshot()
            .await
            .map(TrafficCaptureSnapshot::from_backend)
            .map_err(TrafficCaptureError::Backend)
    }

    async fn best_effort_snapshot(&self) -> TrafficCaptureSnapshot {
        self.snapshot_from_backend()
            .await
            .unwrap_or_else(|error| failed_snapshot(error.to_string()))
    }

    #[cfg(test)]
    fn with_backend(backend: Arc<dyn CaptureBackend>) -> Self {
        Self {
            backend,
            profile: Arc::default(),
            operation: Arc::default(),
        }
    }
}

#[derive(Clone, Debug)]
struct CaptureBackendSnapshot {
    system_proxy: Result<SystemProxySessionSnapshot, String>,
    tun: Result<TunCaptureStatus, String>,
    system_proxy_port: Option<u16>,
    core_available: bool,
}

trait CaptureBackend: Send + Sync {
    fn snapshot(&self) -> CaptureFuture<'_, CaptureBackendSnapshot>;
    fn set_system_proxy(&self, enabled: bool, port: u16) -> CaptureFuture<'_, ()>;
    fn set_tun(&self, enabled: bool) -> CaptureFuture<'_, ()>;
    fn ensure_tun_permission(&self) -> CaptureFuture<'_, ()>;
    fn reconcile(&self) -> CaptureFuture<'_, ()>;
    fn release_owned(&self) -> CaptureFuture<'_, ()>;
}

struct ProductionCaptureBackend {
    core_session: CoreSession,
    controlled: ControlledConfigStore,
    system_proxy: Option<SystemProxySession>,
    tun_permissions: Option<TunPermissionManager>,
    profile: Arc<RwLock<Option<PathBuf>>>,
}

impl CaptureBackend for ProductionCaptureBackend {
    fn snapshot(&self) -> CaptureFuture<'_, CaptureBackendSnapshot> {
        Box::pin(async move {
            let core = self.core_session.snapshot();
            let config = self.core_session.client().runtime_config().await;
            let system_proxy = read_system_proxy(self.system_proxy.clone()).await;
            let (tun, system_proxy_port) = match config {
                Ok(config) => {
                    let tun = observe_tun(core.kind, &config, self.tun_permissions.clone()).await;
                    (tun, config.system_proxy_port())
                }
                Err(error) => (Err(error.to_string()), None),
            };
            Ok(CaptureBackendSnapshot {
                system_proxy,
                tun,
                system_proxy_port,
                core_available: core.running,
            })
        })
    }

    fn set_system_proxy(&self, enabled: bool, port: u16) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            let session = self
                .system_proxy
                .clone()
                .ok_or_else(|| "系统代理持久化状态不可用".to_owned())?;
            tokio::task::spawn_blocking(move || session.set_enabled(enabled, port))
                .await
                .map_err(|error| format!("系统代理任务异常结束：{error}"))?
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }

    fn set_tun(&self, enabled: bool) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            let profile = self
                .profile
                .read()
                .clone()
                .ok_or_else(|| TrafficCaptureError::MissingProfile.to_string())?;
            let overrides =
                tokio::task::spawn_blocking(|| YamlOverrideStore::discover()?.load_enabled_paths())
                    .await
                    .map_err(|error| format!("读取 YAML override 任务异常结束：{error}"))?
                    .map_err(|error| error.to_string())?;
            let patch = if enabled {
                serde_json::json!({"tun": {"enable": true}, "dns": {"enable": true}})
            } else {
                serde_json::json!({"tun": {"enable": false}})
            };
            self.core_session
                .apply(
                    &self.controlled,
                    EffectiveConfigIntent::Patch {
                        profile,
                        patch,
                        overrides,
                    },
                )
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }

    fn ensure_tun_permission(&self) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            let permissions = self
                .tun_permissions
                .clone()
                .ok_or_else(|| "当前 runtime 没有可授权的受管内核".to_owned())?;
            let already_granted = tokio::task::spawn_blocking(move || {
                let already_granted = permissions.status()?.granted;
                permissions.request_grant().map(|_| already_granted)
            })
            .await
            .map_err(|error| format!("TUN 授权任务异常结束：{error}"))?
            .map_err(|error| error.to_string())?;
            #[cfg(unix)]
            if !already_granted {
                self.core_session
                    .maintain(CoreMaintenanceIntent::Restart)
                    .await
                    .map_err(|error| format!("TUN 授权后重启内核失败：{error}"))?;
            }
            Ok(())
        })
    }

    fn reconcile(&self) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            let Some(session) = self.system_proxy.clone() else {
                return Ok(());
            };
            let core = self.core_session.snapshot();
            let port = if core.running {
                self.core_session
                    .client()
                    .runtime_config()
                    .await
                    .map_err(|error| error.to_string())?
                    .system_proxy_port()
            } else {
                None
            };
            tokio::task::spawn_blocking(move || session.reconcile(core.running, port))
                .await
                .map_err(|error| format!("系统代理恢复任务异常结束：{error}"))?
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }

    fn release_owned(&self) -> CaptureFuture<'_, ()> {
        Box::pin(async move {
            let Some(session) = self.system_proxy.clone() else {
                return Ok(());
            };
            tokio::task::spawn_blocking(move || session.release_owned())
                .await
                .map_err(|error| format!("系统代理释放任务异常结束：{error}"))?
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }
}

async fn read_system_proxy(
    session: Option<SystemProxySession>,
) -> Result<SystemProxySessionSnapshot, String> {
    let session = session.ok_or_else(|| "系统代理持久化状态不可用".to_owned())?;
    tokio::task::spawn_blocking(move || session.snapshot())
        .await
        .map_err(|error| format!("系统代理读取任务异常结束：{error}"))?
        .map_err(|error| error.to_string())
}

async fn observe_tun(
    kind: CoreKind,
    config: &crate::RuntimeConfig,
    permissions: Option<TunPermissionManager>,
) -> Result<TunCaptureStatus, String> {
    if kind == CoreKind::Meow {
        return Ok(TunCaptureStatus {
            requested: false,
            configured: false,
            permission: CapabilityState::Unsupported,
            runtime: crate::TunRuntimeObservation {
                device_name: None,
                device: CapabilityState::Unsupported,
                route: CapabilityState::Unsupported,
                detail: "meow-rs 未声明 TUN 能力".into(),
            },
            observed: CapabilityState::Unsupported,
        });
    }
    if !config.tun.enable {
        return Ok(TunCaptureStatus {
            requested: false,
            configured: false,
            permission: CapabilityState::Unknown,
            runtime: crate::TunRuntimeObserver::observe(&config.tun),
            observed: CapabilityState::Inactive,
        });
    }
    let permissions = permissions.ok_or_else(|| "TUN 已配置，但没有可检查的内核权限".to_owned())?;
    let status = tokio::task::spawn_blocking(move || permissions.status())
        .await
        .map_err(|error| format!("TUN 权限读取任务异常结束：{error}"))?
        .map_err(|error| error.to_string())?;
    if !status.granted {
        return Err(format!("TUN 已配置，但权限未就绪：{}", status.detail));
    }
    let config = config.clone();
    tokio::task::spawn_blocking(move || {
        Ok(TunCaptureStatus::from_platform(
            kind,
            &config,
            Some(&status),
        ))
    })
    .await
    .map_err(|error| format!("TUN 设备与路由读取任务异常结束：{error}"))?
}

fn observed_capture_plan(
    system_proxy: &Observation<SystemProxySessionSnapshot>,
    tun: &Observation<TunCaptureStatus>,
) -> ObservedCapturePlan {
    let (Some(system_proxy), Some(tun)) = (system_proxy.value(), tun.value()) else {
        return ObservedCapturePlan::Unknown;
    };
    match (system_proxy.actual.active(), tun.configured) {
        (false, false) => ObservedCapturePlan::Off,
        (true, false) => ObservedCapturePlan::SystemProxy,
        (false, true) => ObservedCapturePlan::TunConfigured,
        (true, true) => ObservedCapturePlan::Advanced,
    }
}

fn plan_matches(plan: CapturePlan, snapshot: &TrafficCaptureSnapshot) -> bool {
    match plan {
        CapturePlan::Off => {
            snapshot.observed_plan == ObservedCapturePlan::Off
                && snapshot.system_proxy.value().is_some_and(|system_proxy| {
                    !system_proxy.intent_enabled
                        && system_proxy.ownership == SystemProxyOwnershipState::Unowned
                })
        }
        CapturePlan::SystemProxy => {
            snapshot.observed_plan == ObservedCapturePlan::SystemProxy
                && snapshot.system_proxy.value().is_some_and(|system_proxy| {
                    system_proxy.intent_enabled
                        && system_proxy.ownership == SystemProxyOwnershipState::Owned
                })
        }
        CapturePlan::Tun => snapshot.observed_plan == ObservedCapturePlan::TunConfigured,
    }
}

fn has_external_system_proxy(snapshot: &TrafficCaptureSnapshot) -> bool {
    snapshot.system_proxy.value().is_some_and(|system_proxy| {
        system_proxy.actual.active() && system_proxy.ownership != SystemProxyOwnershipState::Owned
    })
}

fn system_proxy_is_owned_and_active(snapshot: &TrafficCaptureSnapshot) -> bool {
    snapshot.system_proxy.value().is_some_and(|system_proxy| {
        system_proxy.actual.active() && system_proxy.ownership == SystemProxyOwnershipState::Owned
    })
}

fn system_proxy_has_intent_or_ownership(snapshot: &TrafficCaptureSnapshot) -> bool {
    snapshot.system_proxy.value().is_some_and(|system_proxy| {
        system_proxy.intent_enabled || system_proxy.ownership == SystemProxyOwnershipState::Owned
    })
}

fn tun_is_configured(snapshot: &TrafficCaptureSnapshot) -> bool {
    snapshot.tun.value().is_some_and(|tun| tun.configured)
}

fn failed_snapshot(failure: String) -> TrafficCaptureSnapshot {
    let failure = crate::OperationalFailure {
        message: failure,
        occurred_at_ms: now_ms(),
    };
    TrafficCaptureSnapshot {
        system_proxy: Observation::Failed {
            failure: failure.clone(),
            recovery: RecoveryAction::ReviewCapture,
        },
        tun: Observation::Failed {
            failure,
            recovery: RecoveryAction::ReviewCapture,
        },
        observed_plan: ObservedCapturePlan::Unknown,
        system_proxy_port: None,
        core_available: false,
    }
}

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use crate::{SystemProxyOwnershipState, SystemProxyStatus};

    use super::*;

    #[derive(Clone)]
    struct FakeBackend {
        state: Arc<Mutex<FakeState>>,
    }

    struct FakeState {
        system_proxy: SystemProxySessionSnapshot,
        tun_configured: bool,
        core_available: bool,
        permission_error: Option<String>,
        permission_requests: usize,
        failures: VecDeque<&'static str>,
        operations: Vec<String>,
    }

    impl FakeBackend {
        fn new(system_active: bool, ownership: SystemProxyOwnershipState, tun: bool) -> Self {
            Self {
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
                    tun_configured: tun,
                    core_available: true,
                    permission_error: None,
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

        fn should_fail(state: &mut FakeState, operation: &str) -> Result<(), String> {
            if state
                .failures
                .front()
                .is_some_and(|next| *next == operation)
            {
                state.failures.pop_front();
                Err(format!("{operation} failed"))
            } else {
                Ok(())
            }
        }
    }

    impl CaptureBackend for FakeBackend {
        fn snapshot(&self) -> CaptureFuture<'_, CaptureBackendSnapshot> {
            Box::pin(async move {
                let state = self.state.lock().unwrap();
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
                    system_proxy_port: Some(7890),
                    core_available: state.core_available,
                })
            })
        }

        fn set_system_proxy(&self, enabled: bool, port: u16) -> CaptureFuture<'_, ()> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                let operation = format!("system-proxy:{enabled}");
                state.operations.push(operation.clone());
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
            })
        }

        fn set_tun(&self, enabled: bool) -> CaptureFuture<'_, ()> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                let operation = format!("tun:{enabled}");
                state.operations.push(operation.clone());
                Self::should_fail(&mut state, &operation)?;
                state.tun_configured = enabled;
                Ok(())
            })
        }

        fn ensure_tun_permission(&self) -> CaptureFuture<'_, ()> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                state.permission_requests += 1;
                state.permission_error.clone().map_or(Ok(()), Err)
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

    #[tokio::test]
    async fn reconcile_preserves_an_advanced_combination() {
        let backend = Arc::new(FakeBackend::new(
            true,
            SystemProxyOwnershipState::Owned,
            true,
        ));
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let outcome = session.reconcile().await.unwrap();

        assert_eq!(
            outcome.snapshot().observed_plan,
            ObservedCapturePlan::Advanced
        );
        assert_eq!(backend.operations(), ["reconcile"]);
    }

    #[tokio::test]
    async fn system_proxy_failure_restores_the_previous_tun_state() {
        let backend = Arc::new(FakeBackend::new(
            false,
            SystemProxyOwnershipState::Unowned,
            true,
        ));
        backend.fail_next("system-proxy:true");
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let outcome = session.apply(CapturePlan::SystemProxy).await.unwrap();

        assert!(matches!(outcome, CaptureOutcome::RolledBack { .. }));
        assert_eq!(
            backend.operations(),
            ["tun:false", "system-proxy:true", "tun:true"]
        );
        assert_eq!(
            outcome.snapshot().observed_plan,
            ObservedCapturePlan::TunConfigured
        );
    }

    #[tokio::test]
    async fn failed_rollback_exposes_reconcile_needed() {
        let backend = Arc::new(FakeBackend::new(
            false,
            SystemProxyOwnershipState::Unowned,
            true,
        ));
        backend.fail_next("system-proxy:true");
        backend.fail_next("tun:true");
        let session = TrafficCaptureSession::with_backend(backend);

        let outcome = session.apply(CapturePlan::SystemProxy).await.unwrap();

        assert!(matches!(
            outcome,
            CaptureOutcome::ReconcileNeeded {
                plan: Some(CapturePlan::SystemProxy),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn external_system_proxy_is_not_overwritten_by_off_or_tun() {
        let backend = Arc::new(FakeBackend::new(
            true,
            SystemProxyOwnershipState::Lost,
            false,
        ));
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let result = session.apply(CapturePlan::Off).await;

        assert!(matches!(
            result,
            Err(TrafficCaptureError::ExternalSystemProxy)
        ));
        assert!(backend.operations().is_empty());
    }

    #[tokio::test]
    async fn rejected_permission_performs_no_capture_write() {
        let backend = Arc::new(FakeBackend::new(
            false,
            SystemProxyOwnershipState::Unowned,
            false,
        ));
        backend.state.lock().unwrap().permission_error = Some("permission rejected".into());
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let outcome = session.apply(CapturePlan::Tun).await;

        assert!(matches!(outcome, Err(TrafficCaptureError::Backend(_))));
        assert_eq!(backend.permission_requests(), 1);
        assert!(backend.operations().is_empty());
    }

    #[tokio::test]
    async fn permission_prompt_is_reachable_only_from_an_explicit_tun_plan() {
        let backend = Arc::new(FakeBackend::new(
            false,
            SystemProxyOwnershipState::Unowned,
            false,
        ));
        let session = TrafficCaptureSession::with_backend(backend.clone());

        session.reconcile().await.unwrap();
        session.release_owned().await.unwrap();
        session.apply(CapturePlan::SystemProxy).await.unwrap();
        session.apply(CapturePlan::Off).await.unwrap();

        assert_eq!(backend.permission_requests(), 0);
        session.apply(CapturePlan::Tun).await.unwrap();
        assert_eq!(backend.permission_requests(), 1);
    }

    #[tokio::test]
    async fn normal_exit_releases_owned_system_proxy_without_changing_intent() {
        let backend = Arc::new(FakeBackend::new(
            true,
            SystemProxyOwnershipState::Owned,
            false,
        ));
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let outcome = session.release_owned().await.unwrap();

        let system_proxy = outcome.snapshot().system_proxy.value().unwrap();
        assert!(system_proxy.intent_enabled);
        assert!(!system_proxy.actual.active());
        assert_eq!(system_proxy.ownership, SystemProxyOwnershipState::Unowned);
        assert_eq!(backend.operations(), ["release-owned"]);
    }

    #[tokio::test]
    async fn exit_does_not_overwrite_an_external_system_proxy_replacement() {
        let backend = Arc::new(FakeBackend::new(
            true,
            SystemProxyOwnershipState::Lost,
            false,
        ));
        let session = TrafficCaptureSession::with_backend(backend.clone());

        let outcome = session.release_owned().await.unwrap();

        let system_proxy = outcome.snapshot().system_proxy.value().unwrap();
        assert!(system_proxy.actual.active());
        assert_eq!(system_proxy.ownership, SystemProxyOwnershipState::Unowned);
        assert_eq!(backend.operations(), ["release-owned"]);
    }

    #[tokio::test]
    async fn core_crash_releases_owned_proxy_and_restart_restores_persistent_intent() {
        let backend = Arc::new(FakeBackend::new(
            true,
            SystemProxyOwnershipState::Owned,
            false,
        ));
        let session = TrafficCaptureSession::with_backend(backend.clone());

        backend.set_core_available(false);
        let crashed = session.reconcile().await.unwrap();
        let crashed_proxy = crashed.snapshot().system_proxy.value().unwrap();
        assert!(crashed_proxy.intent_enabled);
        assert!(!crashed_proxy.actual.active());
        assert_eq!(crashed_proxy.ownership, SystemProxyOwnershipState::Unowned);

        backend.set_core_available(true);
        let restarted = session.reconcile().await.unwrap();
        let restarted_proxy = restarted.snapshot().system_proxy.value().unwrap();
        assert!(restarted_proxy.actual.active());
        assert_eq!(restarted_proxy.ownership, SystemProxyOwnershipState::Owned);
        assert_eq!(backend.operations(), ["reconcile", "reconcile"]);
    }
}
