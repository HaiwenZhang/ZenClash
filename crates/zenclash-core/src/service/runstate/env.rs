// Forked from Clash Verge Rev; adapted for ZenClash on 2026-10-04.
// GPL-3.0-only; see NOTICE.md and UPSTREAM.json.
//! Adapters between Run State and the real or scripted machine it observes.

use anyhow::{Context as _, Result};

use super::health::{PendingAction, RunState};
use super::probe::ServiceVersionReply;

/// Everything Run State needs from outside itself.
pub trait RunStateEnv: Send + Sync + 'static {
    /// Probes the native helper protocol and selected approved core.
    fn probe_service_version(&self) -> impl Future<Output = Result<ServiceVersionReply>> + Send;

    /// Returns an error when installation evidence cannot be inspected, not when it is absent.
    fn trusted_install_evidence(&self) -> impl Future<Output = Result<bool>> + Send;

    /// Whether the desktop process has native administrator privileges.
    fn is_elevated(&self) -> bool;

    /// Keeps the PAC endpoint derived from Running Mode.
    fn set_pac_available(&self, available: bool);

    /// Publishes one coherent session-state observation.
    fn publish(&self, state: &RunState);

    ///
    /// # Errors
    /// Returns missing or invalid sources, native authorization or installer-process errors; partial maintenance may have occurred.
    fn run_privileged(&self, action: PendingAction) -> Result<()>;
}

/// Application effects owned by ZenClash's core/PAC/UI layer, rather than Tauri globals.
pub trait RunStateHost: Send + Sync + 'static {
    /// Sets availability of the application-owned PAC endpoint.
    fn set_pac_available(&self, available: bool);
    /// Publishes one coherent session-state observation.
    fn publish(&self, state: &RunState);
    ///
    /// # Errors
    /// Returns missing or invalid sources, native authorization or installer-process errors; partial maintenance may have occurred.
    fn run_privileged(&self, action: PendingAction) -> Result<()>;
}

/// Native service observation with an explicit selected core and application effects.
#[derive(Debug)]
pub struct NativeEnv<H> {
    core: zenclash_service::CoreRequirement,
    host: H,
}

impl<H: RunStateHost> NativeEnv<H> {
    /// Pairs the selected approved core with application-owned effects.
    pub const fn new(core: zenclash_service::CoreRequirement, host: H) -> Self {
        Self { core, host }
    }
}

impl<H: RunStateHost> RunStateEnv for NativeEnv<H> {
    async fn probe_service_version(&self) -> Result<ServiceVersionReply> {
        // Copied from RealEnv. A known stopped SCM job cannot answer IPC retries.
        #[cfg(all(windows, not(feature = "service-ipc-tests")))]
        if tokio::task::spawn_blocking(crate::service::platform::service_stopped)
            .await
            .context("service status probe did not finish")??
        {
            anyhow::bail!("the Windows service is not running");
        }
        let response = zenclash_service::get_version().await?;
        let core = if response.code == 0
            && response.data.as_ref().is_some_and(|info| {
                info.supports_client(
                    zenclash_service::ProtocolVersion::current(),
                    zenclash_service::MIN_REQUIRED_SERVICE_REVISION,
                )
            }) {
            let status =
                zenclash_service::inspect_installation(std::slice::from_ref(&self.core)).await?;
            status
                .cores
                .into_iter()
                .find(|core| core.name == self.core.name)
                .map(|core| core.availability)
        } else {
            None
        };
        Ok(ServiceVersionReply {
            core,
            code: response.code,
            message: response.message,
            protocol: response.data,
        })
    }

    async fn trusted_install_evidence(&self) -> Result<bool> {
        let registered =
            tokio::task::spawn_blocking(crate::service::platform::trusted_service_evidence)
                .await
                .context("service registration probe did not finish")??;
        // A helper that outlived its registration is broken, not absent.
        #[cfg(unix)]
        if !registered
            && let Err(error) = zenclash_service::execution::check_sidecar_available().await
        {
            return Ok(error
                .downcast_ref::<zenclash_service::execution::ResidualServiceError>()
                .is_some());
        }
        Ok(registered)
    }

    fn is_elevated(&self) -> bool {
        crate::service::current_process_elevated()
    }

    fn set_pac_available(&self, available: bool) {
        self.host.set_pac_available(available);
    }

    fn publish(&self, state: &RunState) {
        self.host.publish(state);
    }

    fn run_privileged(&self, action: PendingAction) -> Result<()> {
        self.host.run_privileged(action)
    }
}

#[cfg(test)]
pub use fake::FakeEnv;

#[cfg(test)]
mod fake {
    use anyhow::{Result, anyhow};
    use parking_lot::Mutex;
    use zenclash_service::ProtocolInfo;

    use super::{PendingAction, RunState, RunStateEnv, ServiceVersionReply};
    use crate::service::RunningMode;

    /// A fail-closed scripted machine that records outbound effects.
    #[derive(Debug)]
    pub struct FakeEnv {
        version_replies: Mutex<Vec<Result<ServiceVersionReply, String>>>,
        evidence: Result<bool, String>,
        elevated: bool,
        probe_count: Mutex<usize>,
        pac_available: Mutex<Option<bool>>,
        published: Mutex<Vec<RunState>>,
        privileged_outcome: Mutex<Result<(), String>>,
        privileged_actions: Mutex<Vec<PendingAction>>,
    }

    impl Default for FakeEnv {
        fn default() -> Self {
            Self {
                version_replies: Mutex::new(Vec::new()),
                evidence: Ok(false),
                elevated: false,
                probe_count: Mutex::new(0),
                pac_available: Mutex::new(None),
                published: Mutex::new(Vec::new()),
                privileged_outcome: Mutex::new(Ok(())),
                privileged_actions: Mutex::new(Vec::new()),
            }
        }
    }

    impl FakeEnv {
        #[must_use]
        /// Creates a scripted environment with no trusted installation.
        pub fn new() -> Self {
            Self::default()
        }

        #[must_use]
        /// Configures a compatible service and approved core response.
        pub fn service_ready(self) -> Self {
            self.with_evidence(true)
                .always_replying(Ok(ServiceVersionReply {
                    core: Some(zenclash_service::CoreAvailability::Ready),
                    code: 0,
                    message: "ok".to_owned(),
                    protocol: Some(ProtocolInfo::current()),
                }))
        }

        #[must_use]
        /// Configures trusted installation evidence with an incompatible protocol.
        pub fn service_version_mismatch(self) -> Self {
            self.with_evidence(true)
                .always_replying(Ok(ServiceVersionReply {
                    core: Some(zenclash_service::CoreAvailability::Ready),
                    code: 0,
                    message: "ok".to_owned(),
                    protocol: None,
                }))
        }

        #[must_use]
        /// Configures trusted installation evidence with unreachable IPC.
        pub fn service_unreachable(self) -> Self {
            self.with_evidence(true)
                .always_replying(Err("ipc transport refused".to_owned()))
        }

        #[must_use]
        /// Makes trusted-installation inspection fail.
        pub fn evidence_unavailable(mut self) -> Self {
            self.evidence = Err("registry probe failed".to_owned());
            self
        }

        #[must_use]
        /// Reports the scripted desktop process as elevated.
        pub const fn elevated(mut self) -> Self {
            self.elevated = true;
            self
        }

        #[must_use]
        /// Sets whether a trusted installation marker exists.
        pub fn with_evidence(mut self, exists: bool) -> Self {
            self.evidence = Ok(exists);
            self
        }

        /// Queue replies consumed one per probe; the last one repeats once exhausted.
        #[must_use]
        pub fn replying(self, replies: Vec<Result<ServiceVersionReply, String>>) -> Self {
            *self.version_replies.lock() = replies;
            self
        }

        #[must_use]
        /// Repeats the same version-probe reply on every observation.
        pub fn always_replying(self, reply: Result<ServiceVersionReply, String>) -> Self {
            self.replying(vec![reply])
        }

        #[must_use]
        /// Returns the number of service-version probes performed.
        pub fn probe_count(&self) -> usize {
            *self.probe_count.lock()
        }

        #[must_use]
        /// Returns the last published PAC availability.
        pub fn pac_available(&self) -> Option<bool> {
            *self.pac_available.lock()
        }

        #[must_use]
        /// Returns the backend from each published state in order.
        pub fn published_modes(&self) -> Vec<RunningMode> {
            self.published
                .lock()
                .iter()
                .map(|state| state.mode)
                .collect()
        }

        #[must_use]
        /// Returns all published session-state snapshots.
        pub fn published(&self) -> Vec<RunState> {
            self.published.lock().clone()
        }

        #[must_use]
        /// Makes privileged maintenance return the supplied failure.
        pub fn privileged_operations_fail(self, reason: &str) -> Self {
            *self.privileged_outcome.lock() = Err(reason.to_owned());
            self
        }

        #[must_use]
        /// Returns the admitted privileged maintenance actions in order.
        pub fn privileged_actions(&self) -> Vec<PendingAction> {
            self.privileged_actions.lock().clone()
        }
    }

    impl RunStateEnv for FakeEnv {
        async fn probe_service_version(&self) -> Result<ServiceVersionReply> {
            *self.probe_count.lock() += 1;
            let mut replies = self.version_replies.lock();
            let reply = if replies.len() > 1 {
                replies.remove(0)
            } else {
                replies
                    .first()
                    .cloned()
                    .unwrap_or_else(|| Err("no service configured".to_owned()))
            };
            reply.map_err(|error| anyhow!(error))
        }

        async fn trusted_install_evidence(&self) -> Result<bool> {
            self.evidence.clone().map_err(|error| anyhow!(error))
        }

        fn is_elevated(&self) -> bool {
            self.elevated
        }

        fn set_pac_available(&self, available: bool) {
            *self.pac_available.lock() = Some(available);
        }

        fn publish(&self, state: &RunState) {
            self.published.lock().push(state.clone());
        }

        fn run_privileged(&self, action: PendingAction) -> Result<()> {
            self.privileged_actions.lock().push(action);
            self.privileged_outcome
                .lock()
                .clone()
                .map_err(|error| anyhow!(error))
        }
    }
}
