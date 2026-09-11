/// Operations that must serialize within one runtime page owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MutationDomain {
    Core,
    Logs,
    Language,
    TrafficHistory,
    Network,
    Backup,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MutationToken {
    domain: MutationDomain,
    generation: u64,
}

#[derive(Default)]
pub(super) struct MutationState {
    generation: u64,
    active: Vec<MutationToken>,
}

impl MutationState {
    pub(super) fn busy(&self, domain: MutationDomain) -> bool {
        self.active.iter().any(|token| {
            token.domain == domain
                || token.domain == MutationDomain::Backup
                || domain == MutationDomain::Backup
        })
    }

    pub(super) fn begin(&mut self, domain: MutationDomain) -> Option<MutationToken> {
        if self.busy(domain) {
            return None;
        }
        self.generation = self.generation.checked_add(1)?;
        let token = MutationToken {
            domain,
            generation: self.generation,
        };
        self.active.push(token);
        Some(token)
    }

    pub(super) fn finish(&mut self, token: MutationToken) {
        self.active.retain(|active| *active != token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_preferences_can_finish_while_core_remains_busy() {
        let mut state = MutationState::default();
        let core = state.begin(MutationDomain::Core).unwrap();
        for domain in [
            MutationDomain::Logs,
            MutationDomain::Language,
            MutationDomain::TrafficHistory,
            MutationDomain::Network,
        ] {
            let token = state.begin(domain).unwrap();
            assert!(state.begin(domain).is_none());
            state.finish(token);
            assert!(!state.busy(domain));
            assert!(state.busy(MutationDomain::Core));
        }
        assert!(state.begin(MutationDomain::Core).is_none());
        state.finish(core);
        assert!(state.begin(MutationDomain::Core).is_some());
    }

    #[test]
    fn backup_conflicts_with_every_mutation_in_both_directions() {
        for domain in [
            MutationDomain::Core,
            MutationDomain::Logs,
            MutationDomain::Language,
            MutationDomain::TrafficHistory,
            MutationDomain::Network,
            MutationDomain::Backup,
        ] {
            let mut state = MutationState::default();
            let pending = state.begin(domain).unwrap();
            assert!(state.begin(MutationDomain::Backup).is_none());
            state.finish(pending);
            let backup = state.begin(MutationDomain::Backup).unwrap();
            assert!(state.begin(domain).is_none());
            state.finish(backup);
            assert!(state.begin(domain).is_some());
        }
    }

    #[test]
    fn duplicate_completion_cannot_release_a_new_operation() {
        let mut state = MutationState::default();
        let old = state.begin(MutationDomain::Logs).unwrap();
        state.finish(old);
        let new = state.begin(MutationDomain::Logs).unwrap();
        state.finish(old);
        assert!(state.busy(MutationDomain::Logs));
        state.finish(new);
        assert!(!state.busy(MutationDomain::Logs));
    }
}
