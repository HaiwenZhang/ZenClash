//! Internal ownership view of the controller binding's actual runtime.

use std::sync::Arc;

use crate::{ManagedCoreProcess, runtime::service::session::ServiceRuntimeSession};

#[derive(Clone)]
pub(crate) enum OwnedCore {
    Local(Arc<ManagedCoreProcess>),
    Service(Arc<ServiceRuntimeSession>),
}

impl crate::runtime::client::BindingSnapshot {
    pub(crate) fn owned_core(&self) -> Option<OwnedCore> {
        if let Some(lease) = self.local_lease() {
            return lease
                .into_any_arc()
                .downcast::<ManagedCoreProcess>()
                .ok()
                .map(OwnedCore::Local);
        }
        self.service_runtime().map(OwnedCore::Service)
    }
}

impl crate::MihomoClient {
    pub(crate) fn owned_core(&self) -> Option<OwnedCore> {
        self.binding_snapshot().owned_core()
    }
}

/// Verified permission evidence without pretending that a service copy is a local binary.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum CoreTunPermissionStatus {
    /// Native privilege evidence for the actual local executable.
    Local(crate::TunPermissionStatus),
    /// Administrator helper authority confirmed through authenticated native IPC.
    /// Device and route creation still require independent runtime observation.
    Service,
}

impl CoreTunPermissionStatus {
    /// Reports whether the current ownership boundary has verified TUN privileges.
    #[must_use]
    pub fn granted(&self) -> bool {
        match self {
            Self::Local(status) => status.granted,
            Self::Service => true,
        }
    }
    /// Reports whether this evidence supports the existing local authorization action.
    #[must_use]
    pub fn can_request(&self) -> bool {
        matches!(self, Self::Local(status) if status.can_request)
    }
}
