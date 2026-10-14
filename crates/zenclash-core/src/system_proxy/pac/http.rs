//! Bounded PAC HTTP requests, with shutdown of the current connection.

use std::{
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use parking_lot::Mutex;

const MAX_REQUEST_HEADER_BYTES: usize = 8 * 1024;
const REQUEST_HEADER_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_READ_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(super) fn run_server(
    listener: &TcpListener,
    script: &[u8],
    shutdown: &AtomicBool,
    availability: &AtomicBool,
    active_connection: &Mutex<Option<TcpStream>>,
) {
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((_stream, _)) if shutdown.load(Ordering::Acquire) => break,
            Ok((stream, _)) => {
                let connection = match stream.try_clone() {
                    Ok(connection) => connection,
                    Err(error) => {
                        tracing::warn!(%error, "PAC connection could not be retained for shutdown");
                        continue;
                    }
                };
                {
                    let mut active = active_connection.lock();
                    // Register under the same lock used by Drop, so shutdown
                    // cannot miss a connection accepted just before it began.
                    if shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    *active = Some(connection);
                }
                let result = serve_connection(stream, script, availability, shutdown);
                active_connection.lock().take();
                if let Err(error) = result
                    && !shutdown.load(Ordering::Acquire)
                {
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
    stream.flush()?;
    // Send FIN explicitly before either socket handle is dropped, so Windows
    // clients can read the complete response through a graceful EOF.
    stream.shutdown(Shutdown::Write)
}
