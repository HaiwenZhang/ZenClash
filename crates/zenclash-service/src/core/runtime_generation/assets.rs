#![deny(clippy::undocumented_unsafe_blocks)]
// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
//! Runtime generation asset validation, planning, and materialization facade.

mod materialize;
mod validate;

#[cfg(test)]
mod tests;

pub(super) use materialize::{GatheredBundle, gather_bundle, runtime_cleanup_retry_delay};
pub(crate) use materialize::{PreparedRuntime, prepare_runtime};
pub(super) use validate::{
    RUNTIME_CONFIG_FILE_NAME, destination_key, invalid_asset, resolve_in_generation, validate_destination,
};
pub(crate) use validate::{approved_core_copy, validate_core_path};
