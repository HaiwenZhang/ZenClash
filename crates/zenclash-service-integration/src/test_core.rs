// Based on the fork fixture; ZenClash native-controller test server added 2026-10-04; shared CLI validation adapted 2026-10-05.
// GPL-3.0-only; strictly gated behind ipc-tests and excluded from production builds.
#![cfg(feature = "ipc-tests")]

use anyhow::{Context as _, Result};
use http_body_util::{BodyExt as _, Full};
use hyper::{
    Request, Response,
    body::{Bytes, Incoming},
};
use hyper_util::rt::TokioIo;
use std::{convert::Infallible, sync::Arc};

type Mode = Arc<parking_lot::Mutex<String>>;

/// Runs the isolated native-controller fixture, including syntax validation.
/// This module is available only with the non-production `ipc-tests` feature.
///
/// # Errors
/// Returns invalid fixture arguments, configuration errors, or native listener failures.
pub async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let value = |flag: &str| {
        args.windows(2)
            .find(|args| args[0] == flag)
            .map(|args| args[1].clone())
    };
    if args
        .iter()
        .any(|argument| argument == "--version" || argument == "-v")
    {
        println!("Mihomo Meta integration-fixture");
        return Ok(());
    }
    if args.iter().any(|argument| argument == "-t") {
        let config = value("-f").context("fixture validation config missing")?;
        read_mode(&config)?;
        return Ok(());
    }
    let path = value("-ext-ctl-unix")
        .or_else(|| value("-ext-ctl-pipe"))
        .context("fixture IPC path missing")?;
    let initial = value("-f").context("fixture config path missing")?;
    let mode = Arc::new(parking_lot::Mutex::new(read_mode(&initial)?));
    #[cfg(unix)]
    let listener = tokio::net::UnixListener::bind(&path)?;
    #[cfg(windows)]
    let mut listener = tokio::net::windows::named_pipe::ServerOptions::new().create(&path)?;
    loop {
        #[cfg(unix)]
        let stream = listener.accept().await?.0;
        #[cfg(windows)]
        let stream = {
            listener.connect().await?;
            let replacement =
                tokio::net::windows::named_pipe::ServerOptions::new().create(&path)?;
            std::mem::replace(&mut listener, replacement)
        };
        let mode = Arc::clone(&mode);
        tokio::spawn(async move {
            let service =
                hyper::service::service_fn(move |request| route(request, Arc::clone(&mode)));
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .with_upgrades()
                .await;
        });
    }
}

fn read_mode(path: &str) -> Result<String> {
    let config: serde_yaml::Value = serde_yaml::from_str(&std::fs::read_to_string(path)?)?;
    Ok(config
        .get("mode")
        .and_then(serde_yaml::Value::as_str)
        .unwrap_or("rule")
        .to_owned())
}

async fn route(
    mut request: Request<Incoming>,
    mode: Mode,
) -> Result<Response<Full<Bytes>>, Infallible> {
    use futures_util::SinkExt as _;
    if let Some(key) = request.headers().get("sec-websocket-key") {
        let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
        let oversized = request.uri().query() == Some("oversized");
        let upgrade = hyper::upgrade::on(&mut request);
        tokio::spawn(async move {
            if let Ok(stream) = upgrade.await {
                let mut socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
                    TokioIo::new(stream),
                    tokio_tungstenite::tungstenite::protocol::Role::Server,
                    None,
                )
                .await;
                let message = if oversized {
                    tokio_tungstenite::tungstenite::Message::Binary(vec![b'x'; 128 * 1024 + 1])
                } else {
                    tokio_tungstenite::tungstenite::Message::Text(
                        "{\"up\":1,\"down\":2}".to_owned(),
                    )
                };
                let _ = socket.send(message).await;
            }
        });
        return Ok(Response::builder()
            .status(101)
            .header("upgrade", "websocket")
            .header("connection", "Upgrade")
            .header("sec-websocket-accept", accept)
            .body(Full::new(Bytes::new()))
            .expect("fixture upgrade headers are valid"));
    }
    let (status, body) = match (request.method().as_str(), request.uri().path()) {
        ("GET", "/version") => (
            200,
            serde_json::json!({"meta":true,"version":"integration-fixture"}),
        ),
        ("GET", "/configs") => (200, serde_json::json!({"mode":mode.lock().clone()})),
        ("PUT", "/configs") => {
            let loaded = async {
                let bytes = request.into_body().collect().await?.to_bytes();
                let body: serde_json::Value = serde_json::from_slice(&bytes)?;
                let path = body["path"]
                    .as_str()
                    .context("fixture reload path missing")?;
                *mode.lock() = read_mode(path)?;
                Ok::<_, anyhow::Error>(())
            }
            .await;
            if loaded.is_ok() {
                (204, serde_json::Value::Null)
            } else {
                (
                    400,
                    serde_json::json!({"message":"fixture reload rejected"}),
                )
            }
        }
        _ => (
            404,
            serde_json::json!({"message":"fixture endpoint missing"}),
        ),
    };
    Ok(Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(if status == 204 {
            Vec::new()
        } else {
            serde_json::to_vec(&body).expect("fixture JSON")
        })))
        .expect("fixture response is valid"))
}
