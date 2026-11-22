//! Pure observation and capability value types shared across runtime layers.
//!
//! Kept free of `core_session` and `operational_status` monitor dependencies so
//! both layers can share trustworthy observation envelopes without cycles.

/// Recovery action associated with an observation that has no trustworthy value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Retry the read without mutating runtime state.
    Retry,
    /// Inspect or restart the managed runtime core.
    InspectCore,
    /// Review native capture settings and ownership.
    ReviewCapture,
    /// No action is available because the selected core does not support the fact.
    Unsupported,
}

/// Failure attached to a stale or failed observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationalFailure {
    /// Human-readable failure suitable for diagnostics and localized UI wrapping.
    pub message: String,
    /// Unix timestamp in milliseconds when the failure was observed.
    pub occurred_at_ms: u64,
}

/// Trust state shared by every independently refreshed operational slice.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Observation<T> {
    /// No successful value has been observed yet.
    #[default]
    Loading,
    /// A successful current value and its observation time.
    Fresh {
        /// Most recent successful value.
        value: T,
        /// Unix timestamp in milliseconds when the value was observed.
        observed_at_ms: u64,
    },
    /// The last successful value retained after a newer read failed.
    Stale {
        /// Last successful value.
        value: T,
        /// Unix timestamp in milliseconds of the last success.
        observed_at_ms: u64,
        /// Newer failure that made the value stale.
        failure: OperationalFailure,
    },
    /// A failed read for which no trustworthy value exists.
    Failed {
        /// Failure reported by the observer.
        failure: OperationalFailure,
        /// Safe next action offered to the caller.
        recovery: RecoveryAction,
    },
}

impl<T> Observation<T> {
    /// Returns the trustworthy current or last-successful value, if one exists.
    #[must_use]
    pub const fn value(&self) -> Option<&T> {
        match self {
            Self::Fresh { value, .. } | Self::Stale { value, .. } => Some(value),
            Self::Loading | Self::Failed { .. } => None,
        }
    }

    /// Returns current successful evidence; stale data remains available through `value`.
    #[must_use]
    pub const fn fresh_value(&self) -> Option<&T> {
        match self {
            Self::Fresh { value, .. } => Some(value),
            _ => None,
        }
    }

    /// Returns the timestamp of the current or last-successful value.
    #[must_use]
    pub const fn observed_at_ms(&self) -> Option<u64> {
        match self {
            Self::Fresh { observed_at_ms, .. } | Self::Stale { observed_at_ms, .. } => {
                Some(*observed_at_ms)
            }
            Self::Loading | Self::Failed { .. } => None,
        }
    }

    /// Returns whether this observation contains a current successful value.
    #[must_use]
    pub const fn is_fresh(&self) -> bool {
        matches!(self, Self::Fresh { .. })
    }
}

impl<T: Clone> Observation<T> {
    /// Records an independent read while preserving a last-successful value on failure.
    #[must_use]
    pub fn record<E: ToString>(
        previous: &Self,
        result: Result<T, E>,
        observed_at_ms: u64,
        recovery: RecoveryAction,
    ) -> Self {
        match result {
            Ok(value) => Self::Fresh {
                value,
                observed_at_ms,
            },
            Err(error) => {
                let failure = OperationalFailure {
                    message: error.to_string(),
                    occurred_at_ms: observed_at_ms,
                };
                match previous {
                    Self::Fresh {
                        value,
                        observed_at_ms,
                    }
                    | Self::Stale {
                        value,
                        observed_at_ms,
                        ..
                    } => Self::Stale {
                        value: value.clone(),
                        observed_at_ms: *observed_at_ms,
                        failure,
                    },
                    Self::Loading | Self::Failed { .. } => Self::Failed { failure, recovery },
                }
            }
        }
    }

    /// Keeps an older trustworthy value when an independently loaded replacement failed.
    #[must_use]
    pub fn retain_last_success(previous: &Self, next: Self) -> Self {
        let Self::Failed { failure, recovery } = next else {
            return next;
        };
        match previous {
            Self::Fresh {
                value,
                observed_at_ms,
            }
            | Self::Stale {
                value,
                observed_at_ms,
                ..
            } => Self::Stale {
                value: value.clone(),
                observed_at_ms: *observed_at_ms,
                failure,
            },
            Self::Loading | Self::Failed { .. } => Self::Failed { failure, recovery },
        }
    }
}

/// Runtime support for one observable capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityState {
    /// The capability is observed inactive.
    Inactive,
    /// The capability is observed active.
    Active,
    /// Configuration is known but runtime activation cannot be verified.
    Unknown,
    /// The selected core does not claim this capability.
    Unsupported,
}

/// Derived first-run onboarding stage over trustworthy operational slices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstRunStage {
    /// No managed profile is active yet.
    NoProfile,
    /// A profile exists, but the core/controller is not currently controllable.
    CoreUnavailable,
    /// No ZenClash-owned System Proxy or requested TUN plan exists.
    CaptureNotSelected,
    /// Capture was requested but its activation is not proven.
    CaptureUnconfirmed,
    /// Capture is active but no explicit path probe has completed.
    PathUnknown,
    /// The latest path probe failed or did not traverse Mihomo.
    PathFailed,
    /// Profile, runtime, capture, and path evidence are all present.
    Ready,
}
