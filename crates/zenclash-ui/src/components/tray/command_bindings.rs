//! The command map and original observation installed together in one native menu.

use super::{TrayCommand, TrayEvent};
use std::collections::HashMap;
use zenclash_core::CoreObservationIdentity;

pub(super) struct TrayCommandBindings {
    commands: HashMap<String, TrayCommand>,
    observation: Option<CoreObservationIdentity>,
}

impl TrayCommandBindings {
    pub(super) fn new(
        commands: HashMap<String, TrayCommand>,
        observation: Option<CoreObservationIdentity>,
    ) -> Self {
        Self {
            commands,
            observation,
        }
    }

    pub(super) fn resolve(&self, id: &str) -> Option<TrayEvent> {
        self.commands
            .get(id)
            .cloned()
            .map(|command| TrayEvent::Command(command, self.observation.clone()))
    }
}

#[cfg(test)]
mod tests;
