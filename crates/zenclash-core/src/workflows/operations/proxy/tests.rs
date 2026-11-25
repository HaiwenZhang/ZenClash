use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

use crate::{MihomoEndpoint, MihomoError};

use super::*;

#[tokio::test]
async fn workspace_observation_keeps_catalog_and_mode_bound_through_publication() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let request = crate::test_support::read_controller_request_blocking(&mut stream)
                .unwrap()
                .into_bytes();
            let count = request.len();
            let request = String::from_utf8_lossy(&request[..count]);
            let body = if request.starts_with("GET /configs ") {
                r#"{"mode":"global"}"#
            } else {
                assert!(request.starts_with("GET /proxies "));
                r#"{"proxies":{"DIRECT":{"type":"Direct"},"GLOBAL":{"type":"Selector","now":"DIRECT","all":["DIRECT"]}}}"#
            };
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let observation = ProxyOperations::new(client.clone())
        .observe_workspace(ProxyVisibility::VisibleOnly)
        .await
        .unwrap();
    server.join().unwrap();
    assert_eq!(observation.outbound_mode(), "global");
    assert_eq!(
        observation.catalog().groups()[0].controller_name(),
        "GLOBAL"
    );
    client
        .switch_to_direct(MihomoEndpoint::new("http://127.0.0.1:9", ""))
        .await
        .unwrap();
    let mut published = false;
    assert!(
        observation
            .publish_if_current(|_, _| published = true)
            .is_err()
    );
    assert!(!published);
}

#[tokio::test]
async fn accepted_handle_uses_controller_key_and_publishes_actual_behavior() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        let body = r#"{"now":"Actual","type":"URLTest","fixed":false}"#;
        for response in [
            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned(),
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let request = crate::test_support::read_controller_request_blocking(&mut stream)
                .unwrap()
                .into_bytes();
            let count = request.len();
            requests.push(String::from_utf8_lossy(&request[..count]).to_string());
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let accepted = ProxyOperations::new(client)
        .accept_selection("wire/key", "Requested", ConnectionPolicy::KeepExisting)
        .await
        .unwrap();
    assert_eq!(accepted.group_key(), "wire/key");
    assert_eq!(accepted.requested_member(), "Requested");
    let mut catalog = ProxyCatalog::from_group_nodes(
        vec![(
            crate::ProxyGroup::new("wire/key")
                .with_display_name("Visible label")
                .with_current_member("Previous")
                .with_behavior(crate::ProxyGroupBehavior::Automatic { fixed: true }),
            Vec::new(),
        )],
        0,
    );
    assert_eq!(catalog.groups()[0].now, "Previous");
    let ProxySelectionReadback::Observed(observed) = accepted.read_actual().await else {
        panic!("missing actual observation");
    };
    assert_eq!(observed.member(), "Actual");
    assert!(observed.apply_to(&mut catalog));
    assert_eq!(catalog.groups()[0].name, "Visible label");
    assert_eq!(catalog.groups()[0].now, "Actual");
    assert_eq!(
        catalog.groups()[0].behavior,
        crate::ProxyGroupBehavior::Automatic { fixed: false }
    );
    let requests = server.join().unwrap();
    assert!(requests[0].starts_with("PUT /proxies/wire%2Fkey "));
    assert!(requests[1].starts_with("GET /proxies/wire%2Fkey "));
}

#[tokio::test]
async fn accepted_handle_does_not_read_from_a_replacement_runtime() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0; 4096];
        let _ = stream.read(&mut request).unwrap();
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .unwrap();
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let accepted = ProxyOperations::new(client.clone())
        .accept_selection("Proxy", "Requested", ConnectionPolicy::KeepExisting)
        .await
        .unwrap();
    server.join().unwrap();
    let replacement = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    client
        .switch_to_direct(MihomoEndpoint::new(
            format!("http://{}", replacement.local_addr().unwrap()),
            "",
        ))
        .await
        .unwrap();
    assert!(matches!(
        accepted.read_actual().await,
        ProxySelectionReadback::RuntimeChanged
    ));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), replacement.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn captured_observation_cannot_publish_after_its_runtime_changes() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let body = r#"{"now":"Actual"}"#;
        for response in [
            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned(),
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request).unwrap();
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let accepted = ProxyOperations::new(client.clone())
        .accept_selection("Proxy", "Requested", ConnectionPolicy::KeepExisting)
        .await
        .unwrap();
    let ProxySelectionReadback::Observed(observed) = accepted.read_actual().await else {
        panic!("missing actual observation");
    };
    server.join().unwrap();
    client
        .switch_to_direct(MihomoEndpoint::new("http://127.0.0.1:9", ""))
        .await
        .unwrap();
    let mut catalog = ProxyCatalog::from_group_nodes(
        vec![(
            crate::ProxyGroup::new("Proxy").with_current_member("Previous"),
            Vec::new(),
        )],
        0,
    );
    assert!(!observed.apply_to(&mut catalog));
    assert_eq!(catalog.groups()[0].now, "Previous");
}

#[tokio::test]
async fn selection_cleanup_and_catalog_cannot_move_to_a_replacement_binding() {
    use tokio::io::AsyncWriteExt;
    let original = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let original_url = format!("http://{}", original.local_addr().unwrap());
    let replacement = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let replacement_url = format!("http://{}", replacement.local_addr().unwrap());
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = original.accept().await.unwrap();
        let request = crate::test_support::read_controller_request(&mut stream)
            .await
            .unwrap()
            .into_bytes();
        assert!(String::from_utf8_lossy(&request).starts_with("PUT /proxies/Proxy "));
        entered_tx.send(()).unwrap();
        release_rx.await.unwrap();
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
    });
    let client = MihomoClient::new(MihomoEndpoint::new(original_url, "")).unwrap();
    let operations = ProxyOperations::new(client.clone());
    let selecting = tokio::spawn(async move {
        operations
            .select("Proxy", "Requested", ConnectionPolicy::RebuildAll)
            .await
    });
    entered_rx.await.unwrap();
    let (switching_tx, switching_rx) = tokio::sync::oneshot::channel();
    let switching = tokio::spawn(async move {
        switching_tx.send(()).unwrap();
        client
            .switch_to_direct(MihomoEndpoint::new(replacement_url, ""))
            .await
    });
    switching_rx.await.unwrap();
    tokio::task::yield_now().await;
    release_tx.send(()).unwrap();
    server.await.unwrap();
    switching.await.unwrap().unwrap();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), selecting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(outcome.actual.is_none());
    assert!(outcome.catalog.is_none());
    assert!(!outcome.warnings.is_empty());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), replacement.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn rebuild_all_selection_survives_cleanup_failure_and_uses_readback_truth() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in [
            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned(),
            "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: 20\r\nConnection: close\r\n\r\n{\"message\":\"busy\"}".to_owned(),
            proxy_catalog_response(),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let request = crate::test_support::read_controller_request_blocking(&mut stream).unwrap().into_bytes();
let bytes = request.len();
            requests.push(String::from_utf8_lossy(&request[..bytes]).into_owned());
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    let outcome = ProxyOperations::new(client)
        .select("Proxy", "HK 01", ConnectionPolicy::RebuildAll)
        .await
        .unwrap();
    let requests = server.join().unwrap();

    assert_eq!(outcome.actual.as_deref(), Some("HK 01"));
    assert_eq!(outcome.warnings.len(), 1);
    assert!(requests[0].starts_with("PUT /proxies/Proxy "));
    assert!(requests[1].starts_with("DELETE /connections "));
    assert!(requests[2].starts_with("GET /proxies "));
}

#[tokio::test]
async fn keep_existing_selection_does_not_close_connections() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in [
            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned(),
            proxy_catalog_response(),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let request = crate::test_support::read_controller_request_blocking(&mut stream)
                .unwrap()
                .into_bytes();
            let bytes = request.len();
            requests.push(String::from_utf8_lossy(&request[..bytes]).into_owned());
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    ProxyOperations::new(client)
        .select("Proxy", "HK 01", ConnectionPolicy::default())
        .await
        .unwrap();
    let requests = server.join().unwrap();

    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("PUT /proxies/Proxy "));
    assert!(requests[1].starts_with("GET /proxies "));
}

#[tokio::test]
async fn apply_selection_returns_after_ack_without_waiting_for_catalog_readback() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = crate::test_support::read_controller_request_blocking(&mut stream)
            .unwrap()
            .into_bytes();
        let bytes = request.len();
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .unwrap();
        String::from_utf8_lossy(&request[..bytes]).into_owned()
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    let receipt = ProxyOperations::new(client)
        .apply_selection("Proxy", "HK 01", ConnectionPolicy::KeepExisting)
        .await
        .unwrap();
    let request = server.join().unwrap();

    assert!(receipt.warnings.is_empty());
    assert!(request.starts_with("PUT /proxies/Proxy "));
}

#[tokio::test]
async fn rebuild_affected_closes_only_the_changed_groups_previous_member_connections() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in [
            proxy_catalog_with_current("Old Node"),
            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned(),
            connections_response(),
            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned(),
            proxy_catalog_with_current("New Node"),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let request = crate::test_support::read_controller_request_blocking(&mut stream)
                .unwrap()
                .into_bytes();
            let bytes = request.len();
            requests.push(String::from_utf8_lossy(&request[..bytes]).into_owned());
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    let outcome = ProxyOperations::new(client)
        .select("Proxy", "New Node", ConnectionPolicy::RebuildAffected)
        .await
        .unwrap();
    let requests = server.join().unwrap();

    assert_eq!(outcome.actual.as_deref(), Some("New Node"));
    assert_eq!(requests.len(), 5);
    assert!(requests[0].starts_with("GET /proxies "));
    assert!(requests[1].starts_with("PUT /proxies/Proxy "));
    assert!(requests[2].starts_with("GET /connections "));
    assert!(requests[3].starts_with("DELETE /connections/affected "));
    assert!(requests[4].starts_with("GET /proxies "));
    assert!(requests.iter().all(
        |request| !request.starts_with("DELETE /connections/unrelated ")
            && !request.starts_with("DELETE /connections/shared ")
    ));
}

#[tokio::test]
async fn selection_rejection_remains_a_hard_error() {
    let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();

    let error = ProxyOperations::new(client)
        .select("", "HK 01", ConnectionPolicy::KeepExisting)
        .await
        .unwrap_err();

    assert!(matches!(error, MihomoError::InvalidInput(_)));
}

#[tokio::test]
async fn visible_catalog_filters_hidden_groups_by_default() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = crate::test_support::read_controller_request_blocking(&mut stream)
            .unwrap()
            .into_bytes();
        let bytes = request.len();
        let request = String::from_utf8_lossy(&request[..bytes]).into_owned();
        stream
            .write_all(hidden_proxy_catalog_response().as_bytes())
            .unwrap();
        request
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    let catalog = ProxyOperations::new(client)
        .catalog(ProxyVisibility::default())
        .await
        .unwrap();
    let request = server.join().unwrap();

    assert!(request.starts_with("GET /proxies "));
    assert_eq!(catalog.groups().len(), 1);
    assert_eq!(catalog.groups()[0].name, "Proxy");
}

#[tokio::test]
async fn advanced_catalog_preserves_hidden_groups() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4_096];
        let bytes = stream.read(&mut request).unwrap();
        stream
            .write_all(hidden_proxy_catalog_response().as_bytes())
            .unwrap();
        String::from_utf8_lossy(&request[..bytes]).into_owned()
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    let catalog = ProxyOperations::new(client)
        .catalog(ProxyVisibility::IncludeHidden)
        .await
        .unwrap();
    server.join().unwrap();

    assert_eq!(catalog.groups().len(), 2);
    assert!(catalog.groups().iter().any(|group| group.hidden));
}

#[tokio::test]
async fn restore_auto_deletes_group_and_reads_back_automatic_state() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in [
            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned(),
            automatic_proxy_catalog_response(),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let request = crate::test_support::read_controller_request_blocking(&mut stream)
                .unwrap()
                .into_bytes();
            let bytes = request.len();
            requests.push(String::from_utf8_lossy(&request[..bytes]).into_owned());
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    let outcome = ProxyOperations::new(client)
        .restore_auto("Auto Group")
        .await
        .unwrap();
    let requests = server.join().unwrap();

    assert!(requests[0].starts_with("DELETE /proxies/Auto%20Group "));
    assert!(requests[1].starts_with("GET /proxies "));
    assert_eq!(outcome.actual.as_deref(), Some("HK 01"));
    assert_eq!(
        outcome.catalog.unwrap().groups()[0].behavior,
        crate::ProxyGroupBehavior::Automatic { fixed: false }
    );
}

#[tokio::test]
async fn group_measurement_names_restore_side_effect_and_reads_back_state() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in [group_delay_response(), automatic_proxy_catalog_response()] {
            let (mut stream, _) = listener.accept().unwrap();
            let request = crate::test_support::read_controller_request_blocking(&mut stream)
                .unwrap()
                .into_bytes();
            let bytes = request.len();
            requests.push(String::from_utf8_lossy(&request[..bytes]).into_owned());
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    let outcome = ProxyOperations::new(client)
        .measure_group_and_restore_auto(
            "Auto Group",
            Some("https://www.gstatic.com/generate_204"),
            5_000,
        )
        .await
        .unwrap();
    let requests = server.join().unwrap();

    assert!(requests[0].starts_with("GET /group/Auto%20Group/delay?"));
    assert!(requests[0].contains("timeout=5000"));
    assert!(requests[1].starts_with("GET /proxies "));
    assert_eq!(outcome.delays.get("HK 01"), Some(&42));
    assert_eq!(
        outcome.selection.catalog.unwrap().groups()[0].behavior,
        crate::ProxyGroupBehavior::Automatic { fixed: false }
    );
}

#[tokio::test]
async fn selector_group_measurement_uses_one_group_request_without_catalog_readback() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = crate::test_support::read_controller_request_blocking(&mut stream)
            .unwrap()
            .into_bytes();
        let bytes = request.len();
        stream.write_all(group_delay_response().as_bytes()).unwrap();
        String::from_utf8_lossy(&request[..bytes]).into_owned()
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    let delays = ProxyOperations::new(client)
        .measure_group("Proxy", Some("https://www.gstatic.com/generate_204"), 5_000)
        .await
        .unwrap();
    let request = server.join().unwrap();

    assert_eq!(
        (
            delays.get("HK 01"),
            request.starts_with("GET /group/Proxy/delay?")
        ),
        (Some(&42), true)
    );
}

#[tokio::test]
async fn individual_measurement_never_calls_the_group_restore_endpoint() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = crate::test_support::read_controller_request_blocking(&mut stream)
            .unwrap()
            .into_bytes();
        let bytes = request.len();
        let request = String::from_utf8_lossy(&request[..bytes]).into_owned();
        let body = r#"{"delay":42}"#;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
        request
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

    let delay = ProxyOperations::new(client)
        .measure(
            &ProxyDelayTarget {
                name: "HK 01".into(),
                provider: None,
            },
            Some("https://www.gstatic.com/generate_204"),
            5_000,
        )
        .await
        .unwrap();
    let request = server.join().unwrap();

    assert_eq!(delay.delay, 42);
    assert!(request.starts_with("GET /proxies/HK%2001/delay?"));
    assert!(!request.contains("/group/"));
}

#[tokio::test]
async fn cloned_clients_share_measurement_budget_and_cancellation_releases_it() {
    use tokio::{
        io::AsyncWriteExt,
        time::{Duration, timeout},
    };

    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let client = MihomoClient::new(MihomoEndpoint::new(
        format!("http://{}", listener.local_addr().unwrap()),
        "",
    ))
    .unwrap();
    let reserved = client.delay_gate.acquire_many(15).await.unwrap();
    let start = || {
        let operations = ProxyOperations::new(client.clone());
        tokio::spawn(async move {
            operations
                .measure(
                    &ProxyDelayTarget {
                        name: "node".into(),
                        provider: None,
                    },
                    None,
                    5_000,
                )
                .await
        })
    };
    let first = start();
    let (_first_socket, _) = timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let second = start();
    assert!(
        timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let (mut socket, _) = timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 12\r\nConnection: close\r\n\r\n{\"delay\":42}").await.unwrap();
    assert_eq!(
        timeout(Duration::from_secs(2), second)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .delay,
        42
    );
    drop(reserved);
    assert_eq!(client.delay_gate.available_permits(), 16);
}

fn proxy_catalog_response() -> String {
    let body = r#"{"proxies":{"Proxy":{"name":"Proxy","type":"Selector","now":"HK 01","all":["HK 01"]},"HK 01":{"name":"HK 01","type":"Shadowsocks"}}}"#;
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn hidden_proxy_catalog_response() -> String {
    let body = r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Proxy":{"name":"Proxy","type":"Selector","now":"DIRECT","all":["DIRECT"]},"Internal":{"name":"Internal","type":"Selector","now":"DIRECT","all":["DIRECT"],"hidden":true}}}"#;
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn automatic_proxy_catalog_response() -> String {
    let body = r#"{"proxies":{"HK 01":{"name":"HK 01","type":"Shadowsocks"},"Auto Group":{"name":"Auto Group","type":"URLTest","now":"HK 01","all":["HK 01"],"fixed":false}}}"#;
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn group_delay_response() -> String {
    let body = r#"{"HK 01":42}"#;
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn proxy_catalog_with_current(current: &str) -> String {
    let body = format!(
        r#"{{"proxies":{{"Old Node":{{"name":"Old Node","type":"Shadowsocks"}},"New Node":{{"name":"New Node","type":"Shadowsocks"}},"Proxy":{{"name":"Proxy","type":"Selector","now":"{current}","all":["Old Node","New Node"]}}}}}}"#
    );
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn connections_response() -> String {
    let body = r#"{"connections":[{"id":"affected","chains":["Proxy","Old Node"]},{"id":"unrelated","chains":["Proxy","Other Node"]},{"id":"shared","chains":["Other Group","Old Node"]}]}"#;
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
