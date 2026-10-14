// PAC inactivity behavior adapted from Clash Verge Rev on 2026-10-05.
// GPL-3.0-only; see the repository NOTICE.md for upstream attribution.
use std::{
    fmt,
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use parking_lot::Mutex;

use super::invalid_setting;
use crate::{MihomoResult, SystemProxyError, SystemProxySetting, SystemProxyValidationError};

mod error;
mod http;
pub use error::{PacError, PacServerOperation};

const MAX_PAC_SCRIPT_BYTES: usize = 256 * 1024;
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
    active_connection: Arc<Mutex<Option<TcpStream>>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Drop for RunningPacServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(connection) = self.active_connection.lock().take() {
            // Interrupt both request reads and a response blocked by a slow client.
            let _ = connection.shutdown(Shutdown::Both);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
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
            return Err(SystemProxyError::RecoveryPending.into());
        }
        let bind_host = super::normalize_system_proxy_host(bind_host)?;
        if proxy_port == 0 {
            return Err(invalid_setting(
                SystemProxySetting::Port,
                SystemProxyValidationError::ZeroPort,
            ));
        }
        let script = normalize_pac_script(script)?
            .replace("%mixed-port%", &proxy_port.to_string())
            .into_bytes();
        let listener = TcpListener::bind((bind_host.as_str(), 0)).map_err(|error| {
            SystemProxyError::Pac(PacError::Io {
                operation: PacServerOperation::Bind,
                source: error,
            })
        })?;
        listener.set_nonblocking(true).map_err(|error| {
            SystemProxyError::Pac(PacError::Io {
                operation: PacServerOperation::ConfigureListener,
                source: error,
            })
        })?;
        let address = listener.local_addr().map_err(|error| {
            SystemProxyError::Pac(PacError::Io {
                operation: PacServerOperation::ReadLocalAddress,
                source: error,
            })
        })?;
        let status = PacServerStatus {
            address,
            url: format!("http://{}/pac", socket_authority(address)),
        };
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = shutdown.clone();
        let active_connection = Arc::new(Mutex::new(None));
        let worker_active_connection = active_connection.clone();
        let availability = self.inner.availability.0.clone();
        let script: Arc<[u8]> = script.into();
        let worker = thread::Builder::new()
            .name("zenclash-pac".into())
            .spawn(move || {
                http::run_server(
                    &listener,
                    &script,
                    &worker_shutdown,
                    &availability,
                    &worker_active_connection,
                );
            })
            .map_err(|error| {
                SystemProxyError::Pac(PacError::Io {
                    operation: PacServerOperation::SpawnWorker,
                    source: error,
                })
            })?;
        Ok(RunningPacServer {
            status,
            shutdown,
            active_connection,
            worker: Some(worker),
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

    /// Stops current and retained PAC listeners and joins their workers.
    ///
    /// Pending request reads and response writes are interrupted before joining.
    /// Calling this repeatedly is harmless.
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
        return Err(invalid_setting(
            SystemProxySetting::PacScript,
            SystemProxyValidationError::Empty,
        ));
    }
    if script.len() > MAX_PAC_SCRIPT_BYTES {
        return Err(invalid_setting(
            SystemProxySetting::PacScript,
            SystemProxyValidationError::TooLong {
                maximum: MAX_PAC_SCRIPT_BYTES,
            },
        ));
    }
    if script.contains('\0') {
        return Err(invalid_setting(
            SystemProxySetting::PacScript,
            SystemProxyValidationError::EmbeddedNul,
        ));
    }
    if !script.contains("FindProxyForURL") {
        return Err(invalid_setting(
            SystemProxySetting::PacScript,
            SystemProxyValidationError::MissingPacFunction,
        ));
    }
    Ok(format!("{script}\n"))
}

fn socket_authority(address: SocketAddr) -> String {
    if address.is_ipv6() {
        format!("[{}]:{}", address.ip(), address.port())
    } else {
        address.to_string()
    }
}

#[cfg(test)]
mod tests;
