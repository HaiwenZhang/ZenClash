//! Controller read policy owned by this capability.
//!
//! The ordinary page reader supplies the original pinned client and verifies it
//! again after this policy returns. The payload grants no publication eligibility.

use crate::pages::runtime::PageSnapshotData;
use zenclash_core::MihomoClient;

pub(in crate::pages::runtime) async fn load(
    client: MihomoClient,
) -> Result<PageSnapshotData, String> {
    let catalog = client
        .rule_catalog()
        .await
        .map_err(|error| error.to_string())?;
    let (config, proxies) = tokio::join!(client.runtime_config(), client.proxy_catalog());
    Ok(PageSnapshotData::Rules {
        catalog: std::sync::Arc::new(catalog),
        config: config.ok(),
        proxies: proxies.ok(),
    })
}
#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn rules_read_runtime_mode_without_discarding_catalog_on_config_failure() {
        use tokio::io::AsyncWriteExt;

        for config_available in [true, false] {
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            let client = super::MihomoClient::new(zenclash_core::MihomoEndpoint::new(
                format!("http://{}", listener.local_addr().unwrap()),
                "",
            ))
            .unwrap();
            let server = tokio::spawn(async move {
                for _ in 0..3 {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let request = zenclash_core::test_support::read_controller_request(&mut stream)
                        .await
                        .unwrap()
                        .into_bytes();
                    let request = String::from_utf8(request).unwrap();
                    let expected_path = request.split_whitespace().nth(1).unwrap();
                    assert!(["/rules", "/configs", "/proxies"].contains(&expected_path));
                    let (status, body) = if expected_path == "/rules" {
                        (
                            "200 OK",
                            r#"{"rules":[{"type":"MATCH","payload":"","proxy":"DIRECT"}]}"#,
                        )
                    } else if expected_path == "/proxies" {
                        ("500 Internal Server Error", "{}")
                    } else if config_available {
                        ("200 OK", r#"{"mode":"global","mixed-port":7890}"#)
                    } else {
                        ("500 Internal Server Error", "{}")
                    };
                    stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                }
            });
            let data = crate::pages::runtime::load_page(client, crate::pages::Page::Rules)
                .await
                .unwrap();
            let super::PageSnapshotData::Rules {
                catalog,
                config,
                proxies,
            } = data
            else {
                panic!("expected rules")
            };
            assert_eq!(catalog.rules.len(), 1);
            assert!(proxies.is_none());
            assert_eq!(
                config.as_ref().map(|config| config.mode.as_str()),
                config_available.then_some("global")
            );
            server.await.unwrap();
        }
    }
}
