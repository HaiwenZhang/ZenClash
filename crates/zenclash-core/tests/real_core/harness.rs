use std::{
    io::{Read, Write},
    net::{TcpListener, UdpSocket},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Duration,
};

use zenclash_core::{
    ControlledConfigStore, CoreLaunchConfig, ManagedCoreProcess, MihomoClient, MihomoEndpoint,
    ProxyCatalog, RuntimeConfig,
};

pub(crate) struct IntegrationInputs {
    pub(crate) binary: PathBuf,
    pub(crate) profile: PathBuf,
    pub(crate) home: PathBuf,
    pub(crate) controller: String,
}

impl IntegrationInputs {
    pub(crate) fn from_env() -> Self {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace = manifest_dir
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let binary = std::env::var_os("ZENCLASH_MIHOMO_BINARY")
            .map(PathBuf::from)
            .map(|path| workspace_path(workspace, path))
            .expect("set ZENCLASH_MIHOMO_BINARY to a real Mihomo executable");
        let profile = std::env::var_os("ZENCLASH_CONFIG").map_or_else(
            || workspace.join("platforms/common/default.yaml"),
            |path| workspace_path(workspace, PathBuf::from(path)),
        );
        assert!(profile.is_file(), "missing profile: {}", profile.display());
        let home = std::env::var_os("ZENCLASH_INTEGRATION_HOME").map_or_else(
            || std::env::temp_dir().join(format!("zenclash-real-mihomo-{}", std::process::id())),
            PathBuf::from,
        );
        let controller = std::env::var("ZENCLASH_INTEGRATION_CONTROLLER")
            .unwrap_or_else(|_| "127.0.0.1:19091".to_owned());
        Self {
            binary,
            profile,
            home,
            controller,
        }
    }

    pub(crate) fn seed_real_geodata(&self) {
        let Some(source) = std::env::var_os("ZENCLASH_INTEGRATION_GEODATA_DIR").map(PathBuf::from)
        else {
            return;
        };
        std::fs::create_dir_all(&self.home).expect("create real Mihomo integration home");
        let mut copied = 0;
        for name in ["geoip.metadb", "country.mmdb", "geoip.dat", "geosite.dat"] {
            let source_file = source.join(name);
            if source_file.is_file() {
                std::fs::copy(&source_file, self.home.join(name))
                    .unwrap_or_else(|error| panic!("copy {}: {error}", source_file.display()));
                copied += 1;
            }
        }
        assert!(
            copied > 0,
            "no real GeoData files found in {}",
            source.display()
        );
    }
}

pub(crate) async fn start_mihomo(inputs: &IntegrationInputs) -> Arc<ManagedCoreProcess> {
    let controlled = ControlledConfigStore::new(inputs.home.join("controlled-config-integration"));
    let runtime_profile = controlled
        .materialize(&inputs.profile)
        .expect("materialize managed startup profile");
    let endpoint = MihomoEndpoint::with_random_secret(&inputs.controller)
        .expect("generate authenticated integration controller endpoint");
    let launch = CoreLaunchConfig::new(&inputs.binary, runtime_profile, inputs.home.clone())
        .expect("real launch config")
        .with_controller_endpoint(endpoint);
    let process = ManagedCoreProcess::spawn(launch).expect("start real Mihomo");
    process
        .wait_until_ready(Duration::from_secs(20))
        .await
        .unwrap_or_else(|error| {
            panic!(
                "real Mihomo did not become ready: {error}\n{}",
                process.snapshot().logs.join("\n")
            )
        });
    process
}

pub(crate) async fn wait_for_real_proxy_groups(
    client: &MihomoClient,
    expected: &[&str],
) -> ProxyCatalog {
    // /version can respond before Mihomo applies its initial proxy configuration.
    // Wait for membership only; the caller must still assert order without retries.
    let mut observed = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let catalog = client.proxy_catalog().await.expect("read real Mihomo proxy catalog");
            observed = catalog.groups_for_mode("rule")
                .map(|group| group.controller_name().to_owned())
                .collect::<Vec<_>>();
            if expected.iter().all(|name| observed.iter().any(|key| key == name)) {
                return catalog;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("real Mihomo proxy groups did not become ready: expected {expected:?}, last observed {observed:?}"))
}

pub(crate) async fn wait_for_real_mixed_listener(
    process: &ManagedCoreProcess,
    client: &MihomoClient,
    expected_port: u16,
) -> RuntimeConfig {
    // Mihomo starts its controller before applying the initial configuration.
    // /version readiness can therefore precede the proxy listener appearing in /configs.
    let mut observed_port = None;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let config = client
                .runtime_config()
                .await
                .expect("read real Mihomo listener runtime config");
            observed_port = Some(config.mixed_port);
            if config.mixed_port == expected_port {
                return config;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "real Mihomo mixed listener did not become ready: expected {expected_port}, last observed {observed_port:?}\n{}",
            process.snapshot().logs.join("\n")
        )
    })
}

pub(crate) fn reserve_loopback_port() -> u16 {
    TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .expect("reserve loopback port")
        .local_addr()
        .expect("read reserved loopback port")
        .port()
}

pub(crate) fn start_test_dns_server() -> (String, thread::JoinHandle<Vec<u16>>) {
    let socket =
        UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("bind deterministic DNS server");
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set deterministic DNS timeout");
    let address = socket.local_addr().expect("deterministic DNS address");
    let server = thread::spawn(move || {
        let mut queried_types = Vec::new();
        let mut buffer = [0_u8; 2_048];
        while let Ok((length, peer)) = socket.recv_from(&mut buffer) {
            let Some((question_end, record_type)) = dns_question(&buffer[..length]) else {
                continue;
            };
            queried_types.push(record_type);
            let response = dns_response(&buffer[..length], question_end, record_type);
            socket
                .send_to(&response, peer)
                .expect("reply from deterministic DNS server");
        }
        queried_types
    });
    (format!("udp://{address}"), server)
}

fn dns_question(query: &[u8]) -> Option<(usize, u16)> {
    if query.len() < 17 {
        return None;
    }
    let mut cursor = 12;
    while *query.get(cursor)? != 0 {
        let label_len = usize::from(*query.get(cursor)?);
        cursor = cursor.checked_add(label_len + 1)?;
    }
    let question_end = cursor.checked_add(5)?;
    let record_type = u16::from_be_bytes([*query.get(cursor + 1)?, *query.get(cursor + 2)?]);
    (question_end <= query.len()).then_some((question_end, record_type))
}

fn dns_response(query: &[u8], question_end: usize, record_type: u16) -> Vec<u8> {
    let answer = match record_type {
        1 => Some(vec![192, 0, 2, 1]),
        28 => Some(
            std::net::Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)
                .octets()
                .to_vec(),
        ),
        _ => None,
    };
    let mut response =
        Vec::with_capacity(question_end + answer.as_ref().map_or(0, |data| data.len() + 12));
    response.extend_from_slice(&query[..2]);
    response.extend_from_slice(&[0x81, 0x80]);
    response.extend_from_slice(&[0, 1]);
    response.extend_from_slice(&[0, u8::from(answer.is_some())]);
    response.extend_from_slice(&[0, 0, 0, 0]);
    response.extend_from_slice(&query[12..question_end]);
    if let Some(answer) = answer {
        response.extend_from_slice(&[0xc0, 0x0c]);
        response.extend_from_slice(&record_type.to_be_bytes());
        response.extend_from_slice(&[0, 1]);
        response.extend_from_slice(&120_u32.to_be_bytes());
        response.extend_from_slice(&u16::try_from(answer.len()).unwrap().to_be_bytes());
        response.extend_from_slice(&answer);
    }
    response
}

pub(crate) fn serve_subscription(profile: &Path) -> (String, thread::JoinHandle<()>) {
    let subscription_payload = std::fs::read(profile).expect("read real subscription payload");
    let listener =
        TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("bind subscription endpoint");
    let subscription_url = format!(
        "http://{}/clash.yaml",
        listener.local_addr().expect("subscription address")
    );
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept subscription request");
        let mut request = [0_u8; 8_192];
        let read = stream
            .read(&mut request)
            .expect("read subscription request");
        let request = String::from_utf8_lossy(&request[..read]);
        assert!(
            request
                .to_ascii_lowercase()
                .contains("user-agent: zenclash-integration"),
            "custom subscription User-Agent was not sent: {request}"
        );
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/yaml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            subscription_payload.len()
        )
        .expect("write subscription headers");
        stream
            .write_all(&subscription_payload)
            .expect("write subscription payload");
    });
    (subscription_url, server)
}

fn workspace_path(workspace: &std::path::Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    }
}
