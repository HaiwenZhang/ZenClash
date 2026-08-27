use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use zenclash_core::{
    ControlledConfigStore, CoreKind, MihomoClient, MihomoLaunchConfig, MihomoProcess,
};

/// This test intentionally has no mock controller. Build meow-rs and set
/// `ZENCLASH_MEOW_BINARY` to the resulting real executable before running it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires ZENCLASH_MEOW_BINARY pointing to a real meow-rs executable"]
async fn drives_the_supplied_profile_and_restart_transaction_through_real_meow() {
    let inputs = IntegrationInputs::from_env();
    let controlled = ControlledConfigStore::new(inputs.root.join("controlled-config"));
    let effective = controlled
        .materialize(&inputs.profile)
        .expect("materialize the real profile for meow-rs");
    let controller = reserve_controller();
    let launch = MihomoLaunchConfig::for_kind(
        CoreKind::Meow,
        &inputs.binary,
        effective,
        inputs.root.join("meow-home"),
    )
    .expect("construct a real meow-rs launch")
    .with_controller_override(controller);
    let process = MihomoProcess::spawn(launch).expect("start real meow-rs");
    process
        .wait_until_ready(Duration::from_secs(20))
        .await
        .expect("real meow-rs controller becomes ready");
    let client = MihomoClient::new(process.endpoint().clone()).expect("real meow-rs client");

    let version = client.version().await.expect("read real meow-rs version");
    assert!(!version.version.trim().is_empty());
    assert_eq!(process.kind(), CoreKind::Meow);
    assert!(!process.capabilities().full_config_reload);
    client
        .proxy_catalog()
        .await
        .expect("read real meow-rs proxy catalog");
    client
        .connections_snapshot()
        .await
        .expect("read real meow-rs connection snapshot");

    let proxy_port = reserve_port();
    controlled
        .apply_json_update_with_restart(
            process.clone(),
            &inputs.profile,
            &serde_json::json!({"mode": "direct", "mixed-port": proxy_port}),
            Vec::new(),
        )
        .await
        .expect("apply a real setting by restarting meow-rs");
    let config = client
        .runtime_config()
        .await
        .expect("read config after the real meow-rs restart");
    assert_eq!(config.mode.to_ascii_lowercase(), "direct");
    assert_eq!(config.mixed_port, proxy_port);
    assert_eq!(controlled.load_json().unwrap()["mode"], "direct");
    verify_real_direct_proxy(proxy_port);

    process.stop().expect("stop real meow-rs");
    std::fs::remove_dir_all(&inputs.root).expect("remove isolated meow-rs integration data");
}

struct IntegrationInputs {
    binary: PathBuf,
    profile: PathBuf,
    root: PathBuf,
}

impl IntegrationInputs {
    fn from_env() -> Self {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace = manifest_dir
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let binary = std::env::var_os("ZENCLASH_MEOW_BINARY")
            .map(PathBuf::from)
            .map(|path| workspace_path(workspace, path))
            .expect("set ZENCLASH_MEOW_BINARY to a real meow-rs executable");
        let profile = std::env::var_os("ZENCLASH_CONFIG").map_or_else(
            || workspace.join("platforms/common/default.yaml"),
            |path| workspace_path(workspace, PathBuf::from(path)),
        );
        assert!(binary.is_file(), "missing meow-rs: {}", binary.display());
        assert!(profile.is_file(), "missing profile: {}", profile.display());
        let sequence = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::var_os("ZENCLASH_INTEGRATION_HOME").map_or_else(
            || {
                std::env::temp_dir().join(format!(
                    "zenclash-real-meow-{}-{sequence}",
                    std::process::id()
                ))
            },
            PathBuf::from,
        );
        Self {
            binary,
            profile,
            root,
        }
    }
}

fn workspace_path(workspace: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    }
}

fn reserve_controller() -> String {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .expect("reserve a real meow-rs controller port");
    let port = listener.local_addr().expect("controller address").port();
    drop(listener);
    format!("127.0.0.1:{port}")
}

fn reserve_port() -> u16 {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .expect("reserve a real meow-rs proxy port");
    let port = listener.local_addr().expect("proxy address").port();
    drop(listener);
    port
}

fn verify_real_direct_proxy(proxy_port: u16) {
    let origin = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .expect("start a real loopback HTTP origin");
    let origin_address = origin.local_addr().expect("origin address");
    let server = thread::spawn(move || {
        let (mut stream, _) = origin.accept().expect("accept request from real meow-rs");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set origin timeout");
        let mut request = [0_u8; 4_096];
        let length = stream.read(&mut request).expect("read proxied request");
        assert!(String::from_utf8_lossy(&request[..length]).contains("GET /probe"));
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .expect("write loopback response");
    });

    let mut proxy = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, proxy_port))
        .expect("connect to the real meow-rs mixed listener");
    proxy
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set proxy timeout");
    write!(
        proxy,
        "GET http://{origin_address}/probe HTTP/1.1\r\nHost: {origin_address}\r\nConnection: close\r\n\r\n"
    )
    .expect("send request through real meow-rs");
    let mut response = String::new();
    proxy
        .read_to_string(&mut response)
        .expect("read response through real meow-rs");
    assert!(response.starts_with("HTTP/1.1 204"), "{response}");
    server.join().expect("real loopback HTTP origin completed");
}
