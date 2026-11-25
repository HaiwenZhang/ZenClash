//! Named numeric revision checks; actual session and operation authority remain separate.

use super::{CoreSession, RuntimeConfigurationRevision};

impl CoreSession {
    pub(crate) fn runtime_revision(&self) -> RuntimeConfigurationRevision {
        RuntimeConfigurationRevision {
            binding_generation: self.runtime_descriptor().binding_generation(),
            configuration_generation: self.generation(),
        }
    }
}
