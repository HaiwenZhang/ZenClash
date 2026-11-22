use std::time::Duration;

use tokio::io::AsyncWriteExt;
use zenclash_core::{ControlledConfigStore, CoreKind, MihomoClient, MihomoEndpoint};

use super::*;

#[test]
fn outbound_mode_parses_mihomo_values_case_insensitively() {
    assert_eq!(OutboundMode::from_api("GLOBAL"), OutboundMode::Global);
}

#[test]
fn outbound_mode_defaults_unknown_values_to_rule() {
    assert_eq!(OutboundMode::from_api("unexpected"), OutboundMode::Rule);
}

#[tokio::test]
async fn mode_sync_publishes_fresh_facts_but_discards_changed_configuration_or_runtime() {
    for (change_config, replace_runtime) in [(false, false), (true, false), (false, true)] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = MihomoClient::new(MihomoEndpoint::new(
            format!("http://{}", listener.local_addr().unwrap()),
            "",
        ))
        .unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
        let mode = OutboundModeCoordinator::new_unsynchronized(OutboundMode::Rule);
        let logs = LogMonitor::start(
            &tokio::runtime::Handle::current(),
            MihomoEndpoint::new("http://127.0.0.1:1", ""),
            MihomoLogLevel::Info,
        );
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut old, _) = listener.accept().await.unwrap();
            zenclash_core::test_support::read_controller_request(&mut old)
                .await
                .unwrap();
            entered.send(()).unwrap();
            let old_reply = tokio::spawn(async move {
                released.await.unwrap();
                let body = r#"{"mode":"direct","log-level":"warning"}"#;
                old.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            });
            if change_config {
                for method in ["PATCH", "GET"] {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let (headers, _) =
                        zenclash_core::test_support::read_controller_request(&mut stream)
                            .await
                            .unwrap()
                            .into_parts();
                    assert!(headers.starts_with(method));
                    let response = if method == "PATCH" {
                        "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned()
                    } else {
                        let body = r#"{"mode":"global","log-level":"debug"}"#;
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    };
                    stream.write_all(response.as_bytes()).await.unwrap();
                }
            }
            old_reply.await.unwrap();
        });
        let sync = tokio::spawn(synchronize_runtime_mode(
            session.clone(),
            mode.clone(),
            logs.clone(),
            mode.intent_generation(),
        ));
        tokio::time::timeout(Duration::from_secs(2), started)
            .await
            .unwrap()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "zenclash-mode-sync-{}-{}",
            std::process::id(),
            listener_port_marker(&session)
        ));
        if change_config {
            session
                .set_mode(&ControlledConfigStore::new(&root), "global")
                .await
                .unwrap();
        }
        if replace_runtime {
            session
                .switch_to_direct(MihomoEndpoint::new("http://127.0.0.1:1", ""))
                .await
                .unwrap();
        }
        if change_config || replace_runtime {
            mode.synchronize(OutboundMode::Global, mode.intent_generation());
            logs.set_level(MihomoLogLevel::Debug);
        }
        release.send(()).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), sync)
            .await
            .unwrap()
            .unwrap();
        if !replace_runtime {
            result.unwrap();
        }
        assert_eq!(
            mode.displayed(),
            if change_config || replace_runtime {
                OutboundMode::Global
            } else {
                OutboundMode::Direct
            }
        );
        assert_eq!(
            logs.level(),
            if change_config || replace_runtime {
                MihomoLogLevel::Debug
            } else {
                MihomoLogLevel::Warning
            }
        );
        server.await.unwrap();
        let _ = std::fs::remove_dir_all(root);
    }
}

fn listener_port_marker(session: &CoreSession) -> String {
    session
        .client()
        .endpoint()
        .unwrap()
        .controller
        .replace(':', "-")
}
