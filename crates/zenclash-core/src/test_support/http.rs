//! Bounded HTTP request frames and response status lines for isolated fixtures.

use std::io::{self, Read};

use tokio::io::{AsyncRead, AsyncReadExt};

const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_HEADER_BYTES: usize = 8 * 1024;

#[derive(Debug, thiserror::Error)]
enum IncompleteRequest {
    #[error("controller request headers are incomplete")]
    Headers,
    #[error("controller request body is incomplete")]
    Body,
}

/// Distinguishes a cancelled header submission from a truncated admitted body.
pub fn is_controller_header_eof(error: &io::Error) -> bool {
    matches!(
        error
            .get_ref()
            .and_then(|source| source.downcast_ref::<IncompleteRequest>()),
        Some(IncompleteRequest::Headers)
    )
}

/// One complete Content-Length request from an isolated controller connection.
pub struct ControllerRequest {
    bytes: Vec<u8>,
    header_end: usize,
}

impl ControllerRequest {
    /// Returns the validated UTF-8 headers, excluding their terminating blank line.
    ///
    /// # Panics
    /// Panics only if this module's private UTF-8 invariant is violated.
    pub fn headers(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.header_end])
            .expect("request collector validated UTF-8 headers")
    }

    /// Returns exactly the declared body bytes.
    pub fn body(&self) -> &[u8] {
        &self.bytes[self.header_end + 4..]
    }

    /// Returns the original complete request without copying it.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Splits ownership into headers including the blank line, and body bytes.
    ///
    /// # Panics
    /// Panics only if this module's private UTF-8 invariant is violated.
    pub fn into_parts(mut self) -> (String, Vec<u8>) {
        let body = self.bytes.split_off(self.header_end + 4);
        let headers = String::from_utf8(self.bytes).expect("validated UTF-8 headers");
        (headers, body)
    }
}

/// Reads only an HTTP response status line, leaving headers and tunnel data unread.
///
/// # Errors
/// Returns I/O or EOF errors, or `InvalidData` when the line exceeds 4 KiB.
/// Malformed or non-UTF-8 status lines return `None`.
pub fn read_http_status_line_blocking(stream: &mut impl Read) -> io::Result<Option<u16>> {
    let mut line = Vec::new();
    while line.len() < 4096 {
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        line.push(byte[0]);
        if line.ends_with(b"\r\n") {
            return Ok(std::str::from_utf8(&line).ok().and_then(|line| {
                let mut parts = line.split_whitespace();
                let protocol = parts.next()?;
                if !matches!(protocol, "HTTP/1.0" | "HTTP/1.1") {
                    return None;
                }
                parts
                    .next()?
                    .parse::<u16>()
                    .ok()
                    .filter(|status| (100..=599).contains(status))
            }));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "oversized HTTP response status line",
    ))
}

/// Reads one isolated request with an 8 KiB header and 64 KiB total budget.
///
/// # Errors
/// Returns I/O errors or early EOF; invalid header framing, chunked, pipelined or oversized
/// requests return `InvalidData`. Request-line semantics are checked by each scenario;
/// this is a framing fixture, not a production HTTP parser.
pub async fn read_controller_request(
    stream: &mut (impl AsyncRead + Unpin + Send),
) -> io::Result<ControllerRequest> {
    read_controller_request_with_limit(stream, MAX_REQUEST_BYTES).await
}

/// Reads one isolated request with an explicit total byte budget and 8 KiB header budget.
/// Large-payload fixtures can opt in without relaxing the ordinary 64 KiB limit.
///
/// # Errors
/// Returns the same I/O, EOF and format errors as [`read_controller_request`].
pub async fn read_controller_request_with_limit(
    stream: &mut (impl AsyncRead + Unpin + Send),
    max_request_bytes: usize,
) -> io::Result<ControllerRequest> {
    let mut collector = RequestCollector::new(max_request_bytes);
    loop {
        let mut bytes = [0; 4096];
        let length = stream.read(&mut bytes).await?;
        if length == 0 {
            return Err(collector.eof());
        }
        if collector.push(&bytes[..length])? {
            return Ok(collector.finish());
        }
    }
}

/// Reads one isolated request synchronously under the same header and total budgets.
///
/// # Errors
/// Returns the same I/O, EOF and format errors as [`read_controller_request`].
pub fn read_controller_request_blocking(stream: &mut impl Read) -> io::Result<ControllerRequest> {
    let mut collector = RequestCollector::new(MAX_REQUEST_BYTES);
    loop {
        let mut bytes = [0; 4096];
        let length = stream.read(&mut bytes)?;
        if length == 0 {
            return Err(collector.eof());
        }
        if collector.push(&bytes[..length])? {
            return Ok(collector.finish());
        }
    }
}

struct RequestCollector {
    bytes: Vec<u8>,
    header_end: Option<usize>,
    total: Option<usize>,
    max_request_bytes: usize,
}

impl RequestCollector {
    fn new(max_request_bytes: usize) -> Self {
        Self {
            bytes: Vec::new(),
            header_end: None,
            total: None,
            max_request_bytes,
        }
    }

    fn push(&mut self, chunk: &[u8]) -> io::Result<bool> {
        if self.bytes.len().saturating_add(chunk.len()) > self.max_request_bytes {
            return Err(invalid_request());
        }
        self.bytes.extend_from_slice(chunk);
        if self.header_end.is_none()
            && let Some(end) = self.bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n")
        {
            if end + 4 > MAX_HEADER_BYTES {
                return Err(invalid_request());
            }
            let headers = std::str::from_utf8(&self.bytes[..end]).map_err(|_| invalid_request())?;
            let mut body_length = None;
            for line in headers.lines().skip(1) {
                let (name, value) = line.split_once(':').ok_or_else(invalid_request)?;
                if name.eq_ignore_ascii_case("content-length") {
                    if body_length.is_some() {
                        return Err(invalid_request());
                    }
                    body_length = Some(
                        value
                            .trim()
                            .parse::<usize>()
                            .map_err(|_| invalid_request())?,
                    );
                } else if name.eq_ignore_ascii_case("transfer-encoding") {
                    // These fixtures deliberately serve one Content-Length request,
                    // not streaming/chunked or pipelined HTTP connections.
                    return Err(invalid_request());
                }
            }
            let total = end
                .checked_add(4)
                .and_then(|headers| headers.checked_add(body_length.unwrap_or(0)))
                .filter(|total| *total <= self.max_request_bytes)
                .ok_or_else(invalid_request)?;
            self.header_end = Some(end);
            self.total = Some(total);
        }
        if self.header_end.is_none() && self.bytes.len() > MAX_HEADER_BYTES {
            return Err(invalid_request());
        }
        if let Some(total) = self.total {
            if self.bytes.len() > total {
                return Err(invalid_request());
            }
            return Ok(self.bytes.len() == total);
        }
        Ok(false)
    }

    fn finish(self) -> ControllerRequest {
        ControllerRequest {
            bytes: self.bytes,
            header_end: self
                .header_end
                .expect("only complete requests are returned"),
        }
    }

    fn eof(&self) -> io::Error {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            if self.header_end.is_some() {
                IncompleteRequest::Body
            } else {
                IncompleteRequest::Headers
            },
        )
    }
}

fn invalid_request() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "unsupported or oversized controller fixture request",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn response_status_reader_waits_for_the_complete_line_and_preserves_tunnel_bytes() {
        struct Fragments<'a>(&'a [u8]);
        impl Read for Fragments<'_> {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                let length = usize::from(!self.0.is_empty()).min(output.len());
                output[..length].copy_from_slice(&self.0[..length]);
                self.0 = &self.0[length..];
                Ok(length)
            }
        }
        let mut input = Fragments(b"HTTP/1.1 200 Connection established\r\ntunnel bytes");
        let status = read_http_status_line_blocking(&mut input).unwrap();
        assert_eq!(status, Some(200));
        assert_eq!(input.0, b"tunnel bytes");
    }

    #[test]
    fn response_status_without_line_end_is_an_eof_error() {
        let result = read_http_status_line_blocking(&mut b"HTTP/1.1 200".as_slice());
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::UnexpectedEof));
    }

    #[test]
    fn response_status_line_has_a_budget_even_without_a_line_end() {
        let data = vec![b'x'; 4097];
        let result = read_http_status_line_blocking(&mut data.as_slice());
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::InvalidData));
    }

    #[tokio::test]
    async fn fragmented_headers_and_body_return_only_the_complete_request() {
        let (mut writer, mut reader) = tokio::io::duplex(32);
        let write = tokio::spawn(async move {
            for chunk in [
                b"PUT /configs HTTP/1.1\r\nContent-Len".as_slice(),
                b"gth: 7\r\n\r".as_slice(),
                b"\n{\"x\":1}".as_slice(),
            ] {
                writer.write_all(chunk).await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        let request = read_controller_request(&mut reader).await.unwrap();
        write.await.unwrap();
        assert_eq!(
            request.headers(),
            "PUT /configs HTTP/1.1\r\nContent-Length: 7"
        );
        assert_eq!(request.body(), b"{\"x\":1}");
    }

    #[tokio::test]
    async fn truncated_body_is_not_accepted_as_an_admitted_request() {
        let mut input = b"PUT /configs HTTP/1.1\r\nContent-Length: 7\r\n\r\n{}".as_slice();
        let result = read_controller_request(&mut input).await;
        assert!(matches!(result, Err(error)
            if error.kind() == io::ErrorKind::UnexpectedEof && !is_controller_header_eof(&error)));
    }

    #[test]
    fn blocking_reader_collects_every_fragment_before_returning_body() {
        struct Fragments<'a>(&'a [u8]);
        impl Read for Fragments<'_> {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                let length = output.len().min(self.0.len()).min(2);
                output[..length].copy_from_slice(&self.0[..length]);
                self.0 = &self.0[length..];
                Ok(length)
            }
        }
        let mut input = Fragments(b"POST /x HTTP/1.1\r\ncontent-length: 3\r\n\r\nabc");
        let request = read_controller_request_blocking(&mut input).unwrap();
        assert_eq!(request.body(), b"abc");
    }

    #[tokio::test]
    async fn explicit_request_budget_accepts_a_large_body_without_relaxing_the_default() {
        let body = vec![b'x'; MAX_REQUEST_BYTES];
        let mut bytes = format!(
            "PUT /configs HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        bytes.extend_from_slice(&body);
        let request = read_controller_request_with_limit(&mut bytes.as_slice(), bytes.len())
            .await
            .unwrap();
        assert_eq!(request.body(), body);
        assert!(matches!(
            read_controller_request(&mut bytes.as_slice()).await,
            Err(error) if error.kind() == io::ErrorKind::InvalidData
        ));
    }

    #[tokio::test]
    async fn explicit_request_budget_rejects_oversized_declared_bodies() {
        let mut input = b"PUT /configs HTTP/1.1\r\nContent-Length: 65536\r\n\r\n".as_slice();
        let result = read_controller_request_with_limit(&mut input, 1024).await;
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::InvalidData));
    }

    #[test]
    fn excessive_declared_body_is_rejected_before_allocating_or_waiting_for_it() {
        let bytes = format!(
            "PUT /configs HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_REQUEST_BYTES
        );
        let result = read_controller_request_blocking(&mut bytes.as_bytes());
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::InvalidData));
    }

    #[test]
    fn partial_headers_preserve_eof_instead_of_becoming_an_empty_get() {
        let result =
            read_controller_request_blocking(&mut b"GET /configs HTTP/1.1\r\nHost:".as_slice());
        assert!(matches!(result, Err(error)
            if error.kind() == io::ErrorKind::UnexpectedEof && is_controller_header_eof(&error)));
    }

    #[test]
    fn excessive_headers_are_rejected_before_a_body_can_be_admitted() {
        let data = format!(
            "GET /x HTTP/1.1\r\nX-Large: {}\r\n\r\n",
            "x".repeat(MAX_HEADER_BYTES)
        );
        let result = read_controller_request_blocking(&mut data.as_bytes());
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::InvalidData));
    }
}
