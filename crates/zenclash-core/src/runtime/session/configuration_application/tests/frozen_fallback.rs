//! The ordinary config interface must use the same candidate after a lost reload reply.

use super::*;
use std::os::unix::fs::PermissionsExt;
use tokio::io::AsyncWriteExt;

struct ChangedSourceFixture {
    root: PathBuf,
    profile: PathBuf,
    override_path: PathBuf,
    store: ControlledConfigStore,
    session: CoreSession,
    process: Arc<ManagedCoreProcess>,
    attempted_payload: tokio::sync::oneshot::Receiver<String>,
    controller: tokio::task::JoinHandle<()>,
}

impl ChangedSourceFixture {
    async fn new() -> Self {
        let root = session_test_root("ordinary-frozen-fallback");
        let profile = root.join("profile.yaml");
        let override_path = root.join("override.yaml");
        std::fs::write(&profile, "mode: rule\ntun: {enable: false}\n").unwrap();
        std::fs::write(&override_path, "mode: direct\n").unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        store.materialize(&profile).unwrap();
        let home = root.join("home");
        std::fs::create_dir(&home).unwrap();
        let binary = root.join("fixture-core");
        std::fs::write(
            &binary,
            concat!(
                "#!/bin/sh\nset -e\nif [ \"$1\" = '-t' ]; then exit 0; fi\n",
                "cat \"$4\" > \"$2/startup.$$.tmp\"\n",
                "mv \"$2/startup.$$.tmp\" \"$2/startup.yaml\"\n",
                "printf '%s\\n' \"$$\" > \"$2/startup-pid.$$.tmp\"\n",
                "mv \"$2/startup-pid.$$.tmp\" \"$2/startup.pid\"\n",
                "exec sleep 60\n",
            ),
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let process = ManagedCoreProcess::spawn(crate::CoreLaunchConfig {
            kind: CoreKind::Mihomo,
            binary,
            config_file: store.runtime_path(),
            home_dir: home.clone(),
            endpoint: crate::MihomoEndpoint::new(
                format!("http://{}", listener.local_addr().unwrap()),
                "",
            ),
            controller_override: None,
        })
        .unwrap();
        let owner = Arc::downgrade(&process);
        let changed_profile = profile.clone();
        let removed_override = override_path.clone();
        let (attempted, attempted_payload) = tokio::sync::oneshot::channel();
        let controller = tokio::spawn(async move {
            let mut attempted = Some(attempted);
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (headers, body) = crate::test_support::read_controller_request(&mut stream)
                    .await
                    .unwrap()
                    .into_parts();
                if headers.starts_with("PUT ") {
                    let payload =
                        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["payload"]
                            .as_str()
                            .unwrap()
                            .to_owned();
                    std::fs::write(&changed_profile, "mode: global\ntun: {enable: true}\n")
                        .unwrap();
                    std::fs::remove_file(&removed_override).unwrap();
                    attempted.take().unwrap().send(payload).unwrap();
                    continue;
                }
                assert!(headers.starts_with("GET /version "));
                let pid = owner.upgrade().and_then(|process| process.snapshot().pid);
                let published = std::fs::read_to_string(home.join("startup.pid"))
                    .ok()
                    .and_then(|pid| pid.trim().parse::<u32>().ok());
                let (status, body) = if pid.is_some() && pid == published {
                    (
                        "200 OK",
                        r#"{"meta":true,"version":"frozen-config-fixture"}"#,
                    )
                } else {
                    ("503 Service Unavailable", "not started")
                };
                stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        process
            .wait_until_ready(Duration::from_secs(5))
            .await
            .unwrap();
        let session = CoreSession::open(
            CoreKind::Mihomo,
            MihomoClient::from_process(process.clone()).unwrap(),
        )
        .unwrap();
        Self {
            root,
            profile,
            override_path,
            store,
            session,
            process,
            attempted_payload,
            controller,
        }
    }
}

impl Drop for ChangedSourceFixture {
    fn drop(&mut self) {
        self.controller.abort();
        self.process.stop().unwrap();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[tokio::test]
async fn ordinary_patch_restart_uses_the_attempted_payload_after_sources_change() {
    let mut fixture = ChangedSourceFixture::new().await;
    let pid = fixture.process.snapshot().pid;
    let generation = fixture.session.generation();
    let outcome = fixture
        .session
        .apply(
            &fixture.store,
            EffectiveConfigIntent::Patch {
                profile: fixture.profile.clone(),
                patch: serde_json::json!({"allow-lan":true}),
                overrides: vec![fixture.override_path.clone()],
            },
        )
        .await
        .unwrap();
    let attempted = (&mut fixture.attempted_payload).await.unwrap();
    let startup = std::fs::read_to_string(
        fixture
            .process
            .launch_config()
            .home_dir
            .join("startup.yaml"),
    )
    .unwrap();
    assert_eq!(startup, attempted);
    assert_eq!(
        fixture.store.cached_runtime_payload().unwrap().unwrap(),
        attempted
    );
    let yaml: serde_yaml::Value = serde_yaml::from_str(&startup).unwrap();
    assert_eq!(yaml["tun"]["enable"].as_bool(), Some(false));
    assert_eq!(yaml["mode"].as_str(), Some("direct"));
    assert_eq!(fixture.store.load_json().unwrap()["allow-lan"], true);
    assert_eq!(outcome.kind, CoreApplyKind::Restarted);
    assert_eq!(outcome.generation, generation + 1);
    assert_eq!(fixture.session.generation(), outcome.generation);
    assert_ne!(fixture.process.snapshot().pid, pid);
    fixture.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn ordinary_activation_restart_uses_the_attempted_payload_after_sources_change() {
    let mut fixture = ChangedSourceFixture::new().await;
    let pid = fixture.process.snapshot().pid;
    let generation = fixture.session.generation();
    let outcome = fixture
        .session
        .apply(
            &fixture.store,
            EffectiveConfigIntent::ActivateProfile {
                profile: fixture.profile.clone(),
                overrides: vec![fixture.override_path.clone()],
            },
        )
        .await
        .unwrap();
    let attempted = (&mut fixture.attempted_payload).await.unwrap();
    let startup = std::fs::read_to_string(
        fixture
            .process
            .launch_config()
            .home_dir
            .join("startup.yaml"),
    )
    .unwrap();
    assert_eq!(startup, attempted);
    assert_eq!(
        fixture.store.cached_runtime_payload().unwrap().unwrap(),
        attempted
    );
    let yaml: serde_yaml::Value = serde_yaml::from_str(&startup).unwrap();
    assert_eq!(yaml["tun"]["enable"].as_bool(), Some(false));
    assert_eq!(yaml["mode"].as_str(), Some("direct"));
    assert_eq!(fixture.store.load_json().unwrap(), serde_json::json!({}));
    assert_eq!(outcome.kind, CoreApplyKind::Restarted);
    assert_eq!(outcome.generation, generation + 1);
    assert_eq!(fixture.session.generation(), outcome.generation);
    assert_ne!(fixture.process.snapshot().pid, pid);
    fixture.session.shutdown().await.unwrap();
}
