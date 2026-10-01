use std::{path::PathBuf, process::Command, sync::Arc, time::Duration};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{MihomoClient, MihomoError};
use crate::{CoreKind, MihomoEndpoint, MihomoLaunchConfig, MihomoProcess, owned_core::OwnedCore};

#[test]
fn an_external_controller_has_no_managed_process_owner() {
    let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();

    assert!(client.owned_core().is_none());
}

#[test]
fn a_process_binding_retains_the_actual_child_and_its_endpoint() {
    let fixture = ProcessFixture::new(CoreKind::Mihomo, "local-owner");
    let client = MihomoClient::from_process(fixture.process().clone()).unwrap();
    let snapshot = client.binding.snapshot();
    let Some(OwnedCore::Local(owner)) = snapshot.owned_core() else {
        panic!("managed Local binding lost its real process owner");
    };

    assert!(Arc::ptr_eq(&owner, fixture.process()));
    assert_eq!(client.endpoint().unwrap().secret, "local-owner");
    assert!(owner.snapshot().pid.is_some());
}

#[tokio::test]
async fn a_switch_publishes_process_owner_and_endpoint_to_existing_clones_together() {
    let fixture = ProcessFixture::new(CoreKind::Mihomo, "replacement-owner");
    let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();
    let clone = client.clone();
    let old = clone.binding.snapshot();
    let mut changed = clone.binding.subscribe();

    client
        .switch_to_process(fixture.process().clone())
        .await
        .unwrap();
    changed.changed().await.unwrap();
    let published = changed.borrow_and_update().clone();
    let Some(OwnedCore::Local(owner)) = published.owned_core() else {
        panic!("subscriber received endpoint without managed ownership");
    };

    assert_eq!(published.generation, old.generation + 1);
    assert!(old.owned_core().is_none());
    assert!(Arc::ptr_eq(&owner, fixture.process()));
    assert_eq!(clone.endpoint().unwrap().secret, owner.endpoint().secret);
    let Some(OwnedCore::Local(clone_owner)) = clone.owned_core() else {
        panic!("existing clone did not receive managed ownership");
    };
    assert!(Arc::ptr_eq(&clone_owner, &owner));
}

#[tokio::test]
async fn a_process_of_another_kind_cannot_replace_a_client_binding() {
    let fixture = ProcessFixture::new(CoreKind::Meow, "unsupported-owner");
    let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();
    let generation = client.binding.generation();
    let endpoint = client.endpoint().unwrap();

    let result = client.switch_to_process(fixture.process().clone()).await;

    assert!(matches!(result, Err(MihomoError::InvalidInput(_))));
    assert_eq!(client.binding.generation(), generation);
    assert_eq!(client.endpoint().unwrap().controller, endpoint.controller);
    assert!(client.owned_core().is_none());
}

#[tokio::test]
async fn an_existing_clone_validates_with_the_new_process_binary_and_home() {
    let previous = ProcessFixture::new(CoreKind::Mihomo, "previous-validator");
    let replacement = ProcessFixture::new(CoreKind::Mihomo, "replacement-validator");
    let client = MihomoClient::from_process(previous.process().clone()).unwrap();
    let clone = client.clone();
    previous.process().stop().unwrap();

    client
        .switch_to_process(replacement.process().clone())
        .await
        .unwrap();
    clone
        .validate_config_payload("rules:\n  - MATCH,DIRECT\n")
        .await
        .unwrap();

    assert!(replacement.directory.join("home/validation-ran").exists());
    assert!(!previous.directory.join("home/validation-ran").exists());
}

#[tokio::test]
async fn a_binding_changed_while_waiting_for_data_lease_rejects_before_preflight() {
    let previous = ProcessFixture::new(CoreKind::Mihomo, "leased-validator");
    let replacement = ProcessFixture::new(CoreKind::Mihomo, "unleased-validator");
    let client = MihomoClient::from_process(previous.process().clone()).unwrap();
    let exclusive = crate::data_coordinator::DataWriteLease::exclusive(
        previous.process().config_validator().write_scopes(),
    );
    let clone = client.clone();
    let mut validation = Box::pin(async move {
        clone
            .validate_config_payload("rules:\n  - MATCH,DIRECT\n")
            .await
    });
    // One poll captures the original scopes and starts their blocked Data wait.
    assert!(futures_util::poll!(&mut validation).is_pending());
    client
        .switch_to_process(replacement.process().clone())
        .await
        .unwrap();
    drop(exclusive);

    let result = tokio::spawn(validation).await;

    assert!(
        matches!(result, Ok(Err(MihomoError::StaleBinding))),
        "{result:?}"
    );
    assert!(!previous.directory.join("home/validation-ran").exists());
    assert!(!replacement.directory.join("home/validation-ran").exists());
    assert!(!MihomoError::StaleBinding.mutation_result_unknown());
}

#[tokio::test]
async fn releasing_the_last_process_owner_does_not_hold_the_watch_lock() {
    let mut fixture = ProcessFixture::new(CoreKind::Mihomo, "retired-owner");
    let mut pause = fixture.process().pause_last_drop_for_test();
    let client = MihomoClient::from_process(fixture.process.take().unwrap()).unwrap();
    let detached = client.clone();
    let transition = tokio::spawn(async move {
        detached
            .switch_to_direct(MihomoEndpoint::new("http://127.0.0.1:1", "published"))
            .await
    });
    // Use a blocking helper so the test can observe a worker blocked in the real last-owner Drop.
    tokio::task::spawn_blocking(move || {
        pause.wait_started();
        let (published, observed) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            published.send(client.endpoint().unwrap().secret).unwrap();
        });
        let result = observed.recv_timeout(Duration::from_millis(100));
        pause.resume();
        reader.join().unwrap();
        assert_eq!(result.unwrap(), "published");
    })
    .await
    .unwrap();
    transition.await.unwrap().unwrap();
}

#[tokio::test]
async fn retirement_stops_the_old_child_while_a_previous_pin_retains_it() {
    let fixture = ProcessFixture::new(CoreKind::Mihomo, "retained-old-owner");
    let client = MihomoClient::from_process(fixture.process().clone()).unwrap();
    let previous = client.pin_binding().unwrap();
    assert!(fixture.process().snapshot().pid.is_some());

    client
        .switch_to_direct(MihomoEndpoint::default())
        .await
        .unwrap();

    let Some(OwnedCore::Local(owner)) = previous.owned_core() else {
        panic!("previous pin lost the immutable owner");
    };
    assert!(
        owner.snapshot().pid.is_none(),
        "retired child is still running"
    );
    assert!(matches!(
        previous.version().await,
        Err(MihomoError::StaleBinding)
    ));
}

#[tokio::test]
async fn retirement_preserves_the_child_when_the_same_local_owner_is_rebound() {
    let fixture = ProcessFixture::new(CoreKind::Mihomo, "retained-current-owner");
    let client = MihomoClient::from_process(fixture.process().clone()).unwrap();
    let pid = fixture.process().snapshot().pid;
    assert!(pid.is_some());

    client
        .switch_to_process(fixture.process().clone())
        .await
        .unwrap();

    assert_eq!(fixture.process().snapshot().pid, pid);
}

#[tokio::test]
async fn a_local_binding_cannot_validate_with_an_unrelated_explicit_binary() {
    let fixture = ProcessFixture::new(CoreKind::Mihomo, "actual-validator");
    let client = MihomoClient::from_process(fixture.process().clone())
        .unwrap()
        .with_config_validator(crate::CoreConfigValidator::new(
            CoreKind::Mihomo,
            fixture.directory.join("unrelated-binary-does-not-exist"),
            fixture.directory.join("unrelated-home"),
        ))
        .unwrap();

    client
        .validate_config_payload("rules:\n  - MATCH,DIRECT\n")
        .await
        .unwrap();

    assert!(fixture.directory.join("home/validation-ran").exists());
    assert!(!fixture.directory.join("unrelated-home").exists());
}

#[tokio::test]
async fn an_explicit_external_switch_clears_the_shared_managed_owner() {
    let fixture = ProcessFixture::new(CoreKind::Mihomo, "previous-owner");
    let client = MihomoClient::from_process(fixture.process().clone()).unwrap();
    let clone = client.clone();
    fixture.process().stop().unwrap();

    client
        .switch_to_direct(MihomoEndpoint::default())
        .await
        .unwrap();

    assert!(clone.owned_core().is_none());
    assert!(clone.current_config_validator().is_none());
}

#[tokio::test]
async fn a_previous_controller_response_is_rejected_after_managed_ownership_changes() {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let (received, wait_received) = tokio::sync::oneshot::channel();
    let (resume, wait_resume) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 2048];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        received.send(()).unwrap();
        wait_resume.await.unwrap();
        let body = r#"{"version":"previous-controller"}"#;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let fixture = ProcessFixture::new(CoreKind::Mihomo, "new-owner");
    let client = MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();
    let clone = client.clone();
    let request = tokio::spawn(async move { clone.version().await });
    wait_received.await.unwrap();

    client
        .switch_to_process(fixture.process().clone())
        .await
        .unwrap();
    resume.send(()).unwrap();

    assert!(matches!(
        request.await.unwrap(),
        Err(MihomoError::StaleTransport)
    ));
    server.await.unwrap();
}

// This executable is a real owned child, solely for lifecycle/binding behavior.
// It does not implement Mihomo or prove native core integration.
struct ProcessFixture {
    process: Option<Arc<MihomoProcess>>,
    directory: PathBuf,
}

impl ProcessFixture {
    fn process(&self) -> &Arc<MihomoProcess> {
        self.process.as_ref().unwrap()
    }

    fn new(kind: CoreKind, secret: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "zenclash-binding-child-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("owned_child.rs");
        std::fs::write(
            &source,
            r#"fn main() {
                let args: Vec<_> = std::env::args().collect();
                if args.iter().any(|arg| arg == "-t") {
                    let home = args.windows(2).find(|pair| pair[0] == "-d").unwrap();
                    std::fs::write(std::path::Path::new(&home[1]).join("validation-ran"), "validated").unwrap();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
            "#,
        )
        .unwrap();
        let binary = directory.join(if cfg!(windows) {
            "owned_child.exe"
        } else {
            "owned_child"
        });
        let compilation = Command::new("rustc")
            .arg("--edition=2024")
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compilation.status.success(),
            "{}",
            String::from_utf8_lossy(&compilation.stderr)
        );
        let configuration = directory.join("profile.yaml");
        std::fs::write(&configuration, "rules:\n  - MATCH,DIRECT\n").unwrap();
        let process = MihomoProcess::spawn(MihomoLaunchConfig {
            kind,
            binary,
            config_file: configuration,
            home_dir: directory.join("home"),
            endpoint: MihomoEndpoint::new("http://127.0.0.1:1", secret),
            controller_override: None,
        })
        .unwrap();
        Self {
            process: Some(process),
            directory,
        }
    }
}

impl Drop for ProcessFixture {
    fn drop(&mut self) {
        if let Some(process) = &self.process {
            process.stop().unwrap();
        }
        // Stop joins neither collector. Give their already-closed handles time to close on Windows.
        for attempt in 0..20 {
            match std::fs::remove_dir_all(&self.directory) {
                Ok(()) => return,
                Err(error) if attempt == 19 => panic!("fixture cleanup failed: {error}"),
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
}
