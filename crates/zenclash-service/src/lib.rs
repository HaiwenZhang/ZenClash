//! Authenticated local protocol for the privileged ZenClash runtime service.

#![deny(missing_docs)]

#[cfg(feature = "server")]
mod api;
mod client;
mod frame;
mod installer;
#[cfg(feature = "server")]
mod kernel;
#[cfg(feature = "server")]
mod kernel_transport;
#[cfg(feature = "server")]
mod maintenance_journal;
mod metadata;
mod platform;
mod protocol;
#[cfg(feature = "server")]
mod runtime;
#[cfg(feature = "server")]
mod server;

/// Runs the installed native service until its manager requests shutdown.
///
/// This blocking entry point is used by the service binary, never the GUI.
#[cfg(feature = "server")]
pub fn run_service() -> std::io::Result<()> {
    #[cfg(windows)]
    {
        fn execute(shutdown: tokio::sync::watch::Receiver<bool>) -> std::io::Result<()> {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(server::run(shutdown))
        }
        platform::dispatch_service(execute)
    }
    #[cfg(unix)]
    {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let (sender, receiver) = tokio::sync::watch::channel(false);
                let signal = tokio::spawn(async move {
                    let result = platform::shutdown_signal().await;
                    let _ = sender.send(true);
                    result
                });
                let result = server::run(receiver).await;
                signal.abort();
                result
            })
    }
}
mod session;

pub use client::{ServiceClient, ServiceClientError, ServiceLogs, ServiceSubscription};
pub use frame::{FrameError, MAX_FRAME_BYTES, read_frame, write_frame};
#[cfg(feature = "server")]
pub use installer::run_maintenance;
pub use installer::{MaintenanceAction, maintain_service};
pub use metadata::{
    InstalledMetadata, METADATA_SCHEMA_VERSION, MetadataError, read_metadata, write_metadata_atomic,
};
pub use protocol::{
    ApiResponse as ServiceApiResponse, RuntimeStatus as ServiceRuntimeStatus, ServiceErrorCode,
    RuntimeCandidate as ServiceRuntimeCandidate,
    RuntimeCandidateKind as ServiceRuntimeCandidateKind,
    RuntimeCandidatePhase as ServiceRuntimeCandidatePhase,
    StreamKind as ServiceStream,
};
pub use protocol::{PROTOCOL_VERSION, ProtocolInfo, SessionProof, SessionToken};
