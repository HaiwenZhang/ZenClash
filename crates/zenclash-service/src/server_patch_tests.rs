//! Functional HTTP fixtures; these do not prove native peer identity or real Mihomo behavior.

use super::*;
use crate::installer::OwnedTestRoot;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

enum Reply {
    Json(Value),
    Empty,
    Lost,
}

type Exchange = (&'static str, Option<Value>, Reply);

// Fixed v1.19.30 contract: tunSchema.Enable is a value bool, other
// fields are optional pointers; GET omits the three false booleans below.
fn tun_controller(
    initial: Value,
    exchanges: usize,
    lost_patch_reply: bool,
) -> (
    Kernel,
    tokio::task::JoinHandle<Vec<String>>,
    Arc<std::sync::Mutex<Value>>,
) {
    let live = Arc::new(std::sync::Mutex::new(initial));
    let observed = live.clone();
    let mut clients = Vec::new();
    let mut servers = Vec::new();
    for _ in 0..exchanges {
        let (client, server) = tokio::io::duplex(8192);
        clients.push(client);
        servers.push(server);
    }
    let task = tokio::spawn(async move {
        let mut methods = Vec::new();
        let mut lose_reply = lost_patch_reply;
        for mut server in servers {
            let (line, body) =
                tokio::time::timeout(Duration::from_secs(5), read_request(&mut server))
                    .await
                    .unwrap();
            let method = line.split_whitespace().next().unwrap();
            methods.push(method.to_owned());
            let response = {
                let mut config = observed.lock().unwrap();
                if method == "PATCH" {
                    let patch = body.unwrap();
                    let tun = patch["tun"].as_object().unwrap();
                    let target = config["tun"].as_object_mut().unwrap();
                    target.insert(
                        "enable".into(),
                        json!(tun.get("enable").and_then(Value::as_bool).unwrap_or(false)),
                    );
                    for (key, value) in tun {
                        target.insert(key.clone(), value.clone());
                    }
                    if let Some(values) = target.get_mut("dns-hijack").and_then(Value::as_array_mut)
                    {
                        values.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
                    }
                    None
                } else {
                    assert_eq!(method, "GET");
                    let mut response = config.clone();
                    let tun = response["tun"].as_object_mut().unwrap();
                    for key in ["gso", "auto-redirect", "strict-route"] {
                        if tun.get(key) == Some(&json!(false)) {
                            tun.remove(key);
                        }
                    }
                    if tun.get("mtu") == Some(&json!(0)) {
                        tun.remove("mtu");
                    }
                    Some(response)
                }
            };
            if let Some(response) = response {
                let bytes = serde_json::to_vec(&response).unwrap();
                server
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            bytes.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                server.write_all(&bytes).await.unwrap();
            } else if lose_reply {
                lose_reply = false;
            } else {
                server
                    .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                    .await
                    .unwrap();
            }
        }
        methods
    });
    (Kernel::fixture(clients, 1717), task, live)
}

#[tokio::test]
async fn tun_partial_preserves_fresh_enable_through_lost_reply_and_inverse_patch() {
    let (kernel, requests, live) =
        tun_controller(json!({"tun":{"enable":true,"mtu":1400}}), 5, true);
    let (mut state, root, _) = state(kernel);
    let requested = json!({"tun":{"mtu":1500}});
    let revision = prepare(&mut state, requested.clone()).await;
    let formal: serde_yaml::Value =
        serde_yaml::from_slice(&std::fs::read(candidate_path(&state)).unwrap()).unwrap();
    state
        .operate(SessionOperation::ApplyRuntimePatch { revision })
        .await
        .unwrap();
    assert_eq!(
        live.lock().unwrap()["tun"],
        json!({"enable":true,"mtu":1500})
    );
    assert_eq!(formal["tun"]["enable"], true);
    assert_eq!(prepare(&mut state, requested).await, revision);
    state
        .operate(SessionOperation::RestoreRuntimePatch { revision })
        .await
        .unwrap();
    assert_eq!(
        live.lock().unwrap()["tun"],
        json!({"enable":true,"mtu":1400})
    );
    assert_eq!(
        finish(state, root, requests).await,
        ["GET", "PATCH", "GET", "PATCH", "GET"]
    );
}

#[tokio::test]
async fn disabled_tun_omitted_zero_mtu_can_be_restored_without_public_zero_request() {
    let (kernel, requests, live) =
        tun_controller(json!({"tun":{"enable":false,"mtu":0}}), 5, false);
    let (mut state, root, _) = state(kernel);
    assert!(matches!(
        state
            .operate(SessionOperation::PrepareRuntimePatch {
                base_revision: 1,
                patch: json!({"tun":{"mtu":0}}),
            })
            .await,
        Err(ServiceErrorCode::InvalidConfiguration)
    ));
    let revision = prepare(&mut state, json!({"tun":{"mtu":1500}})).await;
    state
        .operate(SessionOperation::ApplyRuntimePatch { revision })
        .await
        .unwrap();
    assert_eq!(
        live.lock().unwrap()["tun"],
        json!({"enable":false,"mtu":1500})
    );
    state
        .operate(SessionOperation::RestoreRuntimePatch { revision })
        .await
        .unwrap();
    assert_eq!(live.lock().unwrap()["tun"], json!({"enable":false,"mtu":0}));
    assert_eq!(
        state.staged.as_ref().unwrap().patch.as_ref().unwrap().phase,
        RuntimeCandidatePhase::Prepared
    );
    assert_eq!(
        finish(state, root, requests).await,
        ["GET", "PATCH", "GET", "PATCH", "GET"]
    );
}

#[tokio::test]
async fn tun_omitted_false_and_null_dns_are_confirmed_and_restored() {
    let (kernel, requests, live) = tun_controller(
        json!({"tun":{"enable":true,"gso":false,"auto-redirect":false,"strict-route":false,"dns-hijack":null}}),
        5,
        false,
    );
    let (mut state, root, _) = state(kernel);
    let revision = prepare(&mut state, json!({"tun":{"gso":true,"auto-redirect":true,"strict-route":true,"dns-hijack":["udp://9.9.9.9:53","any:53"]}})).await;
    state
        .operate(SessionOperation::ApplyRuntimePatch { revision })
        .await
        .unwrap();
    assert_eq!(live.lock().unwrap()["tun"]["enable"], true);
    state
        .operate(SessionOperation::RestoreRuntimePatch { revision })
        .await
        .unwrap();
    assert_eq!(
        state.staged.as_ref().unwrap().patch.as_ref().unwrap().phase,
        RuntimeCandidatePhase::Prepared
    );
    assert_eq!(live.lock().unwrap()["tun"]["strict-route"], false);
    assert_eq!(live.lock().unwrap()["tun"]["dns-hijack"], json!([]));
    assert_eq!(
        finish(state, root, requests).await,
        ["GET", "PATCH", "GET", "PATCH", "GET"]
    );
}

#[tokio::test]
async fn missing_fresh_tun_enable_rejects_before_staging_or_patch() {
    let (kernel, requests) = controller(vec![(
        "GET",
        None,
        Reply::Json(json!({"tun":{"mtu":1400}})),
    )]);
    let (mut state, root, _) = state(kernel);
    assert!(matches!(
        state
            .operate(SessionOperation::PrepareRuntimePatch {
                base_revision: 1,
                patch: json!({"tun":{"mtu":1500}}),
            })
            .await,
        Err(ServiceErrorCode::KernelFailed)
    ));
    assert!(state.staged.is_none());
    assert_eq!(finish(state, root, requests).await, ["GET"]);
}

#[tokio::test]
async fn full_reload_only_fields_reject_partial_without_controller_requests() {
    let (kernel, requests) = controller(vec![]);
    let (mut state, root, _) = state(kernel);
    for (key, value) in [
        ("unified-delay", json!(true)),
        ("inbound-tfo", json!(true)),
        ("inbound-mptcp", json!(true)),
        ("disable-keep-alive", json!(true)),
        ("keep-alive-interval", json!(30)),
        ("keep-alive-idle", json!(30)),
        ("routing-mark", json!(2)),
    ] {
        let patch = Value::Object([(key.to_owned(), value)].into_iter().collect());
        assert!(
            matches!(
                state
                    .operate(SessionOperation::PrepareRuntimePatch {
                        base_revision: 1,
                        patch
                    })
                    .await,
                Err(ServiceErrorCode::InvalidConfiguration)
            ),
            "{key}"
        );
        assert!(state.staged.is_none());
    }
    assert!(finish(state, root, requests).await.is_empty());
}

#[tokio::test]
async fn full_reload_preserves_fields_not_supported_by_native_partial_patch() {
    let full = json!({
        "unified-delay":true, "inbound-tfo":true, "inbound-mptcp":true,
        "disable-keep-alive":true, "keep-alive-interval":30,
        "keep-alive-idle":30, "routing-mark":2,
    });
    let expected = full.clone();
    let (client, mut server) = tokio::io::duplex(8192);
    let requests = tokio::spawn(async move {
        let (line, body) = tokio::time::timeout(Duration::from_secs(5), read_request(&mut server))
            .await
            .unwrap();
        assert_eq!(line, "PUT /configs?force=false HTTP/1.1");
        let body = body.unwrap();
        let payload: serde_yaml::Value =
            serde_yaml::from_str(body["payload"].as_str().unwrap()).unwrap();
        for (key, value) in expected.as_object().unwrap() {
            assert_eq!(payload[key.as_str()], serde_yaml::to_value(value).unwrap());
        }
        server
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        vec!["PUT".to_owned()]
    });
    let (mut state, root, _) = state(Kernel::fixture(vec![client], 1717));
    let revision = match state
        .operate(SessionOperation::Stage {
            config: serde_yaml::to_string(&full).unwrap(),
        })
        .await
        .unwrap()
    {
        Response::Staged { revision } => revision,
        response => panic!("unexpected stage: {response:?}"),
    };
    // Native validation is outside this HTTP fixture; provide its admitted premise.
    state.staged.as_mut().unwrap().validated = true;
    state
        .operate(SessionOperation::Reload {
            revision,
            force: false,
        })
        .await
        .unwrap();
    state
        .operate(SessionOperation::CommitRuntime { revision })
        .await
        .unwrap();
    assert_eq!(state.applied_revision, Some(revision));
    assert_eq!(state.active.as_ref().unwrap().revision, revision);
    assert_eq!(finish(state, root, requests).await, ["PUT"]);
}

#[tokio::test]
async fn protocol_one_hello_is_rejected_before_any_session_acquisition() {
    let shared = Arc::new(Mutex::new(super::tests::state_for_test(
        std::env::temp_dir(),
    )));
    let (mut client, server) = tokio::io::duplex(4096);
    let task = tokio::spawn(connection(
        server,
        PeerIdentity::new("test-user".into(), 1, 1),
        shared.clone(),
    ));
    write_frame(
        &mut client,
        &Request::Hello {
            protocol_version: 1,
        },
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    let response: Response = read_frame(&mut client, Duration::from_secs(2))
        .await
        .unwrap();
    assert!(matches!(
        response,
        Response::Error {
            code: ServiceErrorCode::Incompatible
        }
    ));
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(shared.lock().await.authority.current_proof().is_none());
    let mut byte = [0];
    assert_eq!(client.read(&mut byte).await.unwrap(), 0);
}

fn controller(script: Vec<Exchange>) -> (Kernel, tokio::task::JoinHandle<Vec<String>>) {
    let mut clients = Vec::new();
    let mut servers = Vec::new();
    for _ in &script {
        let (client, server) = tokio::io::duplex(8192);
        clients.push(client);
        servers.push(server);
    }
    let task = tokio::spawn(async move {
        let mut methods = Vec::new();
        for ((method, expected, reply), mut server) in script.into_iter().zip(servers) {
            let (line, body) =
                tokio::time::timeout(Duration::from_secs(5), read_request(&mut server))
                    .await
                    .unwrap();
            assert_eq!(line, format!("{method} /configs HTTP/1.1"));
            assert_eq!(body, expected);
            methods.push(method.to_owned());
            match reply {
                Reply::Json(body) => {
                    let bytes = serde_json::to_vec(&body).unwrap();
                    server.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).as_bytes()).await.unwrap();
                    server.write_all(&bytes).await.unwrap();
                }
                Reply::Empty => server
                    .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                    .await
                    .unwrap(),
                Reply::Lost => {}
            }
        }
        methods
    });
    (Kernel::fixture(clients, 1717), task)
}

async fn read_request(stream: &mut DuplexStream) -> (String, Option<Value>) {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        assert!(bytes.len() < 32 * 1024);
        bytes.push(stream.read_u8().await.unwrap());
    }
    let headers = String::from_utf8(bytes).unwrap();
    let length = headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map_or(0, |(_, length)| length.trim().parse::<usize>().unwrap());
    assert!(length <= 64 * 1024);
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await.unwrap();
    (
        headers.lines().next().unwrap().to_owned(),
        (!bytes.is_empty()).then(|| serde_json::from_slice(&bytes).unwrap()),
    )
}

fn state(kernel: Kernel) -> (State, Arc<OwnedTestRoot>, PathBuf) {
    let root = Arc::new(OwnedTestRoot::create().unwrap());
    let input = root.path().join("snapshots/revision-1");
    let config = root.path().join("configurations/revision-1");
    root.create_directory(&input).unwrap();
    root.create_directory(&config).unwrap();
    root.create_directory(&root.path().join("assets")).unwrap();
    let mut runtime = StagedRuntime::new_with_configuration(
        input,
        config.clone(),
        "mode: rule\ntun:\n  enable: false\nclient-auth-cert: assets/cert.pem\n",
    )
    .unwrap();
    runtime
        .upload("assets/cert.pem", 0, b"held certificate bytes", true)
        .unwrap();
    runtime
        .materialize("fixed-private-controller", "fixed-secret")
        .unwrap();
    let mut state = super::tests::state_for_test(root.path().to_owned());
    state.fixture_root = Some(root.clone());
    state.active = Some(Stage {
        revision: 1,
        runtime,
        validated: true,
        patch: None,
    });
    state.revision = 1;
    state.kernel = Some(kernel);
    (state, root, config.join("runtime.yaml"))
}

async fn finish(
    mut state: State,
    root: Arc<OwnedTestRoot>,
    requests: tokio::task::JoinHandle<Vec<String>>,
) -> Vec<String> {
    let methods = tokio::time::timeout(Duration::from_secs(5), requests)
        .await
        .unwrap()
        .unwrap();
    state.release().await.unwrap();
    drop(state);
    for entry in std::fs::read_dir(root.path()).unwrap() {
        root.remove(&entry.unwrap().path()).unwrap();
    }
    std::fs::remove_dir(root.path()).unwrap();
    methods
}

async fn prepare(state: &mut State, body: Value) -> u64 {
    match state
        .operate(SessionOperation::PrepareRuntimePatch {
            base_revision: 1,
            patch: body,
        })
        .await
        .unwrap()
    {
        Response::RuntimePatchPrepared { prepared } => prepared.revision,
        response => panic!("unexpected preparation: {response:?}"),
    }
}

fn candidate_path(state: &State) -> PathBuf {
    state
        .session_directory
        .as_ref()
        .unwrap()
        .join("configurations")
        .join(format!(
            "revision-{}/runtime.yaml",
            state.staged.as_ref().unwrap().revision
        ))
}

#[tokio::test]
async fn failed_partial_write_retains_one_cleanup_owner_and_can_prepare_again() {
    let (kernel, requests) = controller(vec![
        ("GET", None, Reply::Json(json!({"mode":"rule"}))),
        ("GET", None, Reply::Json(json!({"mode":"rule"}))),
    ]);
    let (mut state, root, original) = state(kernel);
    let before = std::fs::read(&original).unwrap();
    let session = state.session_directory().unwrap();
    let candidate = session.join("configurations/revision-2");
    root.create_directory(&candidate.join("runtime.yaml"))
        .unwrap();
    assert!(matches!(
        state
            .operate(SessionOperation::PrepareRuntimePatch {
                base_revision: 1,
                patch: json!({"mode":"global"}),
            })
            .await,
        Err(ServiceErrorCode::Internal)
    ));
    assert!(state.staged.is_none());
    assert!(state.retired.is_some());
    assert_eq!(std::fs::read(&original).unwrap(), before);
    assert_eq!(prepare(&mut state, json!({"mode":"global"})).await, 2);
    assert!(state.retired.is_none());
    assert!(candidate.join("runtime.yaml").is_file());
    assert_eq!(finish(state, root, requests).await, ["GET", "GET"]);
}

#[tokio::test]
async fn failed_partial_directory_creation_retains_cleanup_owner_for_missing_candidate() {
    let (kernel, requests) = controller(vec![
        ("GET", None, Reply::Json(json!({"mode":"rule"}))),
        ("GET", None, Reply::Json(json!({"mode":"rule"}))),
    ]);
    let (mut state, root, original) = state(kernel);
    let before = std::fs::read(&original).unwrap();
    let session = state.session_directory().unwrap();
    let configurations = session.join("configurations");
    std::fs::remove_dir(&configurations).unwrap();
    std::fs::write(&configurations, b"blocked configuration parent").unwrap();
    assert!(matches!(
        state
            .operate(SessionOperation::PrepareRuntimePatch {
                base_revision: 1,
                patch: json!({"mode":"global"}),
            })
            .await,
        Err(ServiceErrorCode::Internal)
    ));
    assert!(state.staged.is_none());
    assert!(state.retired.is_some());
    assert_eq!(std::fs::read(&original).unwrap(), before);
    std::fs::remove_file(&configurations).unwrap();
    root.create_directory(&configurations).unwrap();
    state.cleanup_retired().unwrap();
    state.cleanup_retired().unwrap();
    assert!(state.retired.is_none());
    assert_eq!(prepare(&mut state, json!({"mode":"global"})).await, 2);
    assert_eq!(finish(state, root, requests).await, ["GET", "GET"]);
}

#[tokio::test]
async fn partial_apply_commits_only_fields_and_reuses_held_assets() {
    let patch = json!({"mode":"global", "tun":{"enable":true}});
    let (kernel, requests) = controller(vec![
        (
            "GET",
            None,
            Reply::Json(json!({"mode":"rule","tun":{"enable":false},"unrelated":1})),
        ),
        ("PATCH", Some(patch.clone()), Reply::Empty),
        (
            "GET",
            None,
            Reply::Json(json!({"mode":"global","tun":{"enable":true},"unrelated":99})),
        ),
    ]);
    let (mut state, root, original) = state(kernel);
    let before: serde_yaml::Value =
        serde_yaml::from_slice(&std::fs::read(&original).unwrap()).unwrap();
    let asset = root.path().join("assets/revision-1/assets/cert.pem");
    std::fs::remove_file(root.path().join("snapshots/revision-1/assets/cert.pem")).unwrap();
    let revision = prepare(&mut state, patch.clone()).await;
    assert_eq!(prepare(&mut state, patch.clone()).await, revision);
    assert!(matches!(
        state
            .operate(SessionOperation::Stage {
                config: "mode: direct".into()
            })
            .await,
        Err(ServiceErrorCode::InvalidRequest)
    ));
    assert!(matches!(
        state
            .operate(SessionOperation::PrepareRuntimePatch {
                base_revision: 1,
                patch: json!({"mode":"direct"})
            })
            .await,
        Err(ServiceErrorCode::InvalidRequest)
    ));
    let candidate = candidate_path(&state);
    let next: serde_yaml::Value =
        serde_yaml::from_slice(&std::fs::read(&candidate).unwrap()).unwrap();
    assert_eq!(next["mode"], "global");
    assert_eq!(next["tun"]["enable"], true);
    assert_eq!(next["client-auth-cert"], before["client-auth-cert"]);
    assert_eq!(next["secret"], before["secret"]);
    assert_eq!(
        state.snapshot().unwrap().candidate.unwrap().phase,
        RuntimeCandidatePhase::Prepared
    );
    state
        .operate(SessionOperation::ApplyRuntimePatch { revision })
        .await
        .unwrap();
    state
        .operate(SessionOperation::ApplyRuntimePatch { revision })
        .await
        .unwrap();
    assert_eq!(prepare(&mut state, patch).await, revision);
    let applied = state.snapshot().unwrap();
    assert_eq!(applied.pid, Some(1717));
    assert_eq!(applied.applied_revision, Some(revision));
    assert_eq!(applied.committed_revision, Some(1));
    state
        .operate(SessionOperation::CommitRuntime { revision })
        .await
        .unwrap();
    state
        .operate(SessionOperation::CommitRuntime { revision })
        .await
        .unwrap();
    assert_eq!(std::fs::read(&asset).unwrap(), b"held certificate bytes");
    assert!(!original.exists());
    assert_eq!(state.snapshot().unwrap().committed_revision, Some(revision));
    assert_eq!(finish(state, root, requests).await, ["GET", "PATCH", "GET"]);
}

#[tokio::test]
async fn lost_apply_response_is_confirmed_by_status_without_resending() {
    let patch = json!({"mode":"global"});
    let (kernel, requests) = controller(vec![
        ("GET", None, Reply::Json(json!({"mode":"rule"}))),
        ("PATCH", Some(patch.clone()), Reply::Lost),
        ("GET", None, Reply::Lost),
        ("GET", None, Reply::Json(json!({"mode":"global"}))),
    ]);
    let (mut state, root, _) = state(kernel);
    let revision = prepare(&mut state, patch).await;
    assert!(matches!(
        state
            .operate(SessionOperation::ApplyRuntimePatch { revision })
            .await,
        Err(ServiceErrorCode::OutcomeUnknown)
    ));
    assert_eq!(
        state.snapshot().unwrap().candidate.unwrap().phase,
        RuntimeCandidatePhase::Uncertain
    );
    assert!(matches!(
        state
            .operate(SessionOperation::CommitRuntime { revision })
            .await,
        Err(ServiceErrorCode::StaleRevision)
    ));
    let Response::Status { snapshot } = state.operate(SessionOperation::Status {}).await.unwrap()
    else {
        panic!("missing status");
    };
    assert_eq!(snapshot.applied_revision, Some(revision));
    assert_eq!(
        snapshot.candidate.unwrap().phase,
        RuntimeCandidatePhase::Applied
    );
    state
        .operate(SessionOperation::ApplyRuntimePatch { revision })
        .await
        .unwrap();
    state
        .operate(SessionOperation::CommitRuntime { revision })
        .await
        .unwrap();
    assert_eq!(
        finish(state, root, requests).await,
        ["GET", "PATCH", "GET", "GET"]
    );
}

#[tokio::test]
async fn unknown_readback_old_fields_never_blindly_reapplies() {
    let patch = json!({"mode":"global"});
    let (kernel, requests) = controller(vec![
        ("GET", None, Reply::Json(json!({"mode":"rule"}))),
        ("PATCH", Some(patch.clone()), Reply::Lost),
        ("GET", None, Reply::Lost),
        ("GET", None, Reply::Json(json!({"mode":"rule"}))),
    ]);
    let (mut state, root, _) = state(kernel);
    let revision = prepare(&mut state, patch).await;
    assert!(matches!(
        state
            .operate(SessionOperation::ApplyRuntimePatch { revision })
            .await,
        Err(ServiceErrorCode::OutcomeUnknown)
    ));
    assert!(matches!(
        state
            .operate(SessionOperation::ApplyRuntimePatch { revision })
            .await,
        Err(ServiceErrorCode::KernelFailed)
    ));
    assert_eq!(state.snapshot().unwrap().applied_revision, Some(1));
    assert_eq!(
        state.snapshot().unwrap().candidate.unwrap().phase,
        RuntimeCandidatePhase::Prepared
    );
    state
        .operate(SessionOperation::RestoreRuntimePatch { revision })
        .await
        .unwrap();
    state
        .operate(SessionOperation::CommitRuntime { revision: 1 })
        .await
        .unwrap();
    assert_eq!(
        finish(state, root, requests).await,
        ["GET", "PATCH", "GET", "GET"]
    );
}

#[tokio::test]
async fn inverse_patch_lost_ack_retries_only_get_and_keeps_original_config() {
    let patch = json!({"mode":"global"});
    let (kernel, requests) = controller(vec![
        ("GET", None, Reply::Json(json!({"mode":"rule"}))),
        ("PATCH", Some(patch.clone()), Reply::Empty),
        ("GET", None, Reply::Json(json!({"mode":"global"}))),
        ("PATCH", Some(json!({"mode":"rule"})), Reply::Lost),
        ("GET", None, Reply::Lost),
        ("GET", None, Reply::Json(json!({"mode":"rule"}))),
    ]);
    let (mut state, root, original) = state(kernel);
    let before = std::fs::read(&original).unwrap();
    let revision = prepare(&mut state, patch).await;
    state
        .operate(SessionOperation::ApplyRuntimePatch { revision })
        .await
        .unwrap();
    assert!(matches!(
        state
            .operate(SessionOperation::RestoreRuntimePatch { revision })
            .await,
        Err(ServiceErrorCode::OutcomeUnknown)
    ));
    state
        .operate(SessionOperation::RestoreRuntimePatch { revision })
        .await
        .unwrap();
    state
        .operate(SessionOperation::RestoreRuntimePatch { revision })
        .await
        .unwrap();
    assert_eq!(state.snapshot().unwrap().applied_revision, Some(1));
    state
        .operate(SessionOperation::CommitRuntime { revision: 1 })
        .await
        .unwrap();
    assert_eq!(std::fs::read(&original).unwrap(), before);
    assert_eq!(
        finish(state, root, requests).await,
        ["GET", "PATCH", "GET", "PATCH", "GET", "GET"]
    );
}

#[tokio::test]
async fn an_applied_partial_blocks_restart_until_business_confirmation() {
    let patch = json!({"mode":"global"});
    let (kernel, requests) = controller(vec![
        ("GET", None, Reply::Json(json!({"mode":"rule"}))),
        ("PATCH", Some(patch.clone()), Reply::Empty),
        ("GET", None, Reply::Json(json!({"mode":"global"}))),
    ]);
    let (mut state, root, original) = state(kernel);
    let revision = prepare(&mut state, patch).await;
    state
        .operate(SessionOperation::ApplyRuntimePatch { revision })
        .await
        .unwrap();
    state.operate(SessionOperation::Stop {}).await.unwrap();
    let before = std::fs::read(&original).unwrap();
    assert!(matches!(
        state.operate(SessionOperation::Start { revision: 1 }).await,
        Err(ServiceErrorCode::InvalidRequest)
    ));
    assert!(matches!(
        state
            .operate(SessionOperation::Reload {
                revision: 1,
                force: false
            })
            .await,
        Err(ServiceErrorCode::InvalidRequest)
    ));
    assert_eq!(std::fs::read(&original).unwrap(), before);
    assert_eq!(
        state.snapshot().unwrap().candidate.unwrap().phase,
        RuntimeCandidatePhase::Applied
    );
    assert_eq!(finish(state, root, requests).await, ["GET", "PATCH", "GET"]);
}

#[tokio::test]
async fn uncertain_fields_remain_retained_across_pid_change_and_stop() {
    let patch = json!({"mode":"global"});
    let (kernel, requests) = controller(vec![
        ("GET", None, Reply::Json(json!({"mode":"rule"}))),
        ("PATCH", Some(patch.clone()), Reply::Lost),
        ("GET", None, Reply::Json(json!({"mode":"direct"}))),
    ]);
    let (mut state, root, original) = state(kernel);
    let revision = prepare(&mut state, patch).await;
    assert!(matches!(
        state
            .operate(SessionOperation::ApplyRuntimePatch { revision })
            .await,
        Err(ServiceErrorCode::OutcomeUnknown)
    ));
    state.kernel.as_mut().unwrap().replace_fixture_pid(1818);
    let Response::Status { snapshot } = state.operate(SessionOperation::Status {}).await.unwrap()
    else {
        panic!("missing status");
    };
    assert_eq!(
        snapshot.candidate.unwrap().phase,
        RuntimeCandidatePhase::Uncertain
    );
    assert!(matches!(
        state
            .operate(SessionOperation::RestoreRuntimePatch { revision })
            .await,
        Err(ServiceErrorCode::OutcomeUnknown)
    ));
    state.operate(SessionOperation::Stop {}).await.unwrap();
    let Response::Status { snapshot } = state.operate(SessionOperation::Status {}).await.unwrap()
    else {
        panic!("missing status");
    };
    assert!(!snapshot.running);
    assert_eq!(
        snapshot.candidate.unwrap().phase,
        RuntimeCandidatePhase::Uncertain
    );
    let held_config = std::fs::read(&original).unwrap();
    assert!(matches!(
        state.operate(SessionOperation::Start { revision: 1 }).await,
        Err(ServiceErrorCode::InvalidRequest)
    ));
    assert_eq!(std::fs::read(&original).unwrap(), held_config);
    assert!(matches!(
        state
            .operate(SessionOperation::Stage {
                config: "mode: rule".into()
            })
            .await,
        Err(ServiceErrorCode::InvalidRequest)
    ));
    assert_eq!(finish(state, root, requests).await, ["GET", "PATCH", "GET"]);
}

#[tokio::test]
async fn stopped_patch_restore_clears_only_candidate_configuration_and_keeps_shared_assets() {
    let (kernel, requests) = controller(vec![("GET", None, Reply::Json(json!({"mode":"rule"})))]);
    let (mut state, root, original) = state(kernel);
    let before = std::fs::read(&original).unwrap();
    let revision = prepare(&mut state, json!({"mode":"global"})).await;
    let candidate = candidate_path(&state);
    let asset = root.path().join("assets/revision-1/assets/cert.pem");
    state.operate(SessionOperation::Stop {}).await.unwrap();
    assert!(matches!(
        state
            .operate(SessionOperation::RestoreRuntimePatch {
                revision: revision + 1
            })
            .await,
        Err(ServiceErrorCode::StaleRevision)
    ));
    state
        .operate(SessionOperation::RestoreRuntimePatch { revision })
        .await
        .unwrap();
    let snapshot = state.snapshot().unwrap();
    assert!(!snapshot.running && snapshot.pid.is_none());
    assert!(snapshot.candidate.is_none());
    assert_eq!(snapshot.committed_revision, Some(1));
    assert!(snapshot.applied_revision.is_none());
    assert!(!candidate.parent().unwrap().exists());
    assert_eq!(std::fs::read(&original).unwrap(), before);
    assert_eq!(std::fs::read(&asset).unwrap(), b"held certificate bytes");
    assert_eq!(finish(state, root, requests).await, ["GET"]);
}

#[tokio::test]
async fn stopped_patch_cleanup_failure_retains_candidate_and_bounded_retry_owner() {
    let (kernel, requests) = controller(vec![("GET", None, Reply::Json(json!({"mode":"rule"})))]);
    let (mut state, root, original) = state(kernel);
    let before = std::fs::read(&original).unwrap();
    let revision = prepare(&mut state, json!({"mode":"global"})).await;
    let candidate = candidate_path(&state);
    let blocked = candidate.parent().unwrap().join("deep");
    let mut leaf = blocked.clone();
    for _ in 0..26 {
        leaf = leaf.join("d");
    }
    root.create_directory(&leaf).unwrap();
    state.operate(SessionOperation::Stop {}).await.unwrap();
    assert!(matches!(
        state
            .operate(SessionOperation::RestoreRuntimePatch { revision })
            .await,
        Err(ServiceErrorCode::Internal)
    ));
    assert!(state.retired.is_some());
    assert_eq!(
        state.snapshot().unwrap().candidate.unwrap().revision,
        revision
    );
    std::fs::remove_dir_all(&blocked).unwrap();
    state
        .operate(SessionOperation::RestoreRuntimePatch { revision })
        .await
        .unwrap();
    assert!(state.retired.is_none() && state.staged.is_none());
    state.cleanup_retired().unwrap();
    assert_eq!(std::fs::read(&original).unwrap(), before);
    assert_eq!(finish(state, root, requests).await, ["GET"]);
}
