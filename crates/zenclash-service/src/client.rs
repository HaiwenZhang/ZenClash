use std::io;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::sync::Mutex;
use tokio::task::AbortHandle;

use crate::protocol::{
    ApiRequest, ApiResponse, PreparedRuntimePatch, ProviderCacheChunk, ProviderCacheRead,
    ProviderCacheToken, ProviderKind, Request, Response, RuntimeStatus, ServiceErrorCode,
    SessionOperation, StreamKind,
};
use crate::{FrameError, PROTOCOL_VERSION, ProtocolInfo, SessionProof, read_frame, write_frame};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const MAX_ASSET_CHUNK: usize = 256 * 1024;

trait LocalIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> LocalIo for T {}
type Transport = Box<dyn LocalIo>;

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
    ///
    /// # Errors
    /// Returns closing-session, native identity, stream admission or transport errors.
    pub async fn subscribe(
        &self,
        kind: StreamKind,
    ) -> Result<ServiceSubscription, ServiceClientError> {
        if self.closing.load(Ordering::Acquire) {
            return Err(ServiceClientError::Closing);
        }
        let mut channel = self.channel.lock().await;
        if self.closing.load(Ordering::Acquire) {
            return Err(ServiceClientError::Closing);
        }
        let mut stream = open_verified().await?;
        let request = channel.next_request(SessionOperation::Subscribe { stream: kind })?;
        expect_ok(accepted(exchange(&mut stream, &request).await?)?)?;
        // Server admission is confirmed before other connections allocate a
        // later sequence, preventing cross-connection arrival races.
        drop(channel);
        Ok(ServiceSubscription {
            stream: Some(stream),
        })
    }
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

async fn hello(stream: &mut Transport) -> Result<ProtocolInfo, ServiceClientError> {
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
    stream: &mut Transport,
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

    fn channel(stream: tokio::io::DuplexStream) -> ClientChannel {
        ClientChannel {
            stream: Some(Box::new(stream)),
            proof: SessionProof::new(1, SessionToken([1; 32])),
            sequence: 0,
        }
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
