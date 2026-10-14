//! Opaque native state and evidence of values actually attempted by an adapter.

use super::*;

#[derive(Clone)]
pub(in crate::system_proxy) enum NativeProxySnapshot {
    Platform(platform::ProxySnapshot),
    #[cfg(test)]
    Fixture {
        target_id: String,
        summary: SystemProxySummary,
        attributes: std::collections::BTreeMap<String, Vec<u8>>,
    },
}

impl std::fmt::Debug for NativeProxySnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Native attributes can contain proxy credentials. Never format them.
        formatter
            .debug_struct("NativeProxySnapshot")
            .field("service", &self.summary().service)
            .finish_non_exhaustive()
    }
}

impl NativeProxySnapshot {
    pub(in crate::system_proxy) fn summary(&self) -> &SystemProxySummary {
        match self {
            Self::Platform(snapshot) => snapshot.summary(),
            #[cfg(test)]
            Self::Fixture { summary, .. } => summary,
        }
    }

    pub(in crate::system_proxy) fn platform(&self) -> MihomoResult<&platform::ProxySnapshot> {
        match self {
            Self::Platform(snapshot) => Ok(snapshot),
            #[cfg(test)]
            _ => Err(ownership_changed()),
        }
    }

    pub(in crate::system_proxy) fn same_target(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Platform(left), Self::Platform(right)) => left.same_target(right),
            #[cfg(test)]
            (
                Self::Fixture {
                    target_id: left, ..
                },
                Self::Fixture {
                    target_id: right, ..
                },
            ) => left == right,
            #[cfg(test)]
            _ => false,
        }
    }

    pub(in crate::system_proxy) fn legacy_release_is_safe(&self) -> bool {
        match self {
            Self::Platform(snapshot) => snapshot.legacy_release_is_safe(),
            #[cfg(test)]
            Self::Fixture { .. } => true,
        }
    }

    pub(in crate::system_proxy) fn same_state(&self, other: &Self) -> MihomoResult<bool> {
        match (self, other) {
            (Self::Platform(left), Self::Platform(right)) => left.same_state(right),
            #[cfg(test)]
            (
                Self::Fixture {
                    target_id: left_target,
                    summary: left,
                    attributes: left_attributes,
                },
                Self::Fixture {
                    target_id: right_target,
                    summary: right,
                    attributes: right_attributes,
                },
            ) => {
                let mut left = left.clone();
                let mut right = right.clone();
                left.service.clear();
                right.service.clear();
                Ok(left_target == right_target
                    && left == right
                    && left_attributes == right_attributes)
            }
            #[cfg(test)]
            _ => Ok(false),
        }
    }
}

#[derive(Clone, Copy)]
pub(in crate::system_proxy) enum NativeProxyTarget<'a> {
    Service(&'a str),
    Captured(&'a NativeProxySnapshot),
}

pub(in crate::system_proxy) enum NativeProxyChange<'a> {
    Manual {
        host: &'a str,
        port: u16,
        bypass: &'a [String],
    },
    Pac {
        url: &'a str,
    },
    Disable,
}

#[derive(Clone, Debug)]
pub(in crate::system_proxy) struct NativeMutationWitness {
    known_states: Vec<NativeProxySnapshot>,
    attempted: bool,
}

impl NativeMutationWitness {
    pub(in crate::system_proxy) fn new(before: &NativeProxySnapshot) -> Self {
        Self {
            known_states: vec![before.clone()],
            attempted: false,
        }
    }

    /// Record the exact state a native step may write, before attempting it.
    pub(in crate::system_proxy) fn record(&mut self, attempted: NativeProxySnapshot) {
        self.attempted = true;
        self.known_states.push(attempted);
    }

    pub(in crate::system_proxy) fn has_steps(&self) -> bool {
        self.attempted
    }

    pub(in crate::system_proxy) fn append(&mut self, later: Self) {
        self.attempted |= later.attempted;
        self.known_states.extend(later.known_states);
    }

    pub(in crate::system_proxy) fn permits(
        &self,
        actual: &NativeProxySnapshot,
    ) -> MihomoResult<bool> {
        for known in &self.known_states {
            if known.same_state(actual)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

pub(in crate::system_proxy) struct NativeWriteAttempt {
    pub(in crate::system_proxy) witness: NativeMutationWitness,
    pub(in crate::system_proxy) result: MihomoResult<NativeProxySnapshot>,
}

impl NativeWriteAttempt {
    pub(in crate::system_proxy) fn run(
        before: &NativeProxySnapshot,
        operation: impl FnOnce(&mut NativeMutationWitness) -> MihomoResult<NativeProxySnapshot>,
    ) -> Self {
        let mut witness = NativeMutationWitness::new(before);
        let result = operation(&mut witness);
        Self { witness, result }
    }
}

pub(in crate::system_proxy) fn pending_recovery() -> MihomoError {
    SystemProxyError::RecoveryPending.into()
}

pub(in crate::system_proxy) fn verification_failure() -> MihomoError {
    SystemProxyError::VerificationFailed.into()
}

pub(in crate::system_proxy) fn ownership_changed() -> MihomoError {
    SystemProxyError::OwnershipChanged.into()
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux", test))]
pub(in crate::system_proxy) fn invalid_snapshot() -> MihomoError {
    SystemProxyError::InvalidNativeSnapshot { cause: None }.into()
}

#[cfg(any(target_os = "windows", target_os = "linux", test))]
pub(in crate::system_proxy) fn unstable_observation() -> MihomoError {
    SystemProxyError::ObservationUnstable.into()
}
