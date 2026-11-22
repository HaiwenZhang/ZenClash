//! Mihomo TUN configuration normalization, admission checks, permission status, and runtime observation.

pub(crate) mod admission;
pub(crate) mod config;
pub(crate) mod permissions;
pub(crate) mod runtime;

pub use permissions::{TunPermissionError, TunPermissionResult, TunPermissionStatus};
pub use runtime::{TunRuntimeObservation, TunRuntimeObserver};
