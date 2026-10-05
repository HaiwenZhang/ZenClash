// PAC inactivity behavior adapted from Clash Verge Rev on 2026-10-05.
// GPL-3.0-only; see the repository NOTICE.md for upstream attribution.
use std::{
    fmt,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use parking_lot::Mutex;

use crate::{MihomoError, MihomoResult};

const MAX_PAC_SCRIPT_BYTES: usize = 256 * 1024;
const MAX_HTTP_REQUEST_BYTES: usize = 8 * 1024;
const DEFAULT_PAC_SCRIPT: &str = r#"function FindProxyForURL(url, host) {
  return "PROXY 127.0.0.1:%mixed-port%; SOCKS5 127.0.0.1:%mixed-port%; DIRECT;";
}
"#;

/// Snapshot of the PAC HTTP service currently owned by `ZenClash`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PacServerStatus {
    /// Local socket accepting PAC requests.
    pub address: SocketAddr,
    /// URL written into the operating system's automatic-proxy setting.
    pub url: String,
}

/// Cloneable owner for the bounded local PAC HTTP service.
#[derive(Clone, Default)]
pub struct PacServer {
    inner: Arc<PacServerInner>,
}

impl fmt::Debug for PacServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PacServer")
            .field("status", &self.status())
            .finish()
    }
}

#[derive(Default)]
struct PacServerInner {
    running: Mutex<Option<RunningPacServer>>,
    retained: Mutex<Option<RunningPacServer>>,
    availability: PacAvailability,
}

// Workers own only this atomic, never PacServerInner; dropping the last owner
// must still stop and join every current or retained listener.
struct PacAvailability(Arc<AtomicBool>);

impl Default for PacAvailability {
    fn default() -> Self {
        // Standalone and external-controller users retain their existing behavior.
        // Lifecycle integration must close the endpoint before managed startup.
        Self(Arc::new(AtomicBool::new(true)))
    }
}

impl Drop for PacServerInner {
    fn drop(&mut self) {
        if let Some(running) = self.running.get_mut().take() {
            drop(running);
        }
    }
}

pub(super) struct RunningPacServer {
    status: PacServerStatus,
    shutdown: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for RunningPacServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.status.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl PacServer {
    /// Starts or atomically replaces the PAC service on the requested host.
    ///
    /// `%mixed-port%` placeholders are replaced with `proxy_port` before the
    /// script becomes visible. The previous service remains alive if binding
    /// or spawning the replacement fails.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid host, port, or PAC document, or when the
    /// local listener/thread cannot be created.
    pub fn start(
        &self,
        bind_host: &str,
        script: &str,
        proxy_port: u16,
    ) -> MihomoResult<PacServerStatus> {
        let replacement = self.prepare(bind_host, script, proxy_port)?;
        let status = replacement.status.clone();
        self.commit(replacement);
        Ok(status)
    }

    pub(super) fn prepare(
        &self,
        bind_host: &str,
        script: &str,
        proxy_port: u16,
    ) -> MihomoResult<RunningPacServer> {
        if self.inner.retained.lock().is_some() {
            return Err(MihomoError::Process(zenclash_i18n::text(
                "system_proxy.errors.pending_recovery",
            )));
        }
        let bind_host = super::normalize_system_proxy_host(bind_host)?;
        if proxy_port == 0 {
            return Err(MihomoError::Process("PAC 代理端口不能为 0".into()));
        }
        let script = normalize_pac_script(script)?
            .replace("%mixed-port%", &proxy_port.to_string())
            .into_bytes();
        let listener = TcpListener::bind((bind_host.as_str(), 0)).map_err(|error| {
            MihomoError::Process(format!("无法在 {bind_host} 启动 PAC 服务：{error}"))
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|error| MihomoError::Process(format!("无法配置 PAC 监听器：{error}")))?;
        let address = listener
            .local_addr()
            .map_err(|error| MihomoError::Process(format!("无法读取 PAC 监听地址：{error}")))?;
        let status = PacServerStatus {
            address,
            url: format!("http://{}/pac", socket_authority(address)),
        };
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = shutdown.clone();
        let availability = self.inner.availability.0.clone();
        let script: Arc<[u8]> = script.into();
        let thread = thread::Builder::new()
            .name("zenclash-pac".into())
            .spawn(move || run_server(&listener, &script, &worker_shutdown, &availability))
            .map_err(|error| MihomoError::Process(format!("无法启动 PAC 服务线程：{error}")))?;
        Ok(RunningPacServer {
            status,
            shutdown,
            thread: Some(thread),
        })
    }

    pub(super) fn commit(&self, replacement: RunningPacServer) {
        let previous = self.inner.running.lock().replace(replacement);
        drop(previous);
    }

    pub(super) fn retain_for_recovery(&self, replacement: RunningPacServer) {
        let mut retained = self.inner.retained.lock();
        debug_assert!(retained.is_none());
        *retained = Some(replacement);
    }

    pub(super) fn discard_retained(&self) {
        let retained = self.inner.retained.lock().take();
        drop(retained);
    }

    pub(super) fn owns_url(&self, url: &str) -> bool {
        self.inner
            .running
            .lock()
            .as_ref()
            .is_some_and(|server| server.status.url == url)
            || self
                .inner
                .retained
                .lock()
                .as_ref()
                .is_some_and(|server| server.status.url == url)
    }

    /// Controls whether requests may receive the configured PAC document.
    ///
    /// Inactive GET/HEAD requests to `/pac` return HTTP 503, as in Clash Verge
    /// Rev. This only changes an atomic flag: the URL, listener, configured
    /// script and OS proxy settings remain unchanged. Every clone, replacement
    /// and listener retained for recovery observes the same flag.
    pub fn set_available(&self, available: bool) {
        self.inner
            .availability
            .0
            .store(available, Ordering::Release);
    }

    /// Reports configured availability without filesystem, platform or socket I/O.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.inner.availability.0.load(Ordering::Acquire)
    }

    /// Stops the current PAC service. Calling this repeatedly is harmless.
    pub fn stop(&self) {
        let running = self.inner.running.lock().take();
        let retained = self.inner.retained.lock().take();
        drop(running);
        drop(retained);
    }

    /// Returns the currently served PAC URL and socket, when running.
    #[must_use]
    pub fn status(&self) -> Option<PacServerStatus> {
        self.inner
            .running
            .lock()
            .as_ref()
            .map(|running| running.status.clone())
    }
}

impl RunningPacServer {
    pub(super) fn status(&self) -> &PacServerStatus {
        &self.status
    }
}

/// Returns the default PAC document used by a fresh installation.
#[must_use]
pub const fn default_pac_script() -> &'static str {
    DEFAULT_PAC_SCRIPT
}

/// Validates and normalizes a user-supplied PAC document.
///
/// # Errors
///
/// Returns an error for an empty/oversized document, embedded NUL bytes, or a
/// document that does not define `FindProxyForURL`.
pub fn normalize_pac_script(script: &str) -> MihomoResult<String> {
    let script = script.trim();
    if script.is_empty() {
        return Err(MihomoError::Process("PAC 脚本不能为空".into()));
    }
    if script.len() > MAX_PAC_SCRIPT_BYTES {
        return Err(MihomoError::Process(format!(
            "PAC 脚本超过 {MAX_PAC_SCRIPT_BYTES} 字节限制"
        )));
    }
    if script.contains('\0') {
        return Err(MihomoError::Process("PAC 脚本不能包含 NUL 字节".into()));
    }
    if !script.contains("FindProxyForURL") {
        return Err(MihomoError::Process(
            "PAC 脚本必须定义 FindProxyForURL".into(),
        ));
    }
    Ok(format!("{script}\n"))
}

fn run_server(
    listener: &TcpListener,
    script: &[u8],
    shutdown: &AtomicBool,
    availability: &AtomicBool,
) {
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((_stream, _)) if shutdown.load(Ordering::Acquire) => break,
            Ok((stream, _)) => {
                if let Err(error) = serve_connection(stream, script, availability) {
                    tracing::warn!(%error, "PAC request failed");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => {
                tracing::warn!(%error, "PAC listener stopped unexpectedly");
                break;
            }
        }
    }
}

fn serve_connection(
    mut stream: TcpStream,
    script: &[u8],
    availability: &AtomicBool,
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut request = [0_u8; MAX_HTTP_REQUEST_BYTES];
    let mut request_length = 0;
    while request_length < request.len() && !request[..request_length].contains(&b'\n') {
        match stream.read(&mut request[request_length..]) {
            Ok(0) => break,
            Ok(read) => request_length += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    let request_line = std::str::from_utf8(&request[..request_length])
        .ok()
        .and_then(|request| request.lines().next())
        .unwrap_or_default();
    let serves_pac = request_line
        .split_whitespace()
        .next()
        .is_some_and(|method| method == "GET" || method == "HEAD")
        && request_line.split_whitespace().nth(1) == Some("/pac");
    let (status, body, content_type) = if !serves_pac {
        (
            "404 Not Found",
            b"Not Found".as_slice(),
            "text/plain; charset=utf-8",
        )
    } else if availability.load(Ordering::Acquire) {
        ("200 OK", script, "application/x-ns-proxy-autoconfig")
    } else {
        (
            "503 Service Unavailable",
            b"PAC endpoint is inactive".as_slice(),
            "text/plain; charset=utf-8",
        )
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if !request_line.starts_with("HEAD ") {
        stream.write_all(body)?;
    }
    stream.flush()
}

fn socket_authority(address: SocketAddr) -> String {
    if address.is_ipv6() {
        format!("[{}]:{}", address.ip(), address.port())
    } else {
        address.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{PacServer, default_pac_script, normalize_pac_script};
    use std::{
        io::{Read, Write},
        net::TcpStream,
    };

    #[test]
    fn pac_server_serves_the_materialized_script_over_real_http() {
        let server = PacServer::default();
        let status = server
            .start("127.0.0.1", default_pac_script(), 17_890)
            .unwrap();
        let mut stream = TcpStream::connect(status.address).unwrap();
        stream
            .write_all(b"GET /pac HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();

        assert!(response.contains("PROXY 127.0.0.1:17890"), "{response}");
    }

    #[test]
    fn pac_server_accepts_a_fragmented_http_request_line() {
        let server = PacServer::default();
        let status = server
            .start("127.0.0.1", default_pac_script(), 17_890)
            .unwrap();
        let mut stream = TcpStream::connect(status.address).unwrap();
        stream.write_all(b"GET /").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        stream
            .write_all(b"pac HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();

        assert!(response.contains("PROXY 127.0.0.1:17890"), "{response}");
    }

    #[test]
    fn pac_server_replacement_keeps_one_observable_listener() {
        let server = PacServer::default();
        let first = server
            .start("127.0.0.1", default_pac_script(), 17_890)
            .unwrap();
        let second = server
            .start("127.0.0.1", default_pac_script(), 17_891)
            .unwrap();

        assert_ne!(first.address, second.address);
        assert_eq!(server.status().unwrap(), second);
    }

    fn request(address: std::net::SocketAddr, request: &[u8]) -> String {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        stream.write_all(request).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn inactive_pac_returns_503_without_exposing_the_configured_script() {
        let server = PacServer::default();
        server.set_available(false);
        let status = server
            .start("127.0.0.1", default_pac_script(), 17_890)
            .unwrap();
        let response = request(status.address, b"GET /pac HTTP/1.1\r\n\r\n");
        assert!(
            response.starts_with("HTTP/1.1 503 Service Unavailable\r\n"),
            "{response}"
        );
        assert!(
            response.ends_with("\r\n\r\nPAC endpoint is inactive"),
            "{response}"
        );
        assert!(!response.contains("FindProxyForURL"), "{response}");
        assert!(
            response.contains("Cache-Control: no-store\r\n"),
            "{response}"
        );
    }

    #[test]
    fn pac_can_pause_and_resume_at_the_same_url_through_an_owner_clone() {
        let server = PacServer::default();
        let status = server
            .start("127.0.0.1", default_pac_script(), 17_890)
            .unwrap();
        let cloned = server.clone();
        cloned.set_available(false);
        assert!(!server.is_available());
        assert!(request(status.address, b"GET /pac HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 503"));
        cloned.set_available(true);
        let resumed = request(status.address, b"GET /pac HTTP/1.1\r\n\r\n");
        assert!(resumed.starts_with("HTTP/1.1 200 OK\r\n"), "{resumed}");
        assert!(resumed.contains("PROXY 127.0.0.1:17890"), "{resumed}");
        assert_eq!(server.status(), Some(status));
    }

    #[test]
    fn replacement_listener_preserves_inactivity() {
        let server = PacServer::default();
        server.set_available(false);
        let first = server
            .start("127.0.0.1", default_pac_script(), 17_890)
            .unwrap();
        let replacement = server
            .start("127.0.0.1", default_pac_script(), 17_891)
            .unwrap();
        assert_ne!(first.address, replacement.address);
        assert!(
            request(replacement.address, b"GET /pac HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 503")
        );
    }

    #[test]
    fn retained_listener_shares_availability_with_the_current_listener() {
        let server = PacServer::default();
        let current = server
            .start("127.0.0.1", default_pac_script(), 17_890)
            .unwrap();
        let candidate = server
            .prepare("127.0.0.1", default_pac_script(), 17_891)
            .unwrap();
        let candidate_address = candidate.status().address;
        server.retain_for_recovery(candidate);
        server.set_available(false);
        assert!(request(current.address, b"GET /pac HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 503"));
        assert!(
            request(candidate_address, b"GET /pac HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 503")
        );
        server.set_available(true);
        assert!(
            request(candidate_address, b"GET /pac HTTP/1.1\r\n\r\n")
                .contains("PROXY 127.0.0.1:17891")
        );
    }

    #[test]
    fn inactive_head_reports_the_error_body_length_without_sending_a_body() {
        let server = PacServer::default();
        server.set_available(false);
        let status = server
            .start("127.0.0.1", default_pac_script(), 17_890)
            .unwrap();
        let response = request(status.address, b"HEAD /pac HTTP/1.1\r\n\r\n");
        assert!(response.starts_with("HTTP/1.1 503"), "{response}");
        assert!(response.contains("Content-Length: 24\r\n"), "{response}");
        assert!(response.ends_with("\r\n\r\n"), "{response}");
    }

    #[test]
    fn inactive_pac_keeps_unknown_routes_as_not_found() {
        let server = PacServer::default();
        server.set_available(false);
        let status = server
            .start("127.0.0.1", default_pac_script(), 17_890)
            .unwrap();
        let response = request(status.address, b"GET /other HTTP/1.1\r\n\r\n");
        assert!(
            response.starts_with("HTTP/1.1 404 Not Found\r\n"),
            "{response}"
        );
    }

    #[test]
    fn a_partial_request_uses_availability_when_its_request_line_finishes() {
        let server = PacServer::default();
        let status = server
            .start("127.0.0.1", default_pac_script(), 17_890)
            .unwrap();
        let mut stream = TcpStream::connect(status.address).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        stream.write_all(b"GET /pac HTTP/1.1").unwrap();
        server.set_available(false);
        stream.write_all(b"\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 503"), "{response}");
    }

    #[test]
    fn stopping_and_starting_again_cannot_reopen_an_inactive_endpoint() {
        let server = PacServer::default();
        server
            .start("127.0.0.1", default_pac_script(), 17_890)
            .unwrap();
        server.set_available(false);
        server.stop();
        let restarted = server
            .start("127.0.0.1", default_pac_script(), 17_891)
            .unwrap();
        assert!(
            request(restarted.address, b"GET /pac HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 503")
        );
    }

    #[test]
    fn dropping_the_final_owner_closes_a_retained_listener_without_a_reference_cycle() {
        let server = PacServer::default();
        let candidate = server
            .prepare("127.0.0.1", default_pac_script(), 17_890)
            .unwrap();
        let address = candidate.status().address;
        server.retain_for_recovery(candidate);
        drop(server);
        assert!(TcpStream::connect_timeout(&address, std::time::Duration::from_secs(1)).is_err());
    }

    #[test]
    fn pac_script_requires_the_standard_entrypoint() {
        let error = normalize_pac_script("function proxy() { return 'DIRECT'; }").unwrap_err();

        assert!(error.to_string().contains("FindProxyForURL"));
    }
}
