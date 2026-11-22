//! Event-driven bounded PAC HTTP server with blocking `accept()`,
//! self-connect shutdown wakeup, and bounded concurrent request handling.

use std::{
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use parking_lot::Mutex;

const MAX_REQUEST_HEADER_BYTES: usize = 8 * 1024;
const REQUEST_HEADER_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_READ_POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAX_CONCURRENT_CONNECTIONS: usize = 8;

pub(super) type ActiveConnections = Arc<Mutex<Vec<(u64, TcpStream)>>>;

struct ConnectionSlotGuard {
    in_flight: Arc<AtomicUsize>,
    active_connections: ActiveConnections,
    id: u64,
}

impl Drop for ConnectionSlotGuard {
    fn drop(&mut self) {
        self.active_connections
            .lock()
            .retain(|(entry_id, _)| *entry_id != self.id);
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(super) fn run_server(
    listener: &TcpListener,
    script: &Arc<[u8]>,
    shutdown: &Arc<AtomicBool>,
    availability: &Arc<AtomicBool>,
    active_connections: &ActiveConnections,
) {
    let in_flight = Arc::new(AtomicUsize::new(0));
    let next_connection_id = AtomicU64::new(1);
    let mut handlers: Vec<thread::JoinHandle<()>> = Vec::new();

    while !shutdown.load(Ordering::Acquire) {
        reap_finished_handlers(&mut handlers);
        match listener.accept() {
            Ok((_stream, _)) if shutdown.load(Ordering::Acquire) => break,
            Ok((stream, _)) => {
                if !try_acquire_connection_slot(&in_flight) {
                    tracing::debug!(
                        limit = MAX_CONCURRENT_CONNECTIONS,
                        "PAC concurrency limit reached; rejecting excess connection"
                    );
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let id = next_connection_id.fetch_add(1, Ordering::Relaxed);
                let connection = match stream.try_clone() {
                    Ok(connection) => connection,
                    Err(error) => {
                        in_flight.fetch_sub(1, Ordering::AcqRel);
                        tracing::warn!(%error, "PAC connection could not be retained for shutdown");
                        continue;
                    }
                };
                {
                    let mut active = active_connections.lock();
                    // Register under the same lock used by Drop, so shutdown
                    // cannot miss a connection accepted just before it began.
                    if shutdown.load(Ordering::Acquire) {
                        in_flight.fetch_sub(1, Ordering::AcqRel);
                        let _ = stream.shutdown(Shutdown::Both);
                        break;
                    }
                    active.push((id, connection));
                }
                let slot_guard = ConnectionSlotGuard {
                    in_flight: in_flight.clone(),
                    active_connections: active_connections.clone(),
                    id,
                };
                let script = script.clone();
                let availability = availability.clone();
                let shutdown = shutdown.clone();
                match thread::Builder::new()
                    .name(format!("zenclash-pac-conn-{id}"))
                    .spawn(move || {
                        let _slot = slot_guard;
                        let result = serve_connection(stream, &script, &availability, &shutdown);
                        if let Err(error) = result
                            && !shutdown.load(Ordering::Acquire)
                        {
                            tracing::warn!(%error, "PAC request failed");
                        }
                    }) {
                    Ok(handle) => handlers.push(handle),
                    Err(error) => {
                        tracing::warn!(%error, "failed to spawn PAC connection handler");
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                if !shutdown.load(Ordering::Acquire) {
                    tracing::warn!(%error, "PAC listener stopped unexpectedly");
                }
                break;
            }
        }
    }

    for handle in handlers {
        let _ = handle.join();
    }
}

fn try_acquire_connection_slot(in_flight: &AtomicUsize) -> bool {
    in_flight
        .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            (current < MAX_CONCURRENT_CONNECTIONS).then_some(current + 1)
        })
        .is_ok()
}

fn reap_finished_handlers(handlers: &mut Vec<thread::JoinHandle<()>>) {
    let mut index = 0;
    while index < handlers.len() {
        if handlers[index].is_finished() {
            let handle = handlers.swap_remove(index);
            let _ = handle.join();
        } else {
            index += 1;
        }
    }
}

fn is_pac_request_target(target: &str) -> bool {
    target == "/pac" || target.starts_with("/pac?")
}

fn serve_connection(
    mut stream: TcpStream,
    script: &[u8],
    availability: &AtomicBool,
    shutdown: &AtomicBool,
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let deadline = Instant::now() + REQUEST_HEADER_TIMEOUT;
    let mut request = [0_u8; MAX_REQUEST_HEADER_BYTES];
    let mut request_length = 0;
    let mut headers_complete = false;
    // Closing with unread request headers can reset the connection on Windows.
    while request_length < request.len() && !headers_complete {
        if shutdown.load(Ordering::Acquire) {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "PAC request header deadline expired",
            ));
        }
        // A read timeout alone is renewed by each byte. Bound the complete
        // request headers, while still accepting normally fragmented requests.
        stream.set_read_timeout(Some(remaining.min(REQUEST_READ_POLL_INTERVAL)))?;
        match stream.read(&mut request[request_length..]) {
            Ok(0) => break,
            Ok(read) => {
                let previous_length = request_length;
                request_length += read;
                headers_complete = request[previous_length.saturating_sub(3)..request_length]
                    .windows(4)
                    .any(|bytes| bytes == b"\r\n\r\n");
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
    }
    if shutdown.load(Ordering::Acquire) {
        return Ok(());
    }
    if !headers_complete {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "PAC request headers are incomplete or oversized",
        ));
    }
    let request_line = std::str::from_utf8(&request[..request_length])
        .ok()
        .and_then(|request| request.lines().next())
        .unwrap_or_default();
    let serves_pac = request_line
        .split_whitespace()
        .next()
        .is_some_and(|method| method == "GET" || method == "HEAD")
        && request_line
            .split_whitespace()
            .nth(1)
            .is_some_and(is_pac_request_target);
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
    stream.flush()?;
    // Send FIN explicitly before either socket handle is dropped, so Windows
    // clients can read the complete response through a graceful EOF.
    stream.shutdown(Shutdown::Write)
}
