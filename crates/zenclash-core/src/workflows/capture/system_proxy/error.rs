//! System-proxy failure facts, re-exported from [`crate::core_errors`].

pub(super) use crate::mihomo::errors::invalid_setting;
pub use crate::mihomo::errors::{
    SystemProxyError, SystemProxySetting, SystemProxyTransactionCause, SystemProxyValidationError,
};

#[cfg(test)]
use crate::mihomo::errors::MihomoError;

#[cfg(test)]
mod tests;
