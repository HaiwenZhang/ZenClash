//! Named numeric revision checks; actual session and operation authority remain separate.

use super::CoreSession;

/// Correlates a runtime binding with its accepted configuration generation.
/// These counters do not identify the owning session or authorize a mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeConfigurationRevision {
    pub(crate) binding_generation: u64,
    pub(crate) configuration_generation: u64,
}

impl CoreSession {
    pub(crate) fn runtime_revision(&self) -> RuntimeConfigurationRevision {
        RuntimeConfigurationRevision {
            binding_generation: self.runtime_descriptor().binding_generation(),
            configuration_generation: self.generation(),
        }
    }
}
