//! Owns a diagnostic query's cancellation, submitted inputs and guarded publication.

use std::sync::Arc;

use tokio::{runtime::Handle, task::JoinHandle};
use zenclash_core::{
    CoreObservationIdentity, CoreSession, DiagnosticData, DiagnosticPlan, DiagnosticReport,
    DiagnosticStepKind, NetworkDiagnostics, NetworkProbeRoute, NetworkProbeRoutePreference,
    NetworkProbeSnapshot, OperationalStatus,
};

use super::model;
use crate::pages::runtime::page_observation::PageReadTask;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::pages::runtime) struct NetworkDiagnosticRequestId(u64);

struct ActiveNetworkDiagnostic {
    id: NetworkDiagnosticRequestId,
    selected_kind: DiagnosticStepKind,
}

/// Completed network reads retain a weak identity, never a retired runtime owner.
pub(in crate::pages::runtime) struct ObservedNetworkDiagnostic {
    identity: CoreObservationIdentity,
    plan: DiagnosticPlan,
    mihomo_route: Result<NetworkProbeRoute, String>,
    report: DiagnosticReport,
}

#[derive(Debug)]
struct NetworkDiagnosticPresentation {
    dns_name: Option<String>,
    report: Option<DiagnosticReport>,
    snapshot: NetworkProbeSnapshot,
}

impl NetworkDiagnosticPresentation {
    fn failed(error: String) -> Self {
        Self {
            dns_name: None,
            report: None,
            snapshot: NetworkProbeSnapshot {
                public_ip_error: Some(error),
                ..Default::default()
            },
        }
    }

    fn observed(observed: ObservedNetworkDiagnostic, selected_kind: DiagnosticStepKind) -> Self {
        Self::from_report(
            observed.plan.dns_name().to_owned(),
            observed.report,
            selected_kind,
        )
    }

    fn from_report(
        dns_name: String,
        report: DiagnosticReport,
        selected_kind: DiagnosticStepKind,
    ) -> Self {
        let snapshot = diagnostic_network_snapshot(&report, selected_kind)
            .cloned()
            .unwrap_or_else(|error| NetworkProbeSnapshot {
                public_ip_error: Some(error),
                ..Default::default()
            });
        Self {
            dns_name: Some(dns_name),
            report: Some(report),
            snapshot,
        }
    }
}

#[derive(Default)]
pub(in crate::pages::runtime) struct NetworkDiagnosticQueryState {
    next_request_sequence: u64,
    active: Option<ActiveNetworkDiagnostic>,
    task: PageReadTask,
    presentation: Option<NetworkDiagnosticPresentation>,
}

impl NetworkDiagnosticQueryState {
    #[cfg(test)]
    pub(in crate::pages::runtime) fn begin_pending_for_test<T>(
        &mut self,
        task: &JoinHandle<T>,
    ) -> NetworkDiagnosticRequestId {
        let request = self.begin(NetworkProbeRoutePreference::Direct);
        self.task.replace(task);
        request
    }

    #[cfg(all(test, target_os = "windows"))]
    pub(in crate::pages::runtime) fn set_design_validation(
        &mut self,
        dns_name: String,
        report: DiagnosticReport,
    ) {
        self.cancel();
        self.presentation = Some(NetworkDiagnosticPresentation::from_report(
            dns_name,
            report,
            DiagnosticStepKind::NetworkMihomo,
        ));
    }

    #[cfg(all(test, target_os = "windows"))]
    pub(in crate::pages::runtime) fn report_for_design_validation(
        &mut self,
    ) -> Option<&mut DiagnosticReport> {
        self.presentation.as_mut()?.report.as_mut()
    }

    pub(in crate::pages::runtime) fn is_loading(&self) -> bool {
        self.active.is_some()
    }

    pub(in crate::pages::runtime) fn report(&self) -> Option<&DiagnosticReport> {
        self.presentation.as_ref()?.report.as_ref()
    }

    pub(in crate::pages::runtime) fn snapshot(&self) -> Option<&NetworkProbeSnapshot> {
        self.presentation.as_ref().map(|result| &result.snapshot)
    }

    pub(in crate::pages::runtime) fn dns_name(&self) -> Option<&str> {
        self.presentation.as_ref()?.dns_name.as_deref()
    }

    pub(in crate::pages::runtime) fn cancel(&mut self) {
        self.task.cancel();
        self.active = None;
    }

    pub(super) fn reject_plan(&mut self, error: String) {
        self.cancel();
        self.presentation = Some(NetworkDiagnosticPresentation::failed(error));
    }

    pub(super) fn start(
        &mut self,
        runtime: &Handle,
        session: CoreSession,
        status: Arc<OperationalStatus>,
        plan: DiagnosticPlan,
        selection: NetworkProbeRoutePreference,
    ) -> (
        NetworkDiagnosticRequestId,
        JoinHandle<Result<ObservedNetworkDiagnostic, String>>,
    ) {
        let request = self.begin(selection);
        let task = runtime.spawn(observe_network_diagnostic(session, status, plan));
        self.task.replace(&task);
        (request, task)
    }

    fn begin(&mut self, selection: NetworkProbeRoutePreference) -> NetworkDiagnosticRequestId {
        self.cancel();
        self.next_request_sequence = self.next_request_sequence.wrapping_add(1);
        let id = NetworkDiagnosticRequestId(self.next_request_sequence);
        self.active = Some(ActiveNetworkDiagnostic {
            id,
            selected_kind: match selection {
                NetworkProbeRoutePreference::Direct => DiagnosticStepKind::NetworkDirect,
                NetworkProbeRoutePreference::Mihomo => DiagnosticStepKind::NetworkMihomo,
            },
        });
        id
    }

    /// Publication only touches this presentation and operational facts, under the
    /// original observation guard. It never reacquires session/client/repository locks.
    pub(in crate::pages::runtime) fn complete(
        &mut self,
        request: NetworkDiagnosticRequestId,
        session: &CoreSession,
        status: &OperationalStatus,
        result: Result<ObservedNetworkDiagnostic, String>,
    ) {
        let Some(active) = self.active.as_ref().filter(|active| active.id == request) else {
            return;
        };
        let selected_kind = active.selected_kind;
        self.active = None;
        self.task.cancel();
        let observed = match result {
            Ok(observed) => observed,
            Err(error) => {
                self.presentation = Some(NetworkDiagnosticPresentation::failed(error));
                return;
            }
        };
        if !observed.identity.belongs_to(session) {
            return;
        }
        let identity = observed.identity.clone();
        let generation = identity.configuration_generation();
        let path = observed
            .mihomo_route
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|route| {
                let snapshot = diagnostic_network_snapshot(
                    &observed.report,
                    DiagnosticStepKind::NetworkMihomo,
                )?;
                model::path_observation(route, generation, snapshot)
            });
        let presentation = NetworkDiagnosticPresentation::observed(observed, selected_kind);
        identity.publish_if_current(|| {
            status.record_path(generation, path);
            self.presentation = Some(presentation);
        });
    }
}

async fn observe_network_diagnostic(
    session: CoreSession,
    status: Arc<OperationalStatus>,
    plan: DiagnosticPlan,
) -> Result<ObservedNetworkDiagnostic, String> {
    let observation = session
        .begin_observation()
        .await
        .map_err(|error| error.to_string())?;
    let mihomo_route = observation
        .client()
        .runtime_config()
        .await
        .map_err(|error| error.to_string())
        .and_then(|config| model::network_probe_route(&config, true));
    let run_plan = match &mihomo_route {
        Ok(route) => plan
            .clone()
            .with_mihomo_route(route.clone())
            .map_err(|error| error.to_string())?,
        Err(_) => plan.clone(),
    };
    let report = NetworkDiagnostics::new(observation.client().clone(), status)
        .run(run_plan)
        .await;
    Ok(ObservedNetworkDiagnostic {
        identity: observation.identity().clone(),
        plan,
        mihomo_route,
        report,
    })
}

fn diagnostic_network_snapshot(
    report: &DiagnosticReport,
    kind: DiagnosticStepKind,
) -> Result<&NetworkProbeSnapshot, String> {
    let step = report
        .step(kind)
        .ok_or_else(|| zenclash_i18n::text("network.errors.missing_diagnostic_step"))?;
    match &step.outcome {
        Ok(DiagnosticData::Network(snapshot)) => Ok(snapshot),
        Ok(_) => Err(zenclash_i18n::text(
            "network.errors.invalid_diagnostic_step",
        )),
        Err(error) => Err(error.message.clone()),
    }
}

#[cfg(test)]
mod tests;
