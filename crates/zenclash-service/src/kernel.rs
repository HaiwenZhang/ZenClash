use std::{
    collections::VecDeque,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::kernel_transport::{NativeController, NativeHttpError};
use crate::protocol::{ApiRequest, ApiResponse, RuntimeStatus, ServiceErrorCode, StreamKind};
use futures_util::StreamExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{client::IntoClientRequest, protocol::WebSocketConfig},
};

const MAX_LOG_LINES: usize = 1024;
const MAX_LOG_BYTES: usize = 1024 * 1024;
const MAX_LOG_LINE: usize = 4096;
const MAX_API_BYTES: usize = 4 * 1024 * 1024;

#[derive(Default)]
struct LogBuffer {
    lines: VecDeque<(u64, String)>,
    cursor: u64,
    bytes: usize,
}

impl LogBuffer {
    fn append(&mut self, bytes: &[u8], secret: &str) {
        let line = if secret.is_empty() {
            String::from_utf8_lossy(bytes).into_owned()
        } else {
            String::from_utf8_lossy(bytes).replace(secret, "[redacted]")
        };
        self.cursor = self.cursor.saturating_add(1);
        self.bytes += line.len();
        self.lines.push_back((self.cursor, line));
        while self.lines.len() > MAX_LOG_LINES || self.bytes > MAX_LOG_BYTES {
            if let Some((_, line)) = self.lines.pop_front() {
                self.bytes -= line.len();
            }
        }
    }
}

#[cfg(windows)]
type Child = crate::platform::NativeChild;
#[cfg(unix)]
type Child = tokio::process::Child;

pub(crate) struct Kernel {
    child: Option<Child>,
    pid: u32,
    client: NativeController,
    secret: String,
    controller: PathBuf,
    logs: Arc<Mutex<LogBuffer>>,
    readers: Vec<tokio::task::JoinHandle<()>>,
    exit_reason: Option<String>,
    #[cfg(test)]
    fixture_status: Option<RuntimeStatus>,
}

impl Kernel {
    pub(crate) async fn start(
        binary: &Path,
        config: &Path,
        home: &Path,
        controller: PathBuf,
        secret: String,
    ) -> io::Result<Self> {
        #[cfg(windows)]
        let mut child = crate::platform::NativeChild::spawn(binary, config, home)?;
        #[cfg(unix)]
        let mut child = crate::platform::spawn_core(binary, config, home)?;
        #[cfg(windows)]
        let pid = child.pid();
        #[cfg(unix)]
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("kernel has no process identifier"))?;
        let client = NativeController::new(controller.clone(), pid, secret.clone());
        let logs = Arc::new(Mutex::new(LogBuffer::default()));
        let mut readers = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            readers.push(capture(stdout, logs.clone(), secret.clone()));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(capture(stderr, logs.clone(), secret.clone()));
        }
        let mut kernel = Self {
            child: Some(child),
            pid,
            client,
            secret,
            controller,
            logs,
            readers,
            exit_reason: None,
            #[cfg(test)]
            fixture_status: None,
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        loop {
            if !kernel.snapshot()?.running {
                kernel.stop().await?;
                return Err(io::Error::other("approved kernel exited before readiness"));
            }
            let readiness = kernel
                .client
                .request("GET", "/version", None, Duration::from_millis(500))
                .await;
            if readiness.is_ok_and(|response| (200..300).contains(&response.status)) {
                return Ok(kernel);
            }
            if tokio::time::Instant::now() >= deadline {
                kernel.stop().await?;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "kernel controller did not become ready",
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub(crate) fn snapshot(&mut self) -> io::Result<RuntimeStatus> {
        #[cfg(test)]
        if let Some(status) = &self.fixture_status {
            return Ok(status.clone());
        }
        if let Some(child) = &mut self.child
            && let Some(status) = child.try_wait()?
        {
            self.exit_reason = Some(format!("kernel exited: {status}"));
            self.child = None;
        }
        Ok(RuntimeStatus {
            candidate: None,
            applied_revision: None,
            committed_revision: None,
            running: self.child.is_some(),
            pid: self.child.as_ref().map(|_| self.pid),
            exit_reason: self.exit_reason.clone(),
        })
    }

    pub(crate) async fn stop(&mut self) -> io::Result<()> {
        #[cfg(test)]
        if let Some(status) = &mut self.fixture_status {
            status.running = false;
            status.pid = None;
            return Ok(());
        }
        if self.snapshot()?.running {
            // Ask the kernel to release privileged capture before terminating it.
            // A failed controller remains bounded by the native process backstop.
            let _ = self.client.request("PATCH", "/configs", Some(&serde_json::json!({"tun": {"enable": false}, "redir-port": 0, "tproxy-port": 0})), Duration::from_secs(2)).await;
        }
        if let Some(child) = self.child.take() {
            #[cfg(windows)]
            {
                let (child, result) = tokio::task::spawn_blocking(move || {
                    let result = (|| {
                        if child.try_wait()?.is_none() {
                            child.kill()?;
                        }
                        child.wait().map(|_| ())
                    })();
                    (child, result)
                })
                .await
                .map_err(io::Error::other)?;
                if let Err(error) = result {
                    self.child = Some(child);
                    return Err(error);
                }
            }
            #[cfg(unix)]
            {
                let mut child = child;
                let result = async {
                    if child.try_wait()?.is_none() {
                        // The unreaped child identity cannot be reused. Mihomo's
                        // SIGTERM path runs listener and route cleanup before exit.
                        if unsafe { libc::kill(self.pid as i32, libc::SIGTERM) } != 0 {
                            let error = io::Error::last_os_error();
                            if error.raw_os_error() != Some(libc::ESRCH) {
                                return Err(error);
                            }
                        }
                    }
                    match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
                        Ok(result) => {
                            result?;
                        }
                        Err(_) => {
                            child.start_kill()?;
                            tokio::time::timeout(Duration::from_secs(5), child.wait()).await??;
                        }
                    }
                    Ok::<(), io::Error>(())
                }
                .await;
                if let Err(error) = result {
                    self.child = Some(child);
                    return Err(error);
                }
            }
        }
        for task in self.readers.drain(..) {
            let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
        }
        #[cfg(unix)]
        if let Ok(metadata) = std::fs::symlink_metadata(&self.controller) {
            use std::os::unix::fs::FileTypeExt;
            if metadata.file_type().is_socket() {
                std::fs::remove_file(&self.controller)?;
            }
        }
        Ok(())
    }

    pub(crate) fn logs(&self, cursor: u64) -> (u64, Vec<String>) {
        let logs = self.logs.lock().unwrap_or_else(|error| error.into_inner());
        (
            logs.cursor,
            logs.lines
                .iter()
                .filter(|(id, _)| *id > cursor)
                .map(|(_, line)| {
                    line.replace(
                        self.controller.to_string_lossy().as_ref(),
                        "[private-controller]",
                    )
                })
                .collect(),
        )
    }

    pub(crate) async fn validate(binary: &Path, config: &Path, home: &Path) -> io::Result<()> {
        #[cfg(windows)]
        let mut child = crate::platform::NativeChild::spawn_validation(binary, config, home)?;
        #[cfg(unix)]
        let mut child = crate::platform::spawn_validation(binary, config, home)?;
        // Drain bounded output while the real validation process runs. Keeping
        // the pipes unread could deadlock before the completion deadline.
        let logs = Arc::new(Mutex::new(LogBuffer::default()));
        let mut readers = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            readers.push(capture(stdout, logs.clone(), String::new()));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(capture(stderr, logs, String::new()));
        }
        #[cfg(windows)]
        let result = tokio::task::spawn_blocking(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(code) = child.try_wait()? {
                    return if code == 0 {
                        Ok(())
                    } else {
                        Err(io::Error::other("approved kernel rejected configuration"))
                    };
                }
                if std::time::Instant::now() >= deadline {
                    child.kill()?;
                    child.wait()?;
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "configuration validation timed out",
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })
        .await
        .map_err(io::Error::other)?;
        #[cfg(unix)]
        let result = match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
            Ok(Ok(status)) if status.success() => Ok(()),
            Ok(Ok(_)) => Err(io::Error::other("approved kernel rejected configuration")),
            Ok(Err(error)) => Err(error),
            Err(_) => {
                child.start_kill()?;
                tokio::time::timeout(Duration::from_secs(5), child.wait()).await??;
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "configuration validation timed out",
                ))
            }
        };
        for reader in readers {
            let _ = tokio::time::timeout(Duration::from_secs(1), reader).await;
        }
        result
    }

    pub(crate) async fn reload(
        &self,
        runtime: &crate::runtime::StagedRuntime,
        force: bool,
    ) -> Result<(), ServiceErrorCode> {
        let path = runtime
            .materialize(
                self.controller.to_str().ok_or(ServiceErrorCode::Internal)?,
                &self.secret,
            )
            .map_err(|error| error.code())?;
        let body = reload_body(&path)?;
        let response = self
            .client
            .request(
                "PUT",
                &format!("/configs?force={force}"),
                Some(&body),
                Duration::from_secs(12),
            )
            .await
            .map_err(|error| controller_error(error, true))?;
        if !(200..300).contains(&response.status) {
            return Err(ServiceErrorCode::OutcomeUnknown);
        }
        Ok(())
    }

    pub(crate) async fn subscribe(&self, stream: StreamKind) -> io::Result<KernelSubscription> {
        let path = match stream {
            StreamKind::Logs => "/logs?level=debug",
            StreamKind::Traffic => "/traffic",
            StreamKind::Connections => "/connections",
            StreamKind::Memory => "/memory",
        };
        let mut request = format!("ws://localhost{path}")
            .into_client_request()
            .map_err(io::Error::other)?;
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", self.secret)
                .parse()
                .map_err(io::Error::other)?,
        );
        #[cfg(windows)]
        let transport =
            tokio::net::windows::named_pipe::ClientOptions::new().open(&self.controller)?;
        #[cfg(windows)]
        verify_kernel_pipe(&transport, self.pid)?;
        #[cfg(unix)]
        let transport = tokio::net::UnixStream::connect(&self.controller).await?;
        #[cfg(unix)]
        crate::platform::verify_kernel_peer(&transport, self.pid)?;
        let transport: Box<dyn KernelIo> = Box::new(transport);
        let config = WebSocketConfig {
            max_message_size: Some(MAX_API_BYTES),
            max_frame_size: Some(MAX_API_BYTES),
            max_write_buffer_size: MAX_API_BYTES,
            ..WebSocketConfig::default()
        };
        let (socket, _) = tokio::time::timeout(
            Duration::from_secs(5),
            tokio_tungstenite::client_async_with_config(request, transport, Some(config)),
        )
        .await?
        .map_err(io::Error::other)?;
        Ok(KernelSubscription {
            socket,
            secret: self.secret.clone(),
            controller: self.controller.to_string_lossy().into_owned(),
            pid: self.pid,
        })
    }

    pub(crate) async fn api(&self, request: &ApiRequest) -> Result<ApiResponse, ServiceErrorCode> {
        crate::api::validate_request(request)?;
        let mutating = request.method != "GET";
        let response = self
            .client
            .request(
                &request.method,
                &request.path,
                request.body.as_ref(),
                Duration::from_secs(12),
            )
            .await
            .map_err(|error| controller_error(error, mutating))?;
        let body = if response.body.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&response.body).map_err(|_| {
                if mutating {
                    ServiceErrorCode::OutcomeUnknown
                } else {
                    ServiceErrorCode::KernelFailed
                }
            })?
        };
        Ok(ApiResponse {
            status: response.status,
            body: crate::api::redact_response(&request.path, body),
        })
    }

    pub(crate) async fn runtime_config(&self) -> Result<serde_json::Value, ServiceErrorCode> {
        let response = self
            .client
            .request("GET", "/configs", None, Duration::from_secs(5))
            .await
            .map_err(|error| controller_error(error, false))?;
        if !(200..300).contains(&response.status) {
            return Err(ServiceErrorCode::KernelFailed);
        }
        serde_json::from_slice(&response.body).map_err(|_| ServiceErrorCode::KernelFailed)
    }

    pub(crate) async fn patch_runtime(
        &self,
        patch: &serde_json::Value,
    ) -> Result<(), ServiceErrorCode> {
        crate::api::canonical_kernel_runtime_patch(patch)?;
        let response = self
            .client
            .request("PATCH", "/configs", Some(patch), Duration::from_secs(5))
            .await
            .map_err(|error| controller_error(error, true))?;
        if !(200..300).contains(&response.status) {
            return Err(ServiceErrorCode::OutcomeUnknown);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn fixture(streams: Vec<tokio::io::DuplexStream>, pid: u32) -> Self {
        Self {
            child: None,
            pid,
            client: NativeController::fixture(streams, pid),
            secret: "fixture-secret".into(),
            controller: PathBuf::new(),
            logs: Arc::new(Mutex::new(LogBuffer::default())),
            readers: Vec::new(),
            exit_reason: None,
            fixture_status: Some(RuntimeStatus {
                candidate: None,
                applied_revision: None,
                committed_revision: None,
                running: true,
                pid: Some(pid),
                exit_reason: None,
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn replace_fixture_pid(&mut self, pid: u32) {
        self.fixture_status.as_mut().unwrap().pid = Some(pid);
    }
}

fn controller_error(error: NativeHttpError, mutating: bool) -> ServiceErrorCode {
    match error {
        NativeHttpError::BeforeSend(_) => ServiceErrorCode::KernelUnavailable,
        NativeHttpError::OutcomeUnknown(_) if mutating => ServiceErrorCode::OutcomeUnknown,
        NativeHttpError::OutcomeUnknown(_) => ServiceErrorCode::KernelUnavailable,
        NativeHttpError::BudgetExceeded { request_sent: true } if mutating => {
            ServiceErrorCode::OutcomeUnknown
        }
        NativeHttpError::BudgetExceeded { .. } => ServiceErrorCode::BudgetExceeded,
    }
}

fn reload_body(path: &Path) -> Result<serde_json::Value, ServiceErrorCode> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| ServiceErrorCode::Internal)?
        .take(crate::runtime::MAX_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ServiceErrorCode::Internal)?;
    if bytes.len() > crate::runtime::MAX_CONFIG_BYTES {
        return Err(ServiceErrorCode::BudgetExceeded);
    }
    let payload = String::from_utf8(bytes).map_err(|_| ServiceErrorCode::Internal)?;
    // Mihomo's path reload is subject to IsSafePath. Private configuration must
    // stay outside resource roots, so send only this service-generated payload.
    Ok(serde_json::json!({"payload": payload}))
}

#[cfg(windows)]
fn verify_kernel_pipe(
    transport: &tokio::net::windows::named_pipe::NamedPipeClient,
    expected_pid: u32,
) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    let mut pid = 0;
    if unsafe {
        windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId(
            transport.as_raw_handle(),
            &mut pid,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if pid != expected_pid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "kernel controller identity changed",
        ));
    }
    Ok(())
}

trait KernelIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> KernelIo for T {}

pub(crate) struct KernelSubscription {
    socket: WebSocketStream<Box<dyn KernelIo>>,
    secret: String,
    controller: String,
    pub(crate) pid: u32,
}

impl KernelSubscription {
    pub(crate) async fn next(&mut self) -> io::Result<Option<serde_json::Value>> {
        loop {
            let Some(message) = self.socket.next().await else {
                return Ok(None);
            };
            let message = message.map_err(io::Error::other)?;
            let bytes = match message {
                tokio_tungstenite::tungstenite::Message::Text(text) => text.into_bytes(),
                tokio_tungstenite::tungstenite::Message::Binary(bytes) => bytes,
                tokio_tungstenite::tungstenite::Message::Close(_) => return Ok(None),
                _ => continue,
            };
            let mut value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            scrub_stream_strings(&mut value, &self.secret, &self.controller);
            return Ok(Some(crate::api::redact_response("", value)));
        }
    }
}

fn scrub_stream_strings(value: &mut serde_json::Value, secret: &str, controller: &str) {
    match value {
        serde_json::Value::String(text) => {
            *text = text
                .replace(secret, "[redacted]")
                .replace(controller, "[private-controller]");
        }
        serde_json::Value::Array(values) => {
            for value in values {
                scrub_stream_strings(value, secret, controller);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                scrub_stream_strings(value, secret, controller);
            }
        }
        _ => {}
    }
}

fn consume_chunk(
    chunk: &[u8],
    pending: &mut Vec<u8>,
    dropping: &mut bool,
    logs: &Arc<Mutex<LogBuffer>>,
    secret: &str,
) {
    for byte in chunk {
        if *dropping {
            if *byte == b'\n' {
                *dropping = false;
            }
            continue;
        }
        if *byte == b'\n' {
            logs.lock()
                .unwrap_or_else(|error| error.into_inner())
                .append(pending, secret);
            pending.clear();
        } else if pending.len() == MAX_LOG_LINE {
            // Never emit a split secret. Drop the entire oversized line rather
            // than exposing fragments which could be concatenated by a reader.
            pending.clear();
            *dropping = true;
            logs.lock()
                .unwrap_or_else(|error| error.into_inner())
                .append(b"[oversized kernel log line omitted]", secret);
        } else {
            pending.push(*byte);
        }
    }
}

#[cfg(windows)]
fn capture(
    mut output: std::fs::File,
    logs: Arc<Mutex<LogBuffer>>,
    secret: String,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut pending = Vec::with_capacity(MAX_LOG_LINE);
        let mut dropping = false;
        let mut chunk = [0_u8; MAX_LOG_LINE];
        while let Ok(size) = output.read(&mut chunk) {
            if size == 0 {
                break;
            }
            consume_chunk(&chunk[..size], &mut pending, &mut dropping, &logs, &secret);
        }
        if !pending.is_empty() {
            logs.lock()
                .unwrap_or_else(|error| error.into_inner())
                .append(&pending, &secret);
        }
    })
}

#[cfg(unix)]
fn capture<T: tokio::io::AsyncRead + Unpin + Send + 'static>(
    mut output: T,
    logs: Arc<Mutex<LogBuffer>>,
    secret: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut pending = Vec::with_capacity(MAX_LOG_LINE);
        let mut dropping = false;
        let mut chunk = [0_u8; MAX_LOG_LINE];
        while let Ok(size) = output.read(&mut chunk).await {
            if size == 0 {
                break;
            }
            consume_chunk(&chunk[..size], &mut pending, &mut dropping, &logs, &secret);
        }
        if !pending.is_empty() {
            logs.lock()
                .unwrap_or_else(|error| error.into_inner())
                .append(&pending, &secret);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_configuration_reload_uses_payload_without_expanding_safe_paths() {
        let file =
            std::env::temp_dir().join(format!("zenclash-reload-{}.yaml", std::process::id()));
        std::fs::write(&file, "secret: private-controller-token\nmode: rule\n").unwrap();
        let body = reload_body(&file).unwrap();
        std::fs::remove_file(&file).unwrap();
        assert_eq!(
            body["payload"],
            "secret: private-controller-token\nmode: rule\n"
        );
        assert!(body.get("path").is_none());
    }

    #[tokio::test]
    async fn websocket_events_remove_controller_secrets_before_forwarding() {
        use futures_util::SinkExt;
        use tokio_tungstenite::tungstenite::{Message, protocol::Role};
        let (client, server) = tokio::io::duplex(4096);
        let mut server = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
        let transport: Box<dyn KernelIo> = Box::new(client);
        let socket = WebSocketStream::from_raw_socket(transport, Role::Client, None).await;
        let mut events = KernelSubscription {
            socket,
            secret: "private-token".into(),
            controller: "private-address".into(),
            pid: 12,
        };
        server.send(Message::Text(serde_json::json!({"payload": ["private-token at private-address"], "secret": "private-token"}).to_string())).await.unwrap();
        let event = events.next().await.unwrap().unwrap();
        assert_eq!(event["payload"][0], "[redacted] at [private-controller]");
        assert!(event.get("secret").is_none());
    }
    #[test]
    fn child_output_without_newlines_is_bounded_and_secrets_are_redacted() {
        let logs = Arc::new(Mutex::new(LogBuffer::default()));
        let mut pending = Vec::new();
        let mut dropping = false;
        consume_chunk(
            &vec![b'x'; MAX_LOG_LINE * 3],
            &mut pending,
            &mut dropping,
            &logs,
            "private-secret",
        );
        consume_chunk(
            b"\nprivate-secret\n",
            &mut pending,
            &mut dropping,
            &logs,
            "private-secret",
        );
        let logs = logs.lock().unwrap();
        assert!(
            logs.lines
                .iter()
                .all(|(_, line)| line.len() <= MAX_LOG_LINE)
        );
        assert_eq!(logs.lines.back().unwrap().1, "[redacted]");
    }
    #[test]
    fn secrets_crossing_read_chunks_and_the_line_budget_are_not_exposed_in_parts() {
        let secret = "0123456789abcdef".repeat(4);
        for prefix in [32, MAX_LOG_LINE - 32] {
            let logs = Arc::new(Mutex::new(LogBuffer::default()));
            let mut pending = Vec::new();
            let mut dropping = false;
            let mut line = vec![b'x'; prefix];
            line.extend_from_slice(secret.as_bytes());
            line.push(b'\n');
            let boundary = prefix + 32;
            consume_chunk(
                &line[..boundary],
                &mut pending,
                &mut dropping,
                &logs,
                &secret,
            );
            consume_chunk(
                &line[boundary..],
                &mut pending,
                &mut dropping,
                &logs,
                &secret,
            );
            let logs = logs.lock().unwrap();
            let joined: String = logs.lines.iter().map(|(_, text)| text.as_str()).collect();
            assert!(!joined.contains(&secret));
            assert!(!joined.contains(&secret[..32]));
        }
    }
    #[test]
    fn log_history_evicts_old_lines_and_preserves_monotonic_cursors() {
        let mut logs = LogBuffer::default();
        for _ in 0..MAX_LOG_LINES * 2 {
            logs.append(b"line", "secret");
        }
        assert_eq!(logs.lines.len(), MAX_LOG_LINES);
        assert_eq!(logs.cursor, (MAX_LOG_LINES * 2) as u64);
        assert_eq!(logs.bytes, MAX_LOG_LINES * 4);
    }
}
