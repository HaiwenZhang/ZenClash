//! Internal ownership view of the controller binding's actual runtime.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{MihomoProcess, service_runtime_session::ServiceRuntimeSession};

#[derive(Clone)]
pub(crate) enum OwnedCore {
    Local(Arc<MihomoProcess>),
    Service(Arc<ServiceRuntimeSession>),
}

/// Controller ownership reported by the current in-memory runtime binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CoreRuntimeBackend {
    /// A controller whose process is outside ZenClash's ownership.
    Direct,
    /// A real child process owned by this application.
    Local,
    /// A kernel owned by the authenticated administrator service.
    Service,
}

/// Prepared runtime identity and immutable local launch paths.
///
/// Reading this descriptor performs no filesystem, process-status or IPC work.
/// Service paths are absent because protected helper copies are not user sources.
#[derive(Clone, Debug)]
pub struct CoreRuntimeDescriptor {
    pub(crate) binding_generation: u64,
    pub(crate) backend: CoreRuntimeBackend,
    pub(crate) kind: crate::CoreKind,
    pub(crate) binary: Option<PathBuf>,
    pub(crate) config_file: Option<PathBuf>,
    pub(crate) home_dir: Option<PathBuf>,
}

impl CoreRuntimeDescriptor {
    /// Returns the transport generation pairing this identity with its launch metadata.
    #[must_use]
    pub const fn binding_generation(&self) -> u64 {
        self.binding_generation
    }
    /// Returns the process ownership boundary.
    #[must_use]
    pub const fn backend(&self) -> CoreRuntimeBackend {
        self.backend
    }
    /// Returns the actual runtime implementation.
    #[must_use]
    pub const fn kind(&self) -> crate::CoreKind {
        self.kind
    }
    /// Returns the actual local binary, absent for service and external controllers.
    #[must_use]
    pub fn binary(&self) -> Option<&Path> {
        self.binary.as_deref()
    }
    /// Returns the actual local launch configuration path.
    #[must_use]
    pub fn config_file(&self) -> Option<&Path> {
        self.config_file.as_deref()
    }
    /// Returns the actual local process home.
    #[must_use]
    pub fn home_dir(&self) -> Option<&Path> {
        self.home_dir.as_deref()
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
