use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

use zenclash_core::{
    ConnectionPolicy, MihomoClient, MihomoEndpoint, ProxyCatalog, ProxyGroup, ProxyOperations,
};

use super::ProxySelectionUiState;

fn controller(responses: Vec<String>) -> (ProxyOperations, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request).unwrap();
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    (ProxyOperations::new(client), server)
}

fn acknowledgement() -> String {
    "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".into()
}
fn observation(member: &str) -> String {
    let body = serde_json::json!({"now":member,"type":"Selector"}).to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn later_same_group_request_supersedes_an_older_actual_observation() {
    let (operations, server) = controller(vec![
        acknowledgement(),
        acknowledgement(),
        observation("Second actual"),
        observation("First actual"),
    ]);
    let mut state = ProxySelectionUiState::default();
    let mut catalog = ProxyCatalog::from_group_nodes(
        vec![(
            ProxyGroup::new("Proxy").with_current_member("Previous"),
            Vec::new(),
        )],
        0,
    );
    let first = state.start("Proxy".into(), "First request".into()).unwrap();
    let first_accepted = operations
        .accept_selection("Proxy", "First request", ConnectionPolicy::KeepExisting)
        .await
        .unwrap();
    assert!(state.acknowledge(&first));
    let second = state
        .start("Proxy".into(), "Second request".into())
        .unwrap();
    let second_accepted = operations
        .accept_selection("Proxy", "Second request", ConnectionPolicy::KeepExisting)
        .await
        .unwrap();
    assert!(state.acknowledge(&second));
    assert!(state.observe(
        &second,
        &second_accepted,
        second_accepted.read_actual().await,
        Some(&mut catalog)
    ));
    assert!(!state.observe(
        &first,
        &first_accepted,
        first_accepted.read_actual().await,
        Some(&mut catalog)
    ));
    server.join().unwrap();
    assert_eq!(catalog.groups()[0].now, "Second actual");
}

#[tokio::test]
async fn different_groups_accept_their_actual_observations_in_reverse_order() {
    let (operations, server) = controller(vec![
        acknowledgement(),
        acknowledgement(),
        observation("Second actual"),
        observation("First actual"),
    ]);
    let mut state = ProxySelectionUiState::default();
    let mut catalog = ProxyCatalog::from_group_nodes(
        ["First", "Second"]
            .into_iter()
            .map(|key| {
                (
                    ProxyGroup::new(key)
                        .with_display_name("Same label")
                        .with_current_member("Previous"),
                    Vec::new(),
                )
            })
            .collect(),
        0,
    );
    let first = state.start("First".into(), "First request".into()).unwrap();
    let first_accepted = operations
        .accept_selection("First", "First request", ConnectionPolicy::KeepExisting)
        .await
        .unwrap();
    assert!(state.acknowledge(&first));
    let second = state
        .start("Second".into(), "Second request".into())
        .unwrap();
    let second_accepted = operations
        .accept_selection("Second", "Second request", ConnectionPolicy::KeepExisting)
        .await
        .unwrap();
    assert!(state.acknowledge(&second));
    assert!(state.observe(
        &second,
        &second_accepted,
        second_accepted.read_actual().await,
        Some(&mut catalog)
    ));
    assert!(state.observe(
        &first,
        &first_accepted,
        first_accepted.read_actual().await,
        Some(&mut catalog)
    ));
    server.join().unwrap();
    assert_eq!(catalog.groups()[0].now, "First actual");
    assert_eq!(catalog.groups()[1].now, "Second actual");
}

#[tokio::test]
async fn failed_readback_preserves_last_observed_member_and_allows_another_request() {
    let (operations, server) = controller(vec![
        acknowledgement(),
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
    ]);
    let mut state = ProxySelectionUiState::default();
    let mut catalog = ProxyCatalog::from_group_nodes(
        vec![(
            ProxyGroup::new("Proxy").with_current_member("Previous"),
            Vec::new(),
        )],
        0,
    );
    let request = state.start("Proxy".into(), "Requested".into()).unwrap();
    let accepted = operations
        .accept_selection("Proxy", "Requested", ConnectionPolicy::KeepExisting)
        .await
        .unwrap();
    state.acknowledge(&request);
    assert!(state.observe(
        &request,
        &accepted,
        accepted.read_actual().await,
        Some(&mut catalog)
    ));
    server.join().unwrap();
    assert_eq!(catalog.groups()[0].now, "Previous");
    assert!(state.status_text("Proxy").is_some());
    assert!(state.start("Proxy".into(), "Retry".into()).is_some());
}
