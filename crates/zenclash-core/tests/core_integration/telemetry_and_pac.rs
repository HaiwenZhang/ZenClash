use std::{net::SocketAddr, time::Duration};

use anyhow::Result;
use futures_util::{SinkExt as _, StreamExt as _};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use zenclash_core::{
    mihomo::{
        MihomoEndpoint, SystemProxyMode, default_pac_script, default_system_proxy_bypass,
        normalize_pac_script, normalize_system_proxy_bypass, normalize_system_proxy_host,
    },
    workflows::{
        LogMonitor, LogTimeSource, MihomoLogLevel, PacServer, TrafficMonitor,
        format_log_entries_support_safe, format_speed,
    },
};

async fn send_http_get(address: SocketAddr, target: &str) -> Result<String> {
    let mut stream = tokio::net::TcpStream::connect(address).await?;
    let request = format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    Ok(response)
}

#[tokio::test]
async fn pac_server_serves_script_bumps_version_and_toggles_availability() -> Result<()> {
    let pac = PacServer::default();
    assert!(pac.status().is_none());

    let initial = pac.start("127.0.0.1", default_pac_script(), 17_890)?;
    assert_eq!(pac.versioned_url(), Some(format!("{}?v=1", initial.url)));

    let ok_response = send_http_get(initial.address, "/pac").await?;
    assert!(ok_response.starts_with("HTTP/1.1 200 OK"));
    assert!(ok_response.contains("application/x-ns-proxy-autoconfig"));
    assert!(ok_response.contains("127.0.0.1:17890"));

    // Versioned query parameters are accepted on `/pac`; unrelated paths return 404.
    let versioned_response = send_http_get(initial.address, "/pac?v=1").await?;
    assert!(versioned_response.starts_with("HTTP/1.1 200 OK"));
    let not_found = send_http_get(initial.address, "/proxy.pac").await?;
    assert!(not_found.starts_with("HTTP/1.1 404 Not Found"));

    // Updating script and port atomically bumps the cache-busting version.
    let custom_script =
        "function FindProxyForURL(url, host) { return \"PROXY 127.0.0.1:%mixed-port%;\"; }";
    let updated = pac.start("127.0.0.1", custom_script, 17_891)?;
    assert_eq!(pac.versioned_url(), Some(format!("{}?v=2", updated.url)));
    let updated_response = send_http_get(updated.address, "/pac?v=2").await?;
    assert!(updated_response.starts_with("HTTP/1.1 200 OK"));
    assert!(updated_response.contains("127.0.0.1:17891"));

    // Marking the server unavailable flips responses to 503 without dropping the listener.
    pac.set_available(false);
    assert!(!pac.is_available());
    let unavailable = send_http_get(updated.address, "/pac").await?;
    assert!(unavailable.starts_with("HTTP/1.1 503 Service Unavailable"));

    pac.stop();
    assert!(pac.status().is_none());
    Ok(())
}

#[tokio::test]
async fn log_monitor_streams_entries_and_redacts_support_safe_output() -> Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = MihomoEndpoint::new(format!("http://{}", listener.local_addr()?), "");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept log websocket");
        let mut socket = accept_async(stream).await.expect("upgrade log websocket");
        let frame = serde_json::json!({
            "type": "warning",
            "payload": "secret token=super-private-value path=/Users/secret",
            "time": "2026-11-22T10:00:00Z",
            "fields": {"token": "do-not-leak"}
        });
        socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .expect("send structured log frame");
        let _ = socket.next().await;
    });

    let monitor = LogMonitor::start(
        &tokio::runtime::Handle::current(),
        endpoint,
        MihomoLogLevel::Debug,
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while monitor.entries().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;

    let entries = monitor.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].time_source, LogTimeSource::Core);
    assert_eq!(entries[0].level, "warning");

    let safe_export = format_log_entries_support_safe(&entries);
    assert!(safe_export.contains("[core] WARNING [message omitted]"));
    assert!(!safe_export.contains("super-private-value"));
    assert!(!safe_export.contains("do-not-leak"));

    drop(monitor);
    tokio::time::timeout(Duration::from_secs(5), server).await??;
    Ok(())
}

#[tokio::test]
async fn traffic_monitor_streams_rates_and_formats_speed_units() -> Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = MihomoEndpoint::new(format!("http://{}", listener.local_addr()?), "");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept traffic websocket");
        let mut socket = accept_async(stream)
            .await
            .expect("upgrade traffic websocket");
        socket
            .send(Message::Text(r#"{"up":2048,"down":1048576}"#.into()))
            .await
            .expect("send traffic frame");
        let _ = socket.next().await;
    });

    let monitor = TrafficMonitor::start(&tokio::runtime::Handle::current(), endpoint);
    tokio::time::timeout(Duration::from_secs(5), async {
        while monitor.snapshot().updated_at_ms == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;

    let snapshot = monitor.snapshot();
    assert!(snapshot.connected);
    assert_eq!(snapshot.upload, 2_048);
    assert_eq!(snapshot.download, 1_048_576);
    assert_eq!(format_speed(0), "0 B/s");
    assert_eq!(format_speed(snapshot.upload), "2.0 KiB/s");
    assert_eq!(format_speed(snapshot.download), "1.0 MiB/s");
    assert_eq!(format_speed(1_073_741_824), "1.0 GiB/s");

    drop(monitor);
    tokio::time::timeout(Duration::from_secs(5), server).await??;
    Ok(())
}

#[test]
fn system_proxy_mode_and_normalizers_validate_and_normalize_inputs() -> Result<()> {
    assert_eq!(SystemProxyMode::default(), SystemProxyMode::Manual);
    assert_eq!(
        serde_json::to_string(&SystemProxyMode::Manual)?,
        "\"manual\""
    );
    assert_eq!(serde_json::to_string(&SystemProxyMode::Pac)?, "\"pac\"");
    assert_eq!(
        serde_json::from_str::<SystemProxyMode>("\"pac\"")?,
        SystemProxyMode::Pac
    );

    let normalized_pac =
        normalize_pac_script("  function FindProxyForURL(u, h) { return 'DIRECT'; }  ")?;
    assert!(normalized_pac.ends_with('\n'));
    assert!(normalize_pac_script("   ").is_err());
    assert!(normalize_pac_script("function FindProxyForURL() {}\0").is_err());
    assert!(normalize_pac_script("function wrongName() { return 'DIRECT'; }").is_err());

    assert_eq!(normalize_system_proxy_host(" 127.0.0.1 ")?, "127.0.0.1");
    assert_eq!(normalize_system_proxy_host("::1")?, "::1");
    assert_eq!(normalize_system_proxy_host("localhost")?, "localhost");
    assert!(normalize_system_proxy_host("").is_err());
    assert!(normalize_system_proxy_host("0.0.0.0").is_err());
    assert!(normalize_system_proxy_host("bad host").is_err());

    let defaults = default_system_proxy_bypass();
    assert!(!defaults.is_empty());
    let bypass = normalize_system_proxy_bypass(&[
        "  localhost ".into(),
        String::new(),
        "LOCALHOST".into(),
        "127.0.0.1".into(),
        "192.168.0.0/16".into(),
    ])?;
    assert_eq!(bypass, vec!["localhost", "127.0.0.1", "192.168.0.0/16"]);

    Ok(())
}
