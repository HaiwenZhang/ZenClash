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
        #[cfg(unix)]
        {
            trusted_installation_from_native_probes(registered, || {
                zenclash_service::execution::check_sidecar_available()
            })
            .await
        }
        #[cfg(not(unix))]
        {
            Ok(registered)
        }
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

// Registration is positive evidence. Without it, a successful residue check
// establishes absence, while an opaque inspection/occupancy error is inconclusive.
#[cfg(unix)]
async fn trusted_installation_from_native_probes<F, P>(registered: bool, inspect: F) -> Result<bool>
where
    F: FnOnce() -> P + Send,
    P: Future<Output = Result<()>> + Send,
{
    if registered {
        return Ok(true);
    }
    match inspect().await {
        Ok(()) => Ok(false),
        Err(error)
            if error
                .downcast_ref::<zenclash_service::execution::ResidualServiceError>()
                .is_some() =>
        {
            Ok(true)
        }
        Err(error) => Err(error).context("failed to inspect unregistered service residue"),
    }
}

#[cfg(test)]
pub use fake::FakeEnv;

#[cfg(test)]
mod fake;

#[cfg(all(test, unix))]
mod native_evidence_tests;
