//! System-proxy failure facts, re-exported from [`crate::core_errors`].

pub(super) use crate::core_errors::invalid_setting;
pub use crate::core_errors::{
    SystemProxyError, SystemProxySetting, SystemProxyTransactionCause, SystemProxyValidationError,
};

#[cfg(test)]
use crate::core_errors::MihomoError;

#[cfg(test)]
mod tests;
