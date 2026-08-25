use std::time::Duration;

use http::{header::AUTHORIZATION, HeaderValue};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, handshake::client::Request},
    MaybeTlsStream, WebSocketStream,
};

use crate::MihomoEndpoint;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub type MihomoSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

pub async fn connect_stream(
    endpoint: &MihomoEndpoint,
    path: &str,
    query: &[(&str, &str)],
    timeout_message: &str,
) -> Result<MihomoSocket, String> {
    let request = stream_request(endpoint, path, query)?;
    tokio::time::timeout(CONNECT_TIMEOUT, connect_async(request))
        .await
        .map_err(|_| timeout_message.to_owned())?
        .map(|(socket, _)| socket)
        .map_err(|error| error.to_string())
}

fn stream_request(
    endpoint: &MihomoEndpoint,
    path: &str,
    query: &[(&str, &str)],
) -> Result<Request, String> {
    let websocket_url = endpoint
        .websocket_url(path)
        .map_err(|error| error.to_string())?;
    let mut url = reqwest::Url::parse(&websocket_url).map_err(|error| error.to_string())?;
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query.iter().copied());
    }
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|error| error.to_string())?;
    if !endpoint.secret.is_empty() {
        let value = HeaderValue::from_str(&format!("Bearer {}", endpoint.secret))
            .map_err(|_| "invalid Mihomo authorization header".to_owned())?;
        request.headers_mut().insert(AUTHORIZATION, value);
    }
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_request_encodes_query_and_applies_bearer_secret() {
        let endpoint = MihomoEndpoint::new("http://127.0.0.1:9090/base", "top-secret");

        let request = stream_request(&endpoint, "/logs", &[("level", "info / debug")]).unwrap();

        assert_eq!(
            request.uri().to_string(),
            "ws://127.0.0.1:9090/base/logs?level=info+%2F+debug"
        );
        assert_eq!(request.headers()[AUTHORIZATION], "Bearer top-secret");
    }

    #[test]
    fn stream_request_rejects_secret_that_is_not_a_header_value() {
        let endpoint = MihomoEndpoint::new("http://127.0.0.1:9090", "bad\nsecret");

        let error = stream_request(&endpoint, "/traffic", &[]).unwrap_err();

        assert_eq!(error, "invalid Mihomo authorization header");
    }
}
