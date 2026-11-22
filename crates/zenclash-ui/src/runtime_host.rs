//! Layer 2: runtime host services, task tokens, config inputs, observation, and scheduling.

pub(crate) mod busy;
pub(crate) mod config_inputs;
pub(crate) mod lifecycle_schedule;
pub(crate) mod observation;
/// Runtime page service dependencies, preference scopes, and host events.
pub mod services;
pub(crate) mod task_token;
