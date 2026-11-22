#![deny(clippy::undocumented_unsafe_blocks)]
// ZenClash native-controller adaptation, 2026-10-04. GPL-3.0-only; see NOTICE.md.
//! HTTP over one authenticated native controller connection per request.

use std::{io, path::PathBuf, time::Duration};

use http_body_util::{BodyExt, Full};
use hyper::{Request, body::Bytes};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 32 * 1024;
const MAX_REQUEST_BYTES: usize = 24 * 1024 * 1024 + 1024;
const MAX_DEADLINE: Duration = Duration::from_secs(12);

/// Mihomo HTTP and WebSocket transport over a local pipe or Unix socket.
/// The peer must match the PID of the owned kernel before credentials are sent.
pub struct NativeController {
    path: PathBuf,
    pid: u32,
    secret: String,
    #[cfg(unix)]
    expected_uid: libc::uid_t,
}

/// Status and bounded body returned by the native Mihomo controller.
pub struct NativeHttpResponse {
    /// HTTP status returned by the controller.
    pub status: u16,
    /// Response bytes after enforcing the transport body limit.
    pub body: Vec<u8>,
}

pub use crate::mihomo::errors::NativeHttpError;

impl NativeController {
    /// Creates a transport for a service-owned kernel without performing I/O.
    /// Unix service kernels must run as root; isolated service fixtures use the test owner.
    pub fn new(path: PathBuf, pid: u32, secret: String) -> Self {
        Self {
            path,
            pid,
            secret,
            #[cfg(all(unix, not(feature = "service-ipc-tests")))]
            expected_uid: 0,
            #[cfg(all(unix, feature = "service-ipc-tests"))]
            // SAFETY: geteuid has no arguments or memory safety requirements.
            expected_uid: unsafe { libc::geteuid() },
        }
    }

    /// Creates a transport for a child owned by the current application user.
    /// Peer verification still requires the exact owned PID before sending credentials.
    pub fn new_local(path: PathBuf, pid: u32, secret: String) -> Self {
        let controller = Self::new(path, pid, secret);
        #[cfg(unix)]
        let controller = Self {
            // SAFETY: geteuid has no arguments or memory safety requirements.
            expected_uid: unsafe { libc::geteuid() },
            ..controller
        };
        controller
    }

    /// Sends an HTTP request directly to the owned kernel.
    ///
    /// # Errors
    /// Reports peer verification, transport, deadline and body budget failures.
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<NativeHttpResponse, NativeHttpError> {
        let request = self.build_request(method, path, body)?;
        let mut request_sent = false;
        let result = tokio::time::timeout(timeout.min(MAX_DEADLINE), async {
            let stream = self.connect().await.map_err(NativeHttpError::BeforeSend)?;
            exchange(stream, request, &mut request_sent).await
        })
        .await;
        result.unwrap_or_else(|_| Err(deadline_error(request_sent)))
    }

    fn build_request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Request<Full<Bytes>>, NativeHttpError> {
        // The GUI uses ordinary Mihomo HTTP controls; the service only owns core lifecycle.
        if !path.starts_with('/') || path.starts_with("//") || path.len() > MAX_HEADER_BYTES {
            return Err(NativeHttpError::BeforeSend(io::Error::new(
                io::ErrorKind::InvalidInput,
                "controller request requires a bounded origin-form path",
            )));
        }
        if self.secret.len() > 1024 {
            return Err(NativeHttpError::BudgetExceeded {
                request_sent: false,
            });
        }
        let mut bytes = BoundedBody(Vec::new());
        if let Some(body) = body {
            // A bounded writer accommodates JSON escaping without first making
            // an unbounded serialized copy of the service-generated YAML.
            serde_json::to_writer(&mut bytes, body).map_err(|_| {
                NativeHttpError::BudgetExceeded {
                    request_sent: false,
                }
            })?;
        }
        Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::HOST, "localhost")
            .header(
                hyper::header::AUTHORIZATION,
                format!("Bearer {}", self.secret),
            )
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .header(hyper::header::CONNECTION, "close")
            .body(Full::new(Bytes::from(bytes.0)))
            .map_err(|_| {
                NativeHttpError::BeforeSend(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid private controller HTTP request",
                ))
            })
    }

    #[cfg(windows)]
    async fn connect(&self) -> io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;
        // Mihomo publishes the next instance after accepting the previous one.
        // Concurrent HTTP and WebSocket opens can observe that short busy interval.
        let stream = loop {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(&self.path) {
                Ok(stream) => break stream,
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error),
            }
        };
        let mut pid = 0;
        // SAFETY: The named-pipe handle is live and `pid` is writable storage.
        if unsafe {
            windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId(
                stream.as_raw_handle(),
                &mut pid,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if pid != self.pid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "kernel controller identity changed",
            ));
        }
        Ok(stream)
    }

    #[cfg(unix)]
    async fn connect(&self) -> io::Result<tokio::net::UnixStream> {
        let stream = tokio::net::UnixStream::connect(&self.path).await?;
        verify_kernel_peer(&stream, self.pid, self.expected_uid)?;
        Ok(stream)
    }
}

struct BoundedBody(Vec<u8>);

impl io::Write for BoundedBody {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_REQUEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "controller request exceeds byte budget",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Driver(tokio::task::JoinHandle<()>);

impl Drop for Driver {
    fn drop(&mut self) {
        // The driver owns the authenticated handle. Cancelling the caller or
        // reaching either byte/deadline limit must also close that connection.
        self.0.abort();
    }
}

fn deadline_error(request_sent: bool) -> NativeHttpError {
    let error = io::Error::new(io::ErrorKind::TimedOut, "controller request timed out");
    if request_sent {
        NativeHttpError::OutcomeUnknown(error)
    } else {
        NativeHttpError::BeforeSend(error)
    }
}

fn response_error(error: hyper::Error) -> NativeHttpError {
    // Client-only Hyper bounds response parsing but exposes no stable precise
    // header-overflow classification. Preserve the mutation uncertainty instead
    // of matching private error strings or claiming a definite rejection.
    NativeHttpError::OutcomeUnknown(io::Error::other(error))
}

fn headers_within_budget(headers: &hyper::HeaderMap) -> bool {
    headers
        .iter()
        .try_fold(0_usize, |total, (name, value)| {
            total
                .checked_add(name.as_str().len() + value.as_bytes().len() + 4)
                .filter(|total| *total <= MAX_HEADER_BYTES)
        })
        .is_some()
}

async fn exchange<T>(
    io: T,
    request: Request<Full<Bytes>>,
    request_sent: &mut bool,
) -> Result<NativeHttpResponse, NativeHttpError>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use hyper::body::Body;
    let (mut sender, connection) = hyper::client::conn::http1::Builder::new()
        .max_buf_size(MAX_HEADER_BYTES)
        .max_headers(128)
        .handshake(TokioIo::new(io))
        .await
        .map_err(|error| NativeHttpError::BeforeSend(io::Error::other(error)))?;
    let _driver = Driver(tokio::spawn(async move {
        let _ = connection.await;
    }));
    // Hyper may write immediately when this future is polled. Any later error
    // must be reported as unknown rather than a definitely rejected mutation.
    *request_sent = true;
    let response = sender.send_request(request).await.map_err(response_error)?;
    let status = response.status().as_u16();
    if !headers_within_budget(response.headers())
        || response
            .body()
            .size_hint()
            .upper()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
    {
        return Err(NativeHttpError::BudgetExceeded { request_sent: true });
    }
    let mut body = response.into_body();
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(response_error)?;
        if let Some(data) = frame.data_ref() {
            if bytes.len().saturating_add(data.len()) > MAX_RESPONSE_BYTES {
                return Err(NativeHttpError::BudgetExceeded { request_sent: true });
            }
            bytes.extend_from_slice(data);
        }
        if let Some(trailers) = frame.trailers_ref()
            && !headers_within_budget(trailers)
        {
            return Err(NativeHttpError::BudgetExceeded { request_sent: true });
        }
    }
    Ok(NativeHttpResponse {
        status,
        body: bytes,
    })
}

/// Controller WebSocket carried by an authenticated local native connection.
#[cfg(windows)]
pub type NativeSocket =
    tokio_tungstenite::WebSocketStream<tokio::net::windows::named_pipe::NamedPipeClient>;
/// Controller WebSocket carried by an authenticated local native connection.
#[cfg(unix)]
pub type NativeSocket = tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>;

impl NativeController {
    /// Opens a WebSocket directly on the owned kernel's local controller.
    ///
    /// # Errors
    /// Reports peer verification, transport, deadline and handshake failures.
    pub async fn websocket(&self, path: &str) -> Result<NativeSocket, NativeHttpError> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
        // Reuse HTTP origin-form checks and the same secret/header byte limits.
        let http = self.build_request("GET", path, None)?;
        let mut request = format!("ws://localhost{path}")
            .into_client_request()
            .map_err(|error| NativeHttpError::BeforeSend(io::Error::other(error)))?;
        request.headers_mut().insert(
            hyper::header::AUTHORIZATION,
            http.headers()[hyper::header::AUTHORIZATION].clone(),
        );
        let max_bytes = if path.split('?').next() == Some("/logs") {
            128 * 1024
        } else {
            MAX_RESPONSE_BYTES
        };
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(max_bytes))
            .max_frame_size(Some(max_bytes));
        tokio::time::timeout(MAX_DEADLINE, async {
            let stream = self.connect().await.map_err(NativeHttpError::BeforeSend)?;
            tokio_tungstenite::client_async_with_config(request, stream, Some(config))
                .await
                .map(|(socket, _)| socket)
                .map_err(|error| NativeHttpError::BeforeSend(io::Error::other(error)))
        })
        .await
        .unwrap_or_else(|_| Err(deadline_error(false)))
    }
}

#[cfg(unix)]
fn verify_kernel_peer(
    stream: &tokio::net::UnixStream,
    expected_pid: u32,
    expected_uid: libc::uid_t,
) -> io::Result<()> {
    let credentials = stream.peer_cred()?;
    #[cfg(target_os = "linux")]
    let pid = credentials.pid();
    #[cfg(target_os = "macos")]
    let pid = {
        use std::os::fd::AsRawFd as _;
        let mut pid: libc::pid_t = 0;
        let mut len = std::mem::size_of_val(&pid) as libc::socklen_t;
        // SOL_LOCAL=0 and LOCAL_PEERPID are Darwin's documented socket option ABI.
        // SAFETY: a live socket and appropriately sized PID storage are supplied.
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                0,
                libc::LOCAL_PEERPID,
                (&mut pid as *mut libc::pid_t).cast(),
                &mut len,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if len as usize != std::mem::size_of_val(&pid) || pid <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "socket peer PID unavailable",
            ));
        }
        Some(pid)
    };
    if credentials.uid() != expected_uid
        || pid.and_then(|pid| u32::try_from(pid).ok()) != Some(expected_pid)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "kernel controller peer identity changed",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
