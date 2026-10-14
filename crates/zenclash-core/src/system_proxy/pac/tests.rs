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
fn pac_server_consumes_fragmented_headers_before_responding() {
    let server = PacServer::default();
    let status = server
        .start("127.0.0.1", default_pac_script(), 17_890)
        .unwrap();
    for available in [true, false] {
        server.set_available(available);
        let mut stream = TcpStream::connect(status.address).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_millis(150)))
            .unwrap();
        stream.write_all(b"GET /pac HTTP/1.1\r\n").unwrap();
        let mut byte = [0];
        let pending = stream.read(&mut byte).unwrap_err();
        assert!(matches!(
            pending.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
        stream.write_all(b"Host: localhost\r\n\r").unwrap();
        let pending = stream.read(&mut byte).unwrap_err();
        assert!(matches!(
            pending.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
        stream.write_all(b"\n").unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let expected = if available {
            "HTTP/1.1 200 OK\r\n"
        } else {
            "HTTP/1.1 503 Service Unavailable\r\n"
        };
        assert!(response.starts_with(expected), "{response}");
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        assert!(headers.contains(&format!("Content-Length: {}\r\n", body.len())));
    }
}

#[test]
fn stopping_pac_interrupts_an_incomplete_request() {
    let server = PacServer::default();
    let status = server
        .start("127.0.0.1", default_pac_script(), 17_890)
        .unwrap();
    let mut stalled = TcpStream::connect(status.address).unwrap();
    stalled.write_all(b"GET /pac").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));

    let started = std::time::Instant::now();
    server.stop();

    assert!(
        started.elapsed() < std::time::Duration::from_millis(700),
        "stop waited for the incomplete request: {:?}",
        started.elapsed()
    );
}

#[test]
fn stopping_pac_interrupts_incomplete_headers() {
    let server = PacServer::default();
    let status = server
        .start("127.0.0.1", default_pac_script(), 17_890)
        .unwrap();
    let mut stalled = TcpStream::connect(status.address).unwrap();
    stalled
        .write_all(b"GET /pac HTTP/1.1\r\nHost: localhost\r\n")
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));

    let started = std::time::Instant::now();
    server.stop();

    assert!(
        started.elapsed() < std::time::Duration::from_millis(700),
        "stop waited for incomplete headers: {:?}",
        started.elapsed()
    );
}

#[test]
fn incomplete_headers_cannot_block_the_next_request_indefinitely() {
    let server = PacServer::default();
    let status = server
        .start("127.0.0.1", default_pac_script(), 17_890)
        .unwrap();
    let mut stalled = TcpStream::connect(status.address).unwrap();
    stalled
        .write_all(b"GET /pac HTTP/1.1\r\nHost: localhost\r\n")
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));

    let response = request(status.address, b"GET /pac HTTP/1.1\r\n\r\n");
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
}

#[test]
fn a_dripping_request_cannot_extend_the_request_deadline() {
    let server = PacServer::default();
    let status = server
        .start("127.0.0.1", default_pac_script(), 17_890)
        .unwrap();
    let mut slow = TcpStream::connect(status.address).unwrap();
    slow.write_all(b"GET /pac").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    let sender = std::thread::spawn(move || {
        for _ in 0..30 {
            std::thread::sleep(std::time::Duration::from_millis(150));
            if slow.write_all(b"x").is_err() {
                break;
            }
        }
    });

    let mut healthy = TcpStream::connect(status.address).unwrap();
    healthy
        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    healthy.write_all(b"GET /pac HTTP/1.1\r\n\r\n").unwrap();
    let mut response = String::new();
    let received = healthy.read_to_string(&mut response);
    sender.join().unwrap();

    assert!(
        received.is_ok() && response.starts_with("HTTP/1.1 200 OK"),
        "a slow request blocked a healthy request: {received:?}, {response}"
    );
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
    assert!(request(replacement.address, b"GET /pac HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 503"));
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
    assert!(request(candidate_address, b"GET /pac HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 503"));
    server.set_available(true);
    assert!(
        request(candidate_address, b"GET /pac HTTP/1.1\r\n\r\n").contains("PROXY 127.0.0.1:17891")
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
    assert!(request(restarted.address, b"GET /pac HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 503"));
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
