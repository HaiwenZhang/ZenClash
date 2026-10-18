//! PAC server I/O failures, retaining the failed operation and OS cause.

use std::io;
use thiserror::Error;

/// Operation that failed while starting a PAC listener.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PacServerOperation {
    /// Binding the HTTP listener.
    Bind,
    /// Configuring the listener for nonblocking access.
    ConfigureListener,
    /// Reading the listener's local address.
    ReadLocalAddress,
    /// Spawning the HTTP worker.
    SpawnWorker,
}

/// PAC listener or worker startup failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PacError {
    /// A PAC startup operation failed, retaining the OS cause.
    #[error(
        "{}",
        zenclash_i18n::text_with(
            "system_proxy.pac.errors.io",
            &[
                ("operation", operation.to_string()),
                ("error", source.to_string())
            ]
        )
    )]
    Io {
        /// Failed listener or worker operation.
        operation: PacServerOperation,
        /// Original operating-system failure.
        #[source]
        source: io::Error,
    },
}

impl std::fmt::Display for PacServerOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&zenclash_i18n::text(match self {
            Self::Bind => "system_proxy.pac.operations.bind",
            Self::ConfigureListener => "system_proxy.pac.operations.configure_listener",
            Self::ReadLocalAddress => "system_proxy.pac.operations.read_local_address",
            Self::SpawnWorker => "system_proxy.pac.operations.spawn_worker",
        }))
    }
}
