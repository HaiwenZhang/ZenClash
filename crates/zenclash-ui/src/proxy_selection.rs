//! Presentation progress for selection requests; actual facts remain in the proxy catalog.

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use zenclash_core::{AcceptedProxySelection, ProxyCatalog, ProxySelectionReadback};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProxySelectionRequest {
    pub(crate) group_key: String,
    pub(crate) requested_member: String,
    sequence: u64,
}

#[derive(Debug)]
enum SelectionPhase {
    Sending,
    AwaitingConfirmation,
    Unconfirmed(String),
}

#[derive(Debug)]
struct GroupSelectionProgress {
    request: ProxySelectionRequest,
    phase: SelectionPhase,
}

#[derive(Debug, Default)]
pub(crate) struct ProxySelectionUiState {
    next_sequence: u64,
    groups: HashMap<String, GroupSelectionProgress>,
}

impl ProxySelectionUiState {
    pub(crate) fn start(
        &mut self,
        group_key: String,
        requested_member: String,
    ) -> Option<ProxySelectionRequest> {
        if self.group_pending(&group_key) {
            return None;
        }
        self.next_sequence = self.next_sequence.checked_add(1)?;
        let request = ProxySelectionRequest {
            group_key,
            requested_member,
            sequence: self.next_sequence,
        };
        self.groups.insert(
            request.group_key.clone(),
            GroupSelectionProgress {
                request: request.clone(),
                phase: SelectionPhase::Sending,
            },
        );
        Some(request)
    }
    pub(crate) fn is_current(&self, request: &ProxySelectionRequest) -> bool {
        self.groups
            .get(&request.group_key)
            .is_some_and(|progress| progress.request.sequence == request.sequence)
    }
    pub(crate) fn acknowledge(&mut self, request: &ProxySelectionRequest) -> bool {
        if !self.is_current(request) {
            return false;
        }
        if let Some(progress) = self.groups.get_mut(&request.group_key) {
            progress.phase = SelectionPhase::AwaitingConfirmation;
        }
        true
    }
    pub(crate) fn reject(&mut self, request: &ProxySelectionRequest) -> bool {
        if !self.is_current(request) {
            return false;
        }
        self.groups.remove(&request.group_key);
        true
    }
    pub(crate) fn observation_failed(
        &mut self,
        request: &ProxySelectionRequest,
        error: String,
    ) -> bool {
        if !self.is_current(request) {
            return false;
        }
        if let Some(progress) = self.groups.get_mut(&request.group_key) {
            progress.phase = SelectionPhase::Unconfirmed(error);
        }
        true
    }
    pub(crate) fn observe(
        &mut self,
        request: &ProxySelectionRequest,
        accepted: &AcceptedProxySelection,
        readback: ProxySelectionReadback,
        catalog: Option<&mut ProxyCatalog>,
    ) -> bool {
        self.observe_with(request, accepted, readback, catalog, |_| {})
    }

    pub(crate) fn observe_with(
        &mut self,
        request: &ProxySelectionRequest,
        accepted: &AcceptedProxySelection,
        readback: ProxySelectionReadback,
        catalog: Option<&mut ProxyCatalog>,
        on_confirmed: impl FnOnce(&str),
    ) -> bool {
        if !self.is_current(request) {
            return false;
        }
        let failure = match readback {
            ProxySelectionReadback::Observed(observation) => {
                if catalog.is_some_and(|catalog| {
                    observation.apply_to_with(catalog, |member| {
                        self.groups.remove(&request.group_key);
                        on_confirmed(member);
                    })
                }) {
                    return true;
                }
                zenclash_i18n::text("proxies.selection.observation_outdated")
            }
            ProxySelectionReadback::GroupMissing => {
                zenclash_i18n::text("proxies.selection.group_missing")
            }
            ProxySelectionReadback::NoCurrentMember => {
                zenclash_i18n::text("proxies.selection.no_current_member")
            }
            ProxySelectionReadback::RuntimeChanged => {
                zenclash_i18n::text("proxies.selection.runtime_changed")
            }
            ProxySelectionReadback::Unavailable(error) => error.to_string(),
            _ => zenclash_i18n::text("proxies.selection.observation_outdated"),
        };
        if accepted
            .publish_if_current(|| self.observation_failed(request, failure))
            .is_err()
        {
            // This describes rejected eligibility now, rather than adopting an old controller error.
            self.observation_failed(
                request,
                zenclash_i18n::text("proxies.selection.runtime_changed"),
            );
        }
        true
    }
    pub(crate) fn clear(&mut self) {
        self.groups.clear();
    }
    pub(crate) fn any_pending(&self) -> bool {
        self.groups
            .values()
            .any(|progress| matches!(progress.phase, SelectionPhase::Sending))
    }
    pub(crate) fn group_pending(&self, group_key: &str) -> bool {
        self.groups
            .get(group_key)
            .is_some_and(|progress| matches!(progress.phase, SelectionPhase::Sending))
    }
    pub(crate) fn proxy_pending(&self, group_key: &str, member: &str) -> bool {
        self.groups.get(group_key).is_some_and(|progress| {
            matches!(progress.phase, SelectionPhase::Sending)
                && progress.request.requested_member == member
        })
    }
    pub(crate) fn status_text(&self, group_key: &str) -> Option<String> {
        self.groups
            .get(group_key)
            .map(|progress| match &progress.phase {
                SelectionPhase::Sending => zenclash_i18n::text("proxies.selection.sending"),
                SelectionPhase::AwaitingConfirmation => {
                    zenclash_i18n::text("proxies.selection.awaiting_confirmation")
                }
                SelectionPhase::Unconfirmed(error) => zenclash_i18n::text_with(
                    "proxies.selection.unconfirmed",
                    &[("error", error.clone())],
                ),
            })
    }
}
