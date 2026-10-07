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
}

pub struct NativeHttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum NativeHttpError {
    #[error("controller request failed before sending: {0}")]
    BeforeSend(#[source] io::Error),
    #[error("controller request outcome is unknown: {0}")]
    OutcomeUnknown(#[source] io::Error),
    #[error("controller request exceeded its byte budget")]
    BudgetExceeded { request_sent: bool },
}

impl NativeController {
    /// Creates a transport for one owned kernel process without performing I/O.
    pub fn new(path: PathBuf, pid: u32, secret: String) -> Self {
        Self { path, pid, secret }
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
        verify_kernel_peer(&stream, self.pid)?;
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
fn verify_kernel_peer(stream: &tokio::net::UnixStream, expected_pid: u32) -> io::Result<()> {
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
    #[cfg(any(test, feature = "ipc-tests"))]
    // SAFETY: geteuid has no arguments or memory safety requirements.
    let expected_uid = unsafe { libc::geteuid() };
    #[cfg(not(any(test, feature = "ipc-tests")))]
    let expected_uid = 0;
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
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn request() -> Request<Full<Bytes>> {
        Request::builder()
            .method("PUT")
            .uri("/configs?force=false")
            .header("host", "localhost")
            .header("authorization", "Bearer private-secret")
            .body(Full::new(Bytes::from_static(b"private-body")))
            .unwrap()
    }

    async fn receive_request<T: AsyncRead + Unpin>(stream: &mut T) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let mut byte = [0];
            stream.read_exact(&mut byte).await.unwrap();
            bytes.push(byte[0]);
            if bytes.ends_with(b"\r\n\r\n") {
                let length = std::str::from_utf8(&bytes)
                    .unwrap()
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .map(|(_, length)| length.trim().parse::<usize>().unwrap())
                    .unwrap_or(0);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).await.unwrap();
                bytes.extend_from_slice(&body);
                return bytes;
            }
        }
    }

    #[tokio::test]
    async fn chunked_responses_preserve_non_success_status_and_body() {
        let (client, mut server) = tokio::io::duplex(4096);
        let fixture = tokio::spawn(async move {
            let bytes = receive_request(&mut server).await;
            assert!(bytes.windows(14).any(|part| part == b"private-secret"));
            server.write_all(b"HTTP/1.1 409 Conflict\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\ntest\r\n0\r\n\r\n").await.unwrap();
        });
        let response = exchange(client, request(), &mut false).await.unwrap();
        assert_eq!(response.status, 409);
        assert_eq!(response.body, b"test");
        fixture.await.unwrap();
    }

    #[tokio::test]
    async fn chunked_body_cannot_bypass_the_response_byte_limit() {
        let (client, mut server) = tokio::io::duplex(4096);
        let fixture = tokio::spawn(async move {
            receive_request(&mut server).await;
            server
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            let chunk = vec![b'x'; 32 * 1024];
            for _ in 0..=MAX_RESPONSE_BYTES / chunk.len() {
                if server.write_all(b"8000\r\n").await.is_err()
                    || server.write_all(&chunk).await.is_err()
                    || server.write_all(b"\r\n").await.is_err()
                {
                    return;
                }
            }
            let _ = server.write_all(b"0\r\n\r\n").await;
        });
        assert!(matches!(
            exchange(client, request(), &mut false).await,
            Err(NativeHttpError::BudgetExceeded { request_sent: true })
        ));
        fixture.await.unwrap();
    }

    #[tokio::test]
    async fn response_headers_cannot_exhaust_the_body_budget_first() {
        let (client, mut server) = tokio::io::duplex(4096);
        let fixture = tokio::spawn(async move {
            receive_request(&mut server).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nX-Huge: {}\r\nContent-Length: 0\r\n\r\n",
                "x".repeat(MAX_HEADER_BYTES)
            );
            let _ = server.write_all(response.as_bytes()).await;
            let mut tail = Vec::new();
            tokio::time::timeout(Duration::from_secs(1), server.read_to_end(&mut tail))
                .await
                .unwrap()
                .unwrap();
            assert!(tail.is_empty());
        });
        assert!(matches!(
            exchange(client, request(), &mut false).await,
            Err(NativeHttpError::BudgetExceeded { request_sent: true })
                | Err(NativeHttpError::OutcomeUnknown(_))
        ));
        fixture.await.unwrap();
    }

    #[tokio::test]
    async fn oversized_response_is_unknown_after_a_mutating_request_was_sent() {
        let (client, mut server) = tokio::io::duplex(4096);
        let fixture = tokio::spawn(async move {
            receive_request(&mut server).await;
            server
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                        MAX_RESPONSE_BYTES + 1
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let result = exchange(client, request(), &mut false).await;
        assert!(matches!(
            result,
            Err(NativeHttpError::BudgetExceeded { request_sent: true })
        ));
        fixture.await.unwrap();
    }

    #[tokio::test]
    async fn malformed_responses_do_not_report_definitive_mutation_failure() {
        let (client, mut server) = tokio::io::duplex(4096);
        let fixture = tokio::spawn(async move {
            receive_request(&mut server).await;
            server
                .write_all(b"not an HTTP response\r\n\r\n")
                .await
                .unwrap();
        });
        assert!(matches!(
            exchange(client, request(), &mut false).await,
            Err(NativeHttpError::OutcomeUnknown(_))
        ));
        fixture.await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_an_exchange_closes_its_driver_and_native_stream() {
        let (client, mut server) = tokio::io::duplex(4096);
        let exchange_task =
            tokio::spawn(async move { exchange(client, request(), &mut false).await });
        receive_request(&mut server).await;
        exchange_task.abort();
        let _ = exchange_task.await;
        let mut tail = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), server.read_to_end(&mut tail))
            .await
            .unwrap()
            .unwrap();
        assert!(tail.is_empty());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_busy_pipe_wait_is_cancelled_by_the_request_deadline() {
        use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
        let name = format!(r"\\.\pipe\ZenClash.Kernel.Busy.{}", std::process::id());
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .max_instances(1)
            .create(&name)
            .unwrap();
        let _occupied = ClientOptions::new().open(&name).unwrap();
        server.connect().await.unwrap();
        let controller =
            NativeController::new(name.into(), std::process::id(), "private-secret".into());
        let result = controller
            .request("GET", "/version", None, Duration::from_millis(40))
            .await;
        assert!(matches!(result, Err(NativeHttpError::BeforeSend(ref error))
            if error.kind() == io::ErrorKind::TimedOut));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn an_actual_pipe_with_the_wrong_pid_receives_neither_body_nor_secret() {
        use tokio::net::windows::named_pipe::ServerOptions;
        let name = format!(r"\\.\pipe\ZenClash.Kernel.Impostor.{}", std::process::id());
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let fixture = tokio::spawn(async move {
            server.connect().await.unwrap();
            let mut bytes = Vec::new();
            let result = server.read_to_end(&mut bytes).await;
            if let Err(error) = result {
                assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
            }
            bytes
        });
        let controller = NativeController::new(
            name.into(),
            std::process::id().wrapping_add(1),
            "private-secret".into(),
        );
        let result = controller
            .request(
                "PUT",
                "/configs?force=false",
                Some(&serde_json::json!({"payload":"private-body"})),
                Duration::from_secs(1),
            )
            .await;
        assert!(
            matches!(result, Err(NativeHttpError::BeforeSend(ref error)) if error.kind() == io::ErrorKind::PermissionDenied)
        );
        assert!(fixture.await.unwrap().is_empty());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_verified_native_pipe_supports_json_escaping_and_deadline_cleanup() {
        use tokio::net::windows::named_pipe::ServerOptions;
        let name = format!(
            r"\\.\pipe\ZenClash.Kernel.Authenticated.{}",
            std::process::id()
        );
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let fixture = tokio::spawn(async move {
            server.connect().await.unwrap();
            let bytes = receive_request(&mut server).await;
            assert!(bytes.len() > 4 * 1024 * 1024);
            let mut tail = Vec::new();
            let result = server.read_to_end(&mut tail).await;
            if let Err(error) = result {
                assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
            }
            assert!(tail.is_empty());
        });
        let controller =
            NativeController::new(name.into(), std::process::id(), "private-secret".into());
        let body = serde_json::json!({"payload":"\"".repeat(3 * 1024 * 1024)});
        let result = controller
            .request(
                "PUT",
                "/configs?force=false",
                Some(&body),
                Duration::from_secs(2),
            )
            .await;
        assert!(
            matches!(result, Err(NativeHttpError::OutcomeUnknown(ref error)) if error.kind() == io::ErrorKind::TimedOut)
        );
        tokio::time::timeout(Duration::from_secs(1), fixture)
            .await
            .unwrap()
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_actual_socket_with_the_wrong_pid_receives_neither_body_nor_secret() {
        let directory =
            std::env::temp_dir().join(format!("zenclash-controller-peer-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("controller.sock");
        let server = tokio::net::UnixListener::bind(&path).unwrap();
        let fixture = tokio::spawn(async move {
            let (mut stream, _) = server.accept().await.unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.unwrap();
            bytes
        });
        let controller = NativeController::new(
            path.clone(),
            std::process::id().wrapping_add(1),
            "private-secret".into(),
        );
        let result = controller
            .request(
                "PUT",
                "/configs?force=false",
                Some(&serde_json::json!({"payload":"private-body"})),
                Duration::from_secs(1),
            )
            .await;
        assert!(
            matches!(result, Err(NativeHttpError::BeforeSend(ref error)) if error.kind() == io::ErrorKind::PermissionDenied)
        );
        assert!(fixture.await.unwrap().is_empty());
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
