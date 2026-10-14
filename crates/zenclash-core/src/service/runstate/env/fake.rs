//! Scripted RunStateEnv and recorded effects shared by state-machine tests.

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
    #[cfg(unix)]
    native_residue_result: Option<Result<(), String>>,
    #[cfg(unix)]
    residue_probe_count: Mutex<usize>,
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
            #[cfg(unix)]
            native_residue_result: None,
            #[cfg(unix)]
            residue_probe_count: Mutex::new(0),
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

    #[cfg(unix)]
    /// Uses the production native-evidence protocol with scripted registration/residue replies.
    #[must_use]
    pub fn with_native_evidence_probe(
        mut self,
        registered: Result<bool, String>,
        residue: Result<(), String>,
    ) -> Self {
        self.evidence = registered;
        self.native_residue_result = Some(residue);
        self
    }

    #[cfg(unix)]
    /// Returns the number of residue inspections actually requested.
    #[must_use]
    pub fn residue_probe_count(&self) -> usize {
        *self.residue_probe_count.lock()
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
        let registered = self.evidence.clone().map_err(|error| anyhow!(error))?;
        #[cfg(unix)]
        if let Some(residue) = &self.native_residue_result {
            return super::trusted_installation_from_native_probes(registered, || async {
                *self.residue_probe_count.lock() += 1;
                residue.clone().map_err(|error| anyhow!(error))
            })
            .await;
        }
        Ok(registered)
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
