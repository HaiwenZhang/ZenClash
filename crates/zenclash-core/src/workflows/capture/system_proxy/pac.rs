// PAC inactivity behavior adapted from Clash Verge Rev on 2026-10-05.
// GPL-3.0-only; see the repository NOTICE.md for upstream attribution.
use std::{
    fmt,
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use parking_lot::Mutex;

use super::invalid_setting;
use crate::{MihomoResult, SystemProxyError, SystemProxySetting, SystemProxyValidationError};

mod error;
mod http;
pub use crate::mihomo::system_proxy_types::normalize_pac_script;
pub use error::{PacError, PacServerOperation};

/// Snapshot of the PAC HTTP service currently owned by `ZenClash`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PacServerStatus {
    /// Local socket accepting PAC requests.
    pub address: SocketAddr,
    /// URL written into the operating system's automatic-proxy setting.
    pub url: String,
}

impl PacServerStatus {
    /// Returns a cache-busting PAC URL carrying an incrementing version query
    /// parameter (`?v=<version>`) so OS and browser proxy caches reload on changes.
    #[must_use]
    pub fn versioned_url(&self, version: u64) -> String {
        format!("{}?v={version}", self.url)
    }
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
    next_version: AtomicU64,
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
    version: u64,
    shutdown: Arc<AtomicBool>,
    active_connections: http::ActiveConnections,
    worker: Option<thread::JoinHandle<()>>,
}

impl Drop for RunningPacServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        {
            let mut connections = self.active_connections.lock();
            for (_, connection) in connections.drain(..) {
                // Interrupt both request reads and responses blocked by slow clients.
                let _ = connection.shutdown(Shutdown::Both);
            }
        }
        // Self-connect wakes the blocking `TcpListener::accept()` loop immediately
        // without polling or sleeping.
        let _ = TcpStream::connect_timeout(&self.status.address, Duration::from_millis(200));
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
        let address = listener.local_addr().map_err(|error| {
            SystemProxyError::Pac(PacError::Io {
                operation: PacServerOperation::ReadLocalAddress,
                source: error,
            })
        })?;
        let version = self.inner.next_version.fetch_add(1, Ordering::AcqRel) + 1;
        let status = PacServerStatus {
            address,
            url: format!("http://{}/pac", socket_authority(address)),
        };
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = shutdown.clone();
        let active_connections = Arc::new(Mutex::new(Vec::new()));
        let worker_active_connections = active_connections.clone();
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
                    &worker_active_connections,
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
            version,
            shutdown,
            active_connections,
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
        let matches = |server: &RunningPacServer| {
            server.status.url == url || server.status.versioned_url(server.version) == url
        };
        self.inner.running.lock().as_ref().is_some_and(matches)
            || self.inner.retained.lock().as_ref().is_some_and(matches)
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

    /// Returns a cache-busting versioned URL (`?v=<n>`) for the running PAC server.
    #[must_use]
    pub fn versioned_url(&self) -> Option<String> {
        self.inner
            .running
            .lock()
            .as_ref()
            .map(|running| running.status.versioned_url(running.version))
    }
}

impl crate::runtime::session::CoreSession {
    /// Binds the actual application PAC owner without changing OS proxy settings.
    /// Managed sessions stay closed until actual liveness has been observed.
    pub fn attach_pac_server(&self, server: PacServer) {
        self.attach_pac_availability_sink(move |available| server.set_available(available));
    }
}

impl RunningPacServer {
    pub(super) fn status(&self) -> &PacServerStatus {
        &self.status
    }
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
