use std::io;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::sync::Mutex;
use tokio::task::AbortHandle;

use crate::protocol::{
    ApiRequest, ApiResponse, LogStreamOptions, PreparedRuntimePatch, ProviderCacheChunk,
    ProviderCacheRead, ProviderCacheToken, ProviderKind, Request, Response, RuntimeStatus,
    ServiceErrorCode, SessionOperation, StreamKind,
};
use crate::{FrameError, PROTOCOL_VERSION, ProtocolInfo, SessionProof, read_frame, write_frame};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const MAX_ASSET_CHUNK: usize = 256 * 1024;

pub(crate) trait LocalIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> LocalIo for T {}
type Transport = Box<dyn LocalIo>;

#[cfg(feature = "server")]
pub(crate) struct MaintenanceHost {
    stream: crate::platform::MaintenanceStream,
    peer: crate::session::PeerIdentity,
}

#[cfg(feature = "server")]
impl MaintenanceHost {
    pub(crate) async fn probe() -> Result<Self, ServiceClientError> {
        tokio::time::timeout(REQUEST_TIMEOUT, async {
            let mut stream = crate::platform::connect()
                .await
                .map_err(ServiceClientError::Connection)?;
            let peer = maintenance_hello(&mut stream, crate::platform::maintenance_peer).await?;
            Ok(Self { stream, peer })
        })
        .await
        .map_err(|_| ServiceClientError::Timeout)?
    }

    pub(crate) fn revalidate(&self) -> io::Result<()> {
        if crate::platform::maintenance_peer(&self.stream)? != self.peer {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "maintenance host changed",
            ));
        }
        crate::platform::maintenance_host_registered(&self.peer)?;
        if crate::platform::maintenance_peer(&self.stream)? != self.peer {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "maintenance host changed during registered identity query",
            ));
        }
        Ok(())
    }

    pub(crate) fn peer(&self) -> &crate::session::PeerIdentity {
        &self.peer
    }
}

#[cfg(feature = "server")]
pub(crate) async fn maintenance_hello<T: LocalIo>(
    stream: &mut T,
    mut identify: impl FnMut(&T) -> io::Result<crate::session::PeerIdentity> + Send,
) -> Result<crate::session::PeerIdentity, ServiceClientError> {
    let peer = identify(stream).map_err(ServiceClientError::Connection)?;
    hello(stream).await?;
    if identify(stream).map_err(ServiceClientError::Connection)? != peer {
        return Err(ServiceClientError::Connection(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "maintenance host changed during handshake",
        )));
    }
    Ok(peer)
}

/// A bounded client or service rejection, without private request contents.
#[derive(Debug, thiserror::Error)]
pub enum ServiceClientError {
    /// Native service connection or identity verification failed.
    #[error("service connection failed")]
    Connection(#[source] io::Error),
    /// Protocol negotiation failed before any session operation was submitted.
    #[error("service handshake failed")]
    Preflight(#[source] Box<ServiceClientError>),
    /// The peer could not exchange a complete bounded frame.
    #[error("service message exchange failed")]
    Frame(#[from] FrameError),
    /// The service rejected the requested business operation.
    #[error("service rejected operation: {0:?}")]
    Rejected(ServiceErrorCode),
    /// An unexpected response was received.
    #[error("unexpected service response")]
    UnexpectedResponse,
    /// This client is releasing its session and accepts no new operations.
    #[error("service session is closing")]
    Closing,
    /// No further strictly monotonic request sequence can be allocated.
    #[error("service request sequence exhausted")]
    SequenceExhausted,
    /// The native connection or handshake exceeded its deadline.
    #[error("service connection timed out")]
    Timeout,
}

/// A bounded slice of captured service-managed kernel logs.
#[derive(Clone, Debug)]
pub struct ServiceLogs {
    /// Cursor to use for the next read.
    pub cursor: u64,
    /// Log lines captured since the requested cursor.
    pub lines: Vec<String>,
}

struct ClientChannel {
    stream: Option<Transport>,
    proof: SessionProof,
    sequence: u64,
}

impl ClientChannel {
    fn next_request(&mut self, operation: SessionOperation) -> Result<Request, ServiceClientError> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or(ServiceClientError::SequenceExhausted)?;
        Ok(Request::Session {
            proof: self.proof.clone(),
            sequence: self.sequence,
            operation,
        })
    }

    async fn exchange(
        &mut self,
        operation: SessionOperation,
    ) -> Result<Response, ServiceClientError> {
        // Taking the transport before awaiting makes cancellation discard a
        // possibly partial frame. Sequence values are never reused on retry.
        let mut stream = match self.stream.take() {
            Some(stream) => stream,
            None => open_verified().await?,
        };
        let request = self.next_request(operation)?;
        let response = exchange(&mut stream, &request).await?;
        self.stream = Some(stream);
        accepted(response)
    }
}

/// Authenticated owner of one service session, shared by application tasks.
///
/// Native identity is verified on every connection. Failed transports reconnect
/// with the original proof and increasing sequence; mutations are never retried
/// automatically. The snapshot is prepared in memory by status/heartbeat work.
pub struct ServiceClient {
    channel: Mutex<ClientChannel>,
    snapshot: RwLock<Option<RuntimeStatus>>,
    closing: AtomicBool,
    heartbeat: RwLock<Option<AbortHandle>>,
}

impl ServiceClient {
    /// Verifies the service, negotiates its protocol and acquires ownership.
    ///
    /// # Errors
    /// Returns native identity, connection, protocol, authorization or ownership errors.
    pub async fn connect() -> Result<Arc<Self>, ServiceClientError> {
        let mut stream = open_verified().await?;
        let Response::Acquired {
            proof,
            next_sequence,
        } = accepted(exchange(&mut stream, &Request::Acquire {}).await?)?
        else {
            return Err(ServiceClientError::UnexpectedResponse);
        };
        let sequence = next_sequence
            .checked_sub(1)
            .ok_or(ServiceClientError::UnexpectedResponse)?;
        let client = Arc::new(Self {
            channel: Mutex::new(ClientChannel {
                stream: Some(stream),
                proof,
                sequence,
            }),
            snapshot: RwLock::new(None),
            closing: AtomicBool::new(false),
            heartbeat: RwLock::new(None),
        });
        let weak = Arc::downgrade(&client);
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(HEARTBEAT_INTERVAL).await;
                let Some(client) = weak.upgrade() else {
                    break;
                };
                if client.closing.load(Ordering::Acquire) {
                    break;
                }
                let _ = client.status().await;
            }
        });
        *client
            .heartbeat
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(task.abort_handle());
        Ok(client)
    }

    /// Checks verified service health without reserving the system runtime.
    ///
    /// # Errors
    /// Returns native identity, connection or incompatible-protocol errors.
    pub async fn probe() -> Result<ProtocolInfo, ServiceClientError> {
        let mut stream = native_connect().await?;
        hello(&mut stream).await
    }

    /// Checks whether the verified service has an owner or pending kernel state.
    /// Does not acquire a session or change the running configuration.
    ///
    /// # Errors
    /// Reports unavailable or incompatible service evidence.
    pub async fn inspect_busy() -> Result<bool, ServiceClientError> {
        let mut stream = native_connect().await?;
        hello(&mut stream).await?;
        write_frame(&mut stream, &Request::Inspect {}, Duration::from_secs(1)).await?;
        match read_frame(&mut stream, Duration::from_secs(1)).await? {
            Response::Inspection { busy } => Ok(busy),
            Response::Error { code } => Err(ServiceClientError::Rejected(code)),
            _ => Err(ServiceClientError::UnexpectedResponse),
        }
    }

    /// Reads the last prepared runtime observation without performing IPC.
    pub fn snapshot(&self) -> Option<RuntimeStatus> {
        self.snapshot
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    async fn request(&self, operation: SessionOperation) -> Result<Response, ServiceClientError> {
        if self.closing.load(Ordering::Acquire) {
            return Err(ServiceClientError::Closing);
        }
        let mut channel = self.channel.lock().await;
        if self.closing.load(Ordering::Acquire) {
            return Err(ServiceClientError::Closing);
        }
        channel.exchange(operation).await
    }

    /// Validates and stages runtime YAML; returns its service revision.
    ///
    /// # Errors
    /// Returns session, configuration, staging-budget or native transport errors.
    pub async fn stage(&self, config: &str) -> Result<u64, ServiceClientError> {
        match self
            .request(SessionOperation::Stage {
                config: config.to_owned(),
            })
            .await?
        {
            Response::Staged { revision } => Ok(revision),
            _ => Err(ServiceClientError::UnexpectedResponse),
        }
    }

    /// Uploads one bounded chunk of a relative runtime asset.
    ///
    /// # Errors
    /// Rejects oversized chunks, invalid paths or offsets, closed staging and transport failures.
    pub async fn upload_asset(
        &self,
        path: &str,
        offset: u64,
        bytes: &[u8],
        finished: bool,
    ) -> Result<(), ServiceClientError> {
        if bytes.len() > MAX_ASSET_CHUNK {
            return Err(ServiceClientError::Rejected(
                ServiceErrorCode::BudgetExceeded,
            ));
        }
        expect_ok(
            self.request(SessionOperation::UploadAsset {
                path: path.to_owned(),
                offset,
                bytes: bytes.to_vec(),
                finished,
            })
            .await?,
        )
    }

    /// Starts the approved kernel with the exact staged revision.
    ///
    /// # Errors
    /// Returns revision, approval, readiness, session or transport failures without retrying Start.
    pub async fn start(&self, revision: u64) -> Result<(), ServiceClientError> {
        expect_ok(self.request(SessionOperation::Start { revision }).await?)
    }

    /// Validates a complete staged configuration with the approved real kernel.
    ///
    /// # Errors
    /// Returns revision, resource-budget, kernel validation or transport failures.
    pub async fn validate(&self, revision: u64) -> Result<(), ServiceClientError> {
        expect_ok(
            self.request(SessionOperation::Validate { revision })
                .await?,
        )
    }

    /// Applies a staged revision while preserving the committed rollback revision.
    ///
    /// # Errors
    /// Returns revision, kernel, session or transport errors; lost responses remain unconfirmed.
    pub async fn reload(&self, revision: u64, force: bool) -> Result<(), ServiceClientError> {
        expect_ok(
            self.request(SessionOperation::Reload { revision, force })
                .await?,
        )
    }

    /// Commits the applied revision after the application's local transaction succeeds.
    ///
    /// # Errors
    /// Rejects stale or unconfirmed revisions and propagates session or transport failures.
    pub async fn commit_runtime(&self, revision: u64) -> Result<(), ServiceClientError> {
        expect_ok(
            self.request(SessionOperation::CommitRuntime { revision })
                .await?,
        )
    }

    /// Prepares a bounded partial candidate from an accepted revision without changing the kernel.
    /// Repeating the same base and patch returns the retained candidate without resetting its phase.
    ///
    /// # Errors
    /// Returns unsupported fields, stale base, budget, kernel observation or transport errors.
    pub async fn prepare_runtime_patch(
        &self,
        base_revision: u64,
        patch: &serde_json::Value,
    ) -> Result<PreparedRuntimePatch, ServiceClientError> {
        match self
            .request(SessionOperation::PrepareRuntimePatch {
                base_revision,
                patch: patch.clone(),
            })
            .await?
        {
            Response::RuntimePatchPrepared { prepared } => Ok(prepared),
            _ => Err(ServiceClientError::UnexpectedResponse),
        }
    }

    /// Applies only the candidate's managed fields and verifies their same-kernel readback.
    /// An uncertain candidate is observed without blindly resending its PATCH.
    ///
    /// # Errors
    /// Returns stale candidate, policy, kernel, unknown-outcome or transport errors.
    pub async fn apply_runtime_patch(&self, revision: u64) -> Result<(), ServiceClientError> {
        expect_ok(
            self.request(SessionOperation::ApplyRuntimePatch { revision })
                .await?,
        )
    }

    /// Restores the held old fields of an unsaved partial candidate.
    /// A confirmed stopped kernel permits discarding only its candidate configuration.
    /// Failed cleanup retains the candidate for a later explicit recovery.
    /// Application code must never call this after business persistence has succeeded.
    ///
    /// # Errors
    /// Returns stale candidate, cleanup, kernel, unknown-outcome or transport errors.
    pub async fn restore_runtime_patch(&self, revision: u64) -> Result<(), ServiceClientError> {
        expect_ok(
            self.request(SessionOperation::RestoreRuntimePatch { revision })
                .await?,
        )
    }

    /// Stops and waits for the service-managed kernel while retaining ownership.
    ///
    /// # Errors
    /// Returns session, native stop or transport errors; a lost response does not prove exit.
    pub async fn stop(&self) -> Result<(), ServiceClientError> {
        expect_ok(self.request(SessionOperation::Stop {}).await?)
    }

    /// Begins a declared HTTP cache snapshot after the kernel has been confirmed stopped.
    /// This request does not stop the kernel or open arbitrary filesystem paths.
    ///
    /// # Errors
    /// Rejects running, stale, uncommitted or candidate-bearing runtimes, invalid providers,
    /// exhausted readback budgets, and session or transport failures.
    pub async fn begin_provider_cache_read(
        &self,
        revision: u64,
        kind: ProviderKind,
        name: &str,
    ) -> Result<ProviderCacheRead, ServiceClientError> {
        match self
            .request(SessionOperation::BeginProviderCacheRead {
                revision,
                kind,
                name: name.to_owned(),
            })
            .await?
        {
            Response::ProviderCacheRead { snapshot } => Ok(snapshot),
            _ => Err(ServiceClientError::UnexpectedResponse),
        }
    }

    /// Reads the next 256 KiB page before the snapshot's absolute deadline.
    ///
    /// # Errors
    /// Rejects stale tokens, out-of-order offsets, expired snapshots, changed runtimes,
    /// and session or transport failures. Failed pages are not retried automatically.
    pub async fn read_provider_cache(
        &self,
        token: &ProviderCacheToken,
        offset: u64,
    ) -> Result<ProviderCacheChunk, ServiceClientError> {
        match self
            .request(SessionOperation::ReadProviderCache {
                token: token.clone(),
                offset,
            })
            .await?
        {
            Response::ProviderCacheChunk { chunk } => Ok(chunk),
            _ => Err(ServiceClientError::UnexpectedResponse),
        }
    }

    /// Finishes or cancels the retained snapshot without resetting cumulative wave budgets.
    /// Dropped waiters release server memory at its absolute deadline or owner cleanup.
    ///
    /// # Errors
    /// Rejects stale tokens and propagates session or transport errors.
    pub async fn finish_provider_cache_read(
        &self,
        token: &ProviderCacheToken,
    ) -> Result<(), ServiceClientError> {
        expect_ok(
            self.request(SessionOperation::FinishProviderCacheRead {
                token: token.clone(),
            })
            .await?,
        )
    }

    /// Reads one complete stopped-runtime cache and verifies its length and SHA-256.
    /// The channel is retained across all pages, preventing Start or Release from
    /// interleaving. Missing caches return `None`; empty caches return `Some(vec![])`.
    /// No request is retried. Cancelled or failed reads expire on the service deadline.
    ///
    /// # Errors
    /// Returns admission, transport, invalid-page, digest, finish or deadline errors.
    /// This does not stop the kernel or save bytes to the user's filesystem.
    pub async fn read_complete_provider_cache(
        &self,
        revision: u64,
        kind: ProviderKind,
        name: &str,
    ) -> Result<Option<Vec<u8>>, ServiceClientError> {
        if self.closing.load(Ordering::Acquire) {
            return Err(ServiceClientError::Closing);
        }
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut channel = self.channel.lock().await;
            if self.closing.load(Ordering::Acquire) {
                return Err(ServiceClientError::Closing);
            }
            let snapshot = channel
                .exchange(SessionOperation::BeginProviderCacheRead {
                    revision,
                    kind,
                    name: name.to_owned(),
                })
                .await?;
            let Response::ProviderCacheRead { snapshot } = snapshot else {
                return Err(ServiceClientError::UnexpectedResponse);
            };
            let ProviderCacheRead::Ready { token, len, sha256 } = snapshot else {
                return Ok(None);
            };
            let limit = match kind {
                ProviderKind::Proxy => 4 * 1024 * 1024,
                ProviderKind::Rule => 128 * 1024 * 1024,
            };
            if len > limit {
                return Err(ServiceClientError::UnexpectedResponse);
            }
            let mut bytes = Vec::new();
            let mut digest = Sha256::new();
            loop {
                let offset = bytes.len() as u64;
                let response = channel
                    .exchange(SessionOperation::ReadProviderCache {
                        token: token.clone(),
                        offset,
                    })
                    .await?;
                let Response::ProviderCacheChunk { chunk } = response else {
                    return Err(ServiceClientError::UnexpectedResponse);
                };
                let end = offset + chunk.bytes.len() as u64;
                if chunk.offset != offset
                    || chunk.bytes.len() > MAX_ASSET_CHUNK
                    || end > len
                    || chunk.finished != (end == len)
                    || (!chunk.finished && chunk.bytes.is_empty())
                {
                    return Err(ServiceClientError::UnexpectedResponse);
                }
                digest.update(&chunk.bytes);
                bytes.extend_from_slice(&chunk.bytes);
                if chunk.finished {
                    break;
                }
            }
            expect_ok(
                channel
                    .exchange(SessionOperation::FinishProviderCacheRead { token })
                    .await?,
            )?;
            if <[u8; 32]>::from(digest.finalize()) != sha256 {
                return Err(ServiceClientError::UnexpectedResponse);
            }
            Ok(Some(bytes))
        })
        .await
        .map_err(|_| ServiceClientError::Timeout)?
    }

    /// Stops the kernel, clears staged data, and releases ownership.
    ///
    /// New work and heartbeat stop immediately, including when release fails.
    /// The service's lease cleanup remains the bounded fallback on disconnect.
    ///
    /// # Errors
    /// Returns confirmed-stop, private cleanup or transport failures while closing new admission.
    pub async fn release(&self) -> Result<(), ServiceClientError> {
        self.closing.store(true, Ordering::Release);
        if let Some(task) = self
            .heartbeat
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            task.abort();
        }
        let mut channel = self.channel.lock().await;
        expect_ok(channel.exchange(SessionOperation::Release {}).await?)
    }

    /// Refreshes runtime observations and the lease, then caches the result.
    ///
    /// # Errors
    /// Returns session, kernel observation or transport errors and clears the cached observation.
    pub async fn status(&self) -> Result<RuntimeStatus, ServiceClientError> {
        let response = self.request(SessionOperation::Status {}).await;
        let snapshot = match response {
            Ok(Response::Status { snapshot }) => snapshot,
            other => {
                *self
                    .snapshot
                    .write()
                    .unwrap_or_else(|error| error.into_inner()) = None;
                return Err(match other {
                    Err(error) => error,
                    Ok(_) => ServiceClientError::UnexpectedResponse,
                });
            }
        };
        *self
            .snapshot
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(snapshot.clone());
        Ok(snapshot)
    }

    /// Reads bounded recent kernel output from a service cursor.
    ///
    /// # Errors
    /// Returns session, response-shape or transport errors.
    pub async fn logs(&self, cursor: u64) -> Result<ServiceLogs, ServiceClientError> {
        match self.request(SessionOperation::Logs { cursor }).await? {
            Response::Logs { cursor, lines } => Ok(ServiceLogs { cursor, lines }),
            _ => Err(ServiceClientError::UnexpectedResponse),
        }
    }

    /// Forwards a named kernel API request through service-side policy checks.
    ///
    /// # Errors
    /// Rejects requests outside the fixed policy and propagates kernel or transport failures.
    pub async fn api(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<ApiResponse, ServiceClientError> {
        match self
            .request(SessionOperation::Api {
                request: ApiRequest {
                    method: method.to_owned(),
                    path: path.to_owned(),
                    body,
                },
            })
            .await?
        {
            Response::Api { response } => Ok(response),
            _ => Err(ServiceClientError::UnexpectedResponse),
        }
    }

    /// Subscribes on a separate verified connection with shared request ordering.
    /// Logs use the named operation with Info and Structured options.
    ///
    /// # Errors
    /// Returns closing-session, native identity, stream admission or transport errors.
    pub async fn subscribe(
        &self,
        kind: StreamKind,
    ) -> Result<ServiceSubscription, ServiceClientError> {
        let operation = match kind {
            StreamKind::Logs => SessionOperation::SubscribeLogs {
                options: LogStreamOptions::default(),
            },
            _ => SessionOperation::Subscribe { stream: kind },
        };
        self.subscribe_operation(operation).await
    }

    /// Subscribes to logs with explicit severity and JSON representation.
    ///
    /// Old services reject this named operation. Failures never retry the
    /// legacy unparameterized log subscription or acquire another owner.
    ///
    /// # Errors
    /// Returns closing-session, native identity, stream admission or transport errors.
    pub async fn subscribe_logs(
        &self,
        options: LogStreamOptions,
    ) -> Result<ServiceSubscription, ServiceClientError> {
        self.subscribe_operation(SessionOperation::SubscribeLogs { options })
            .await
    }

    async fn subscribe_operation(
        &self,
        operation: SessionOperation,
    ) -> Result<ServiceSubscription, ServiceClientError> {
        if self.closing.load(Ordering::Acquire) {
            return Err(ServiceClientError::Closing);
        }
        let mut channel = self.channel.lock().await;
        if self.closing.load(Ordering::Acquire) {
            return Err(ServiceClientError::Closing);
        }
        let stream = open_verified().await?;
        let subscription = subscribe_connection(&mut channel, stream, operation).await?;
        // Server admission is confirmed before other connections allocate a
        // later sequence, preventing cross-connection arrival races.
        drop(channel);
        Ok(subscription)
    }
}

async fn subscribe_connection(
    channel: &mut ClientChannel,
    mut stream: Transport,
    operation: SessionOperation,
) -> Result<ServiceSubscription, ServiceClientError> {
    let request = channel.next_request(operation)?;
    expect_ok(accepted(exchange(&mut stream, &request).await?)?)?;
    Ok(ServiceSubscription {
        stream: Some(stream),
    })
}

impl Drop for ServiceClient {
    fn drop(&mut self) {
        if let Some(task) = self
            .heartbeat
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            task.abort();
        }
    }
}

/// A bounded native kernel event stream tied to its owner's service session.
pub struct ServiceSubscription {
    stream: Option<Transport>,
}

impl ServiceSubscription {
    /// Waits for the next event; returns `None` after a clean peer disconnect.
    ///
    /// Idle waiting does not expire the application lease; the control client's
    /// heartbeat owns that lease. Once a frame starts its body has a deadline.
    /// A cancelled or failed read discards the transport to prevent reuse of a
    /// partially consumed frame.
    ///
    /// # Errors
    /// Returns framing, stream policy or native transport errors and discards the connection.
    pub async fn next(&mut self) -> Result<Option<Value>, ServiceClientError> {
        let Some(mut stream) = self.stream.take() else {
            return Ok(None);
        };
        let first = match stream.read_u8().await {
            Ok(first) => first,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(ServiceClientError::Frame(FrameError::Io(error))),
        };
        let mut reader = std::io::Cursor::new([first]).chain(&mut stream);
        let response: Response = read_frame(&mut reader, REQUEST_TIMEOUT).await?;
        match accepted(response)? {
            Response::Stream { data } => {
                self.stream = Some(stream);
                Ok(Some(data))
            }
            _ => Err(ServiceClientError::UnexpectedResponse),
        }
    }
}

async fn native_connect() -> Result<Transport, ServiceClientError> {
    let stream = tokio::time::timeout(REQUEST_TIMEOUT, crate::platform::connect())
        .await
        .map_err(|_| ServiceClientError::Timeout)?
        .map_err(ServiceClientError::Connection)?;
    Ok(Box::new(stream))
}

async fn open_verified() -> Result<Transport, ServiceClientError> {
    let mut stream = native_connect().await?;
    hello(&mut stream).await?;
    Ok(stream)
}

async fn hello(stream: &mut impl LocalIo) -> Result<ProtocolInfo, ServiceClientError> {
    match accepted(
        exchange(
            stream,
            &Request::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .await
        .map_err(preflight_error)?,
    )
    .map_err(preflight_error)?
    {
        Response::Hello { info } if info.is_compatible() => Ok(info),
        Response::Hello { .. } => Err(ServiceClientError::Rejected(ServiceErrorCode::Incompatible)),
        _ => Err(preflight_error(ServiceClientError::UnexpectedResponse)),
    }
}

fn preflight_error(error: ServiceClientError) -> ServiceClientError {
    match error {
        ServiceClientError::Frame(FrameError::Timeout) | ServiceClientError::Timeout => {
            ServiceClientError::Timeout
        }
        ServiceClientError::Connection(_) => error,
        ServiceClientError::Rejected(code)
            if !matches!(
                code,
                ServiceErrorCode::OutcomeUnknown
                    | ServiceErrorCode::Internal
                    | ServiceErrorCode::KernelFailed
            ) =>
        {
            error
        }
        error => ServiceClientError::Preflight(Box::new(error)),
    }
}

async fn exchange(
    stream: &mut impl LocalIo,
    request: &Request,
) -> Result<Response, ServiceClientError> {
    tokio::time::timeout(REQUEST_TIMEOUT, async {
        write_frame(stream, request, REQUEST_TIMEOUT).await?;
        read_frame(stream, REQUEST_TIMEOUT).await
    })
    .await
    .map_err(|_| ServiceClientError::Frame(FrameError::Timeout))?
    .map_err(ServiceClientError::Frame)
}

fn accepted(response: Response) -> Result<Response, ServiceClientError> {
    match response {
        Response::Error { code } => Err(ServiceClientError::Rejected(code)),
        response => Ok(response),
    }
}

fn expect_ok(response: Response) -> Result<(), ServiceClientError> {
    match response {
        Response::Ok => Ok(()),
        _ => Err(ServiceClientError::UnexpectedResponse),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionToken;
    use tokio::io::AsyncWriteExt;

    fn channel(stream: tokio::io::DuplexStream) -> ClientChannel {
        ClientChannel {
            stream: Some(Box::new(stream)),
            proof: SessionProof::new(1, SessionToken([1; 32])),
            sequence: 0,
        }
    }

    #[tokio::test]
    async fn same_version_unknown_logs_operation_has_no_legacy_retry_or_owner_reset() {
        // A same-version peer with an unknown operation closes its stream.
        #[derive(serde::Deserialize)]
        #[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
        enum OldOperation {
            Subscribe { stream: StreamKind },
        }
        let (control, _control_peer) = tokio::io::duplex(4096);
        let mut channel = channel(control);
        let original_proof = channel.proof.clone();
        let (client, mut peer) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let request: Request = read_frame(&mut peer, REQUEST_TIMEOUT).await.unwrap();
            assert!(matches!(
                request,
                Request::Hello {
                    protocol_version: PROTOCOL_VERSION
                }
            ));
            write_frame(
                &mut peer,
                &Response::Hello {
                    info: ProtocolInfo::current(),
                },
                REQUEST_TIMEOUT,
            )
            .await
            .unwrap();
            let request: Value = read_frame(&mut peer, REQUEST_TIMEOUT).await.unwrap();
            assert_eq!(request["operation"]["action"], "subscribe_logs");
            assert_eq!(request["sequence"], 1);
            assert_eq!(request["proof"]["generation"], 1);
            assert!(serde_json::from_value::<OldOperation>(request["operation"].clone()).is_err());
            // Old service closes malformed requests; leave the read half open
            // to observe any erroneous legacy retry on the same connection.
            peer.shutdown().await.unwrap();
            assert!(matches!(
                read_frame::<_, Value>(&mut peer, REQUEST_TIMEOUT).await,
                Err(FrameError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof
            ));
            let OldOperation::Subscribe { stream } =
                serde_json::from_value(serde_json::json!({"action":"subscribe", "stream":"logs"}))
                    .unwrap();
            assert!(matches!(stream, StreamKind::Logs));
        });
        let mut transport: Transport = Box::new(client);
        hello(&mut transport).await.unwrap();
        let result = subscribe_connection(
            &mut channel,
            transport,
            SessionOperation::SubscribeLogs {
                options: LogStreamOptions::default(),
            },
        )
        .await;
        assert!(matches!(
            result,
            Err(ServiceClientError::Frame(FrameError::Io(_)))
        ));
        server.await.unwrap();
        assert_eq!(channel.proof, original_proof);
        assert!(matches!(
            channel.next_request(SessionOperation::Status {}).unwrap(),
            Request::Session { sequence: 2, .. }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn mutation_received_without_a_response_times_out_with_unknown_outcome() {
        let (client, mut server) = tokio::io::duplex(1);
        let task = tokio::spawn(async move {
            let mut client = channel(client);
            let result = client
                .exchange(SessionOperation::Start { revision: 7 })
                .await;
            (result, client)
        });
        // Backpressure consumes part of the whole-exchange deadline before the
        // mutation is delivered, so the response-only timer expires later.
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let request: Request = read_frame(&mut server, REQUEST_TIMEOUT).await.unwrap();
        assert!(matches!(
            request,
            Request::Session {
                operation: SessionOperation::Start { revision: 7 },
                ..
            }
        ));
        tokio::time::advance(REQUEST_TIMEOUT - Duration::from_secs(1) + Duration::from_millis(1))
            .await;
        let (result, client) = task.await.unwrap();
        assert!(matches!(
            result,
            Err(ServiceClientError::Frame(FrameError::Timeout))
        ));
        assert!(client.stream.is_none());
        assert_eq!(client.sequence, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn handshake_timeout_precedes_any_session_mutation() {
        let (client, mut server) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            let mut client: Transport = Box::new(client);
            hello(&mut client).await
        });
        let request: Request = read_frame(&mut server, REQUEST_TIMEOUT).await.unwrap();
        assert!(matches!(request, Request::Hello { .. }));
        tokio::time::advance(REQUEST_TIMEOUT + Duration::from_millis(1)).await;
        assert!(matches!(
            task.await.unwrap(),
            Err(ServiceClientError::Timeout)
        ));
    }

    #[tokio::test]
    async fn invalid_handshake_response_is_a_preflight_failure() {
        let (client, mut server) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            let request: Request = read_frame(&mut server, REQUEST_TIMEOUT).await.unwrap();
            assert!(matches!(request, Request::Hello { .. }));
            write_frame(&mut server, &Response::Ok, REQUEST_TIMEOUT)
                .await
                .unwrap();
        });
        let mut client: Transport = Box::new(client);
        assert!(matches!(
            hello(&mut client).await,
            Err(ServiceClientError::Preflight(_))
        ));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn rejected_requests_consume_their_sequence_before_later_mutations() {
        let (client, mut server) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            for sequence in [1, 2] {
                let Request::Session {
                    sequence: actual, ..
                } = read_frame(&mut server, REQUEST_TIMEOUT).await.unwrap()
                else {
                    panic!("expected session operation")
                };
                assert_eq!(actual, sequence);
                write_frame(
                    &mut server,
                    &if sequence == 1 {
                        Response::Error {
                            code: ServiceErrorCode::InvalidConfiguration,
                        }
                    } else {
                        Response::Ok
                    },
                    REQUEST_TIMEOUT,
                )
                .await
                .unwrap();
            }
        });
        let mut client = channel(client);
        assert!(matches!(
            client
                .exchange(SessionOperation::Stage {
                    config: "bad".into()
                })
                .await,
            Err(ServiceClientError::Rejected(
                ServiceErrorCode::InvalidConfiguration
            ))
        ));
        assert!(client.exchange(SessionOperation::Stop {}).await.is_ok());
        task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancelled_exchange_discards_transport_and_never_reuses_sequence() {
        let (client, _server) = tokio::io::duplex(4096);
        let mut client = channel(client);
        assert!(
            tokio::time::timeout(
                Duration::from_secs(1),
                client.exchange(SessionOperation::Stop {})
            )
            .await
            .is_err()
        );
        assert!(client.stream.is_none());
        let Request::Session { sequence, .. } =
            client.next_request(SessionOperation::Status {}).unwrap()
        else {
            panic!("expected session operation")
        };
        assert_eq!(sequence, 2);
    }

    #[tokio::test]
    async fn failed_status_readback_invalidates_the_previous_running_snapshot() {
        let (client, mut server) = tokio::io::duplex(4096);
        let service = ServiceClient {
            channel: Mutex::new(channel(client)),
            snapshot: RwLock::new(Some(RuntimeStatus {
                candidate: None,
                applied_revision: None,
                committed_revision: None,
                running: true,
                pid: Some(42),
                exit_reason: None,
            })),
            closing: AtomicBool::new(false),
            heartbeat: RwLock::new(None),
        };
        let task = tokio::spawn(async move {
            let _: Request = read_frame(&mut server, REQUEST_TIMEOUT).await.unwrap();
            write_frame(
                &mut server,
                &Response::Error {
                    code: ServiceErrorCode::Expired,
                },
                REQUEST_TIMEOUT,
            )
            .await
            .unwrap();
        });
        assert!(service.status().await.is_err());
        assert!(service.snapshot().is_none());
        task.await.unwrap();
    }

    fn cache_client() -> (ServiceClient, tokio::io::DuplexStream) {
        let (client, server) = tokio::io::duplex(4096);
        (
            ServiceClient {
                channel: Mutex::new(channel(client)),
                snapshot: RwLock::new(None),
                closing: AtomicBool::new(false),
                heartbeat: RwLock::new(None),
            },
            server,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn complete_cache_read_total_deadline_includes_channel_queue_without_begin() {
        let (service, mut peer) = cache_client();
        let service = Arc::new(service);
        let queued = service.clone();
        let guard = service.channel.lock().await;
        let task = tokio::spawn(async move {
            queued
                .read_complete_provider_cache(7, ProviderKind::Rule, "rules")
                .await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(15)).await;
        tokio::task::yield_now().await;
        assert!(
            task.is_finished(),
            "cache read exceeded its deadline in the queue"
        );
        assert!(matches!(
            task.await.unwrap(),
            Err(ServiceClientError::Timeout)
        ));
        drop(guard);
        drop(service);
        assert!(
            matches!(read_frame::<_, Request>(&mut peer, REQUEST_TIMEOUT).await,
            Err(FrameError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn complete_cache_read_total_deadline_includes_begin_and_all_pages() {
        let (service, mut peer) = cache_client();
        let task = tokio::spawn(async move {
            service
                .read_complete_provider_cache(7, ProviderKind::Rule, "rules")
                .await
        });
        let _: Request = read_frame(&mut peer, REQUEST_TIMEOUT).await.unwrap();
        tokio::time::advance(Duration::from_secs(10)).await;
        write_frame(
            &mut peer,
            &Response::ProviderCacheRead {
                snapshot: ProviderCacheRead::Ready {
                    token: ProviderCacheToken([7; 32]),
                    len: 1,
                    sha256: Sha256::digest([1]).into(),
                },
            },
            REQUEST_TIMEOUT,
        )
        .await
        .unwrap();
        let _: Request = read_frame(&mut peer, REQUEST_TIMEOUT).await.unwrap();
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(matches!(
            task.await.unwrap(),
            Err(ServiceClientError::Timeout)
        ));
    }

    #[tokio::test]
    async fn complete_cache_read_oversized_snapshot_is_rejected_before_paging() {
        let (service, mut peer) = cache_client();
        let server = tokio::spawn(async move {
            let _: Request = read_frame(&mut peer, REQUEST_TIMEOUT).await.unwrap();
            write_frame(
                &mut peer,
                &Response::ProviderCacheRead {
                    snapshot: ProviderCacheRead::Ready {
                        token: ProviderCacheToken([8; 32]),
                        len: 4 * 1024 * 1024 + 1,
                        sha256: [0; 32],
                    },
                },
                REQUEST_TIMEOUT,
            )
            .await
            .unwrap();
            assert!(
                matches!(read_frame::<_, Request>(&mut peer, REQUEST_TIMEOUT).await,
                Err(FrameError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof)
            );
        });
        assert!(matches!(
            service
                .read_complete_provider_cache(7, ProviderKind::Proxy, "proxy")
                .await,
            Err(ServiceClientError::UnexpectedResponse)
        ));
        drop(service);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn complete_cache_read_pages_verifies_and_finishes_before_returning_bytes() {
        let (service, mut peer) = cache_client();
        let bytes = vec![b'x'; MAX_ASSET_CHUNK + 7];
        let expected = bytes.clone();
        let server = tokio::spawn(async move {
            let request: Request = read_frame(&mut peer, REQUEST_TIMEOUT).await.unwrap();
            assert!(matches!(request, Request::Session {
                operation: SessionOperation::BeginProviderCacheRead {
                    revision: 7, kind: ProviderKind::Rule, name,
                }, ..
            } if name == "rules"));
            write_frame(
                &mut peer,
                &Response::ProviderCacheRead {
                    snapshot: ProviderCacheRead::Ready {
                        token: ProviderCacheToken([3; 32]),
                        len: bytes.len() as u64,
                        sha256: Sha256::digest(&bytes).into(),
                    },
                },
                REQUEST_TIMEOUT,
            )
            .await
            .unwrap();
            for (index, page) in bytes.chunks(MAX_ASSET_CHUNK).enumerate() {
                let expected_offset = (index * MAX_ASSET_CHUNK) as u64;
                let request: Request = read_frame(&mut peer, REQUEST_TIMEOUT).await.unwrap();
                assert!(matches!(request, Request::Session {
                    operation: SessionOperation::ReadProviderCache { token, offset }, ..
                } if token == ProviderCacheToken([3; 32]) && offset == expected_offset));
                write_frame(
                    &mut peer,
                    &Response::ProviderCacheChunk {
                        chunk: ProviderCacheChunk {
                            offset: expected_offset,
                            bytes: page.to_vec(),
                            finished: index == 1,
                        },
                    },
                    REQUEST_TIMEOUT,
                )
                .await
                .unwrap();
            }
            let request: Request = read_frame(&mut peer, REQUEST_TIMEOUT).await.unwrap();
            assert!(matches!(request, Request::Session {
                operation: SessionOperation::FinishProviderCacheRead { token }, ..
            } if token == ProviderCacheToken([3; 32])));
            write_frame(&mut peer, &Response::Ok, REQUEST_TIMEOUT)
                .await
                .unwrap();
        });
        assert_eq!(
            service
                .read_complete_provider_cache(7, ProviderKind::Rule, "rules")
                .await
                .unwrap(),
            Some(expected)
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn complete_cache_read_distinguishes_missing_cache_from_empty_cache() {
        for absent in [true, false] {
            let (service, mut peer) = cache_client();
            let server = tokio::spawn(async move {
                let _: Request = read_frame(&mut peer, REQUEST_TIMEOUT).await.unwrap();
                let snapshot = if absent {
                    ProviderCacheRead::Absent
                } else {
                    ProviderCacheRead::Ready {
                        token: ProviderCacheToken([4; 32]),
                        len: 0,
                        sha256: Sha256::digest([]).into(),
                    }
                };
                write_frame(
                    &mut peer,
                    &Response::ProviderCacheRead { snapshot },
                    REQUEST_TIMEOUT,
                )
                .await
                .unwrap();
                if !absent {
                    let _: Request = read_frame(&mut peer, REQUEST_TIMEOUT).await.unwrap();
                    write_frame(
                        &mut peer,
                        &Response::ProviderCacheChunk {
                            chunk: ProviderCacheChunk {
                                offset: 0,
                                bytes: vec![],
                                finished: true,
                            },
                        },
                        REQUEST_TIMEOUT,
                    )
                    .await
                    .unwrap();
                    let _: Request = read_frame(&mut peer, REQUEST_TIMEOUT).await.unwrap();
                    write_frame(&mut peer, &Response::Ok, REQUEST_TIMEOUT)
                        .await
                        .unwrap();
                }
            });
            assert_eq!(
                service
                    .read_complete_provider_cache(7, ProviderKind::Proxy, "proxy")
                    .await
                    .unwrap(),
                (!absent).then(Vec::new)
            );
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn complete_cache_read_rejects_bad_pages_without_returning_partial_bytes() {
        for chunk in [
            ProviderCacheChunk {
                offset: 1,
                bytes: vec![1],
                finished: true,
            },
            ProviderCacheChunk {
                offset: 0,
                bytes: vec![],
                finished: false,
            },
            ProviderCacheChunk {
                offset: 0,
                bytes: vec![1],
                finished: false,
            },
            ProviderCacheChunk {
                offset: 0,
                bytes: vec![1, 2],
                finished: true,
            },
        ] {
            let (service, mut peer) = cache_client();
            let server = tokio::spawn(async move {
                let _: Request = read_frame(&mut peer, REQUEST_TIMEOUT).await.unwrap();
                write_frame(
                    &mut peer,
                    &Response::ProviderCacheRead {
                        snapshot: ProviderCacheRead::Ready {
                            token: ProviderCacheToken([5; 32]),
                            len: 1,
                            sha256: Sha256::digest([1]).into(),
                        },
                    },
                    REQUEST_TIMEOUT,
                )
                .await
                .unwrap();
                let _: Request = read_frame(&mut peer, REQUEST_TIMEOUT).await.unwrap();
                write_frame(
                    &mut peer,
                    &Response::ProviderCacheChunk { chunk },
                    REQUEST_TIMEOUT,
                )
                .await
                .unwrap();
            });
            assert!(matches!(
                service
                    .read_complete_provider_cache(7, ProviderKind::Rule, "rules")
                    .await,
                Err(ServiceClientError::UnexpectedResponse)
            ));
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn complete_cache_read_never_publishes_a_digest_mismatch() {
        let (service, mut peer) = cache_client();
        let server = tokio::spawn(async move {
            let _: Request = read_frame(&mut peer, REQUEST_TIMEOUT).await.unwrap();
            write_frame(
                &mut peer,
                &Response::ProviderCacheRead {
                    snapshot: ProviderCacheRead::Ready {
                        token: ProviderCacheToken([6; 32]),
                        len: 1,
                        sha256: [0; 32],
                    },
                },
                REQUEST_TIMEOUT,
            )
            .await
            .unwrap();
            let _: Request = read_frame(&mut peer, REQUEST_TIMEOUT).await.unwrap();
            write_frame(
                &mut peer,
                &Response::ProviderCacheChunk {
                    chunk: ProviderCacheChunk {
                        offset: 0,
                        bytes: vec![1],
                        finished: true,
                    },
                },
                REQUEST_TIMEOUT,
            )
            .await
            .unwrap();
            let _: Request = read_frame(&mut peer, REQUEST_TIMEOUT).await.unwrap();
            write_frame(&mut peer, &Response::Ok, REQUEST_TIMEOUT)
                .await
                .unwrap();
        });
        assert!(matches!(
            service
                .read_complete_provider_cache(7, ProviderKind::Rule, "rules")
                .await,
            Err(ServiceClientError::UnexpectedResponse)
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn subscriptions_distinguish_clean_eof_from_truncated_frames() {
        let (client, mut server) = tokio::io::duplex(4096);
        write_frame(
            &mut server,
            &Response::Stream {
                data: serde_json::json!({"up":1}),
            },
            REQUEST_TIMEOUT,
        )
        .await
        .unwrap();
        drop(server);
        let mut subscription = ServiceSubscription {
            stream: Some(Box::new(client)),
        };
        assert_eq!(
            subscription.next().await.unwrap(),
            Some(serde_json::json!({"up":1}))
        );
        assert!(subscription.next().await.unwrap().is_none());
        let (client, mut server) = tokio::io::duplex(4096);
        tokio::io::AsyncWriteExt::write_all(&mut server, &[1])
            .await
            .unwrap();
        drop(server);
        let mut subscription = ServiceSubscription {
            stream: Some(Box::new(client)),
        };
        assert!(matches!(
            subscription.next().await,
            Err(ServiceClientError::Frame(FrameError::Io(_)))
        ));
    }
}
