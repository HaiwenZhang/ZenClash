//! Read policy for this capability on the caller-selected runtime.
//!
//! The ordinary page reader supplies the original pinned client and verifies it
//! again after this policy returns. The payload grants no publication eligibility.

use crate::pages::runtime::{PageSnapshotData, page_observation::now_ms};
use zenclash_core::{MihomoClient, Observation, ProxyOperations, ProxyVisibility, RecoveryAction};

pub(in crate::pages::runtime) async fn load(
    client: MihomoClient,
) -> Result<PageSnapshotData, String> {
    let proxy_operations = ProxyOperations::new(client.clone());
    let (config, proxies) = tokio::join!(
        client.runtime_config(),
        proxy_operations.catalog(ProxyVisibility::VisibleOnly),
    );
    let observed_at_ms = now_ms();
    Ok(PageSnapshotData::Dashboard {
        config: Observation::record(
            &Observation::Loading,
            config.map_err(|error| error.to_string()),
            observed_at_ms,
            RecoveryAction::Retry,
        ),
        proxies: Observation::record(
            &Observation::Loading,
            proxies.map_err(|error| error.to_string()),
            observed_at_ms,
            RecoveryAction::Retry,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, net::TcpListener, thread};
    use zenclash_core::MihomoEndpoint;

    #[tokio::test]
    async fn dashboard_keeps_successful_config_when_proxy_catalog_fails() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let request =
                    zenclash_core::test_support::read_controller_request_blocking(&mut stream)
                        .unwrap();
                let request = request.headers();
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap();
                let (status, body) = match path {
                    "/configs" => ("200 OK", r#"{"mode":"rule"}"#),
                    "/proxies" => ("503 Service Unavailable", r#"{"message":"busy"}"#),
                    _ => ("404 Not Found", r#"{"message":"missing"}"#),
                };
                write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            }
        });
        let client =
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

        let data = load(client).await.unwrap();
        server.join().unwrap();

        let PageSnapshotData::Dashboard { config, proxies } = data else {
            panic!("expected dashboard data");
        };
        assert_eq!(
            config.value().map(|config| config.mode.as_str()),
            Some("rule")
        );
        assert!(matches!(proxies, Observation::Failed { .. }));
    }
}
