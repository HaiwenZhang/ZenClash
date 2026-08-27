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
}

impl Drop for PacServerInner {
    fn drop(&mut self) {
        if let Some(running) = self.running.get_mut().take() {
            drop(running);
        }
    }
}

struct RunningPacServer {
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
        let script: Arc<[u8]> = script.into();
        let thread = thread::Builder::new()
            .name("zenclash-pac".into())
            .spawn(move || run_server(&listener, &script, &worker_shutdown))
            .map_err(|error| MihomoError::Process(format!("无法启动 PAC 服务线程：{error}")))?;
        let replacement = RunningPacServer {
            status: status.clone(),
            shutdown,
            thread: Some(thread),
        };
        let previous = self.inner.running.lock().replace(replacement);
        drop(previous);
        Ok(status)
    }

    /// Stops the current PAC service. Calling this repeatedly is harmless.
    pub fn stop(&self) {
        let running = self.inner.running.lock().take();
        drop(running);
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

fn run_server(listener: &TcpListener, script: &[u8], shutdown: &AtomicBool) {
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((_stream, _)) if shutdown.load(Ordering::Acquire) => break,
            Ok((stream, _)) => {
                if let Err(error) = serve_connection(stream, script) {
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

fn serve_connection(mut stream: TcpStream, script: &[u8]) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut request = [0_u8; MAX_HTTP_REQUEST_BYTES];
    let read = stream.read(&mut request)?;
    let request_line = std::str::from_utf8(&request[..read])
        .ok()
        .and_then(|request| request.lines().next())
        .unwrap_or_default();
    let serves_pac = request_line
        .split_whitespace()
        .next()
        .is_some_and(|method| method == "GET" || method == "HEAD")
        && request_line.split_whitespace().nth(1) == Some("/pac");
    let body = if serves_pac { script } else { b"Not Found" };
    let status = if serves_pac {
        "200 OK"
    } else {
        "404 Not Found"
    };
    let content_type = if serves_pac {
        "application/x-ns-proxy-autoconfig"
    } else {
        "text/plain; charset=utf-8"
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

    #[test]
    fn pac_script_requires_the_standard_entrypoint() {
        let error = normalize_pac_script("function proxy() { return 'DIRECT'; }").unwrap_err();

        assert!(error.to_string().contains("FindProxyForURL"));
    }
}
