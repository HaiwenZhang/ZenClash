use std::{os::unix::fs::PermissionsExt, sync::Arc, time::Duration};

use serde_yaml::Value;
use tokio::io::AsyncWriteExt;

use super::*;
use crate::{CoreLaunchConfig, ManagedCoreProcess};

struct RestartFixture {
    root: PathBuf,
    profile: PathBuf,
    store: ControlledConfigStore,
    process: Arc<ManagedCoreProcess>,
    controller: tokio::task::JoinHandle<()>,
}

struct ReadinessPause {
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

impl RestartFixture {
    async fn new() -> Self {
        Self::with_readiness_pause(None).await
    }

    async fn with_readiness_pause(pause: Option<ReadinessPause>) -> Self {
        let root = test_root("public-restart-completion");
        fs::create_dir_all(&root).unwrap();
        let profile = root.join("source.yaml");
        fs::write(
            &profile,
            "mixed-port: 0\nmode: rule\ntun: {enable: false}\nrules: [MATCH,DIRECT]\n",
        )
        .unwrap();
        let store = ControlledConfigStore::new(root.join("controlled"));
        store
            .materialize_with_overrides_for_core(&profile, &[], CoreKind::Mihomo)
            .unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let binary = root.join("fixture-core");
        fs::write(
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
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint =
            MihomoEndpoint::new(format!("http://{}", listener.local_addr().unwrap()), "");
        // Core unit tests bypass the production kernel slot in reserve_execution.
        let process = ManagedCoreProcess::spawn(CoreLaunchConfig {
            kind: CoreKind::Mihomo,
            binary,
            config_file: store.runtime_path(),
            home_dir: home.clone(),
            endpoint,
            controller_override: None,
        })
        .unwrap();
        let owner = Arc::downgrade(&process);
        let first_pid = process.snapshot().pid;
        let controller = tokio::spawn(async move {
            let mut pause = pause;
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = crate::test_support::read_controller_request(&mut stream)
                    .await
                    .unwrap()
                    .into_bytes();
                let size = request.len();
                assert!(request[..size].starts_with(b"GET /version "));
                let pid = owner.upgrade().and_then(|process| process.snapshot().pid);
                let published = fs::read_to_string(home.join("startup.pid"))
                    .ok()
                    .and_then(|pid| pid.trim().parse::<u32>().ok());
                let (status, body) = if pid.is_some() && pid == published {
                    if pid != first_pid
                        && let Some(pause) = pause.take()
                    {
                        pause.entered.send(()).unwrap();
                        pause.release.await.unwrap();
                    }
                    ("200 OK", r#"{"meta":true,"version":"fixture"}"#)
                } else {
                    ("503 Service Unavailable", "not started")
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        Self {
            root,
            profile,
            store,
            process,
            controller,
        }
    }

    fn stop(self) {
        self.process.stop().unwrap();
        self.controller.abort();
        fs::remove_dir_all(self.root).unwrap();
    }
}

struct PendingRestartSave {
    fixture: RestartFixture,
    waiter: tokio::task::JoinHandle<super::super::ControlledConfigResult<()>>,
    release_commit: std::sync::mpsc::Sender<()>,
}

async fn prepare_pending_restart_save() -> PendingRestartSave {
    let mut fixture = RestartFixture::new().await;
    let (gate, entered, release_commit) = super::super::CommitGate::new();
    fixture.store.commit_gate = Some(gate);
    let store = fixture.store.clone();
    let process = fixture.process.clone();
    let profile = fixture.profile.clone();
    let mut waiter = tokio::spawn(async move {
        store
            .apply_json_update_with_restart(
                process,
                profile,
                &serde_json::json!({"mode":"global"}),
                vec![],
            )
            .await
    });
    tokio::select! {
        result = entered => result.unwrap(),
        result = &mut waiter => panic!("restart never reached commit: {result:?}"),
        _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("restart did not reach the commit gate"),
    }
    PendingRestartSave {
        fixture,
        waiter,
        release_commit,
    }
}

#[tokio::test]
async fn cancelled_public_restart_save_finishes_commit_and_keeps_running_payload() {
    let PendingRestartSave {
        fixture,
        waiter,
        release_commit,
    } = prepare_pending_restart_save().await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    release_commit.send(()).unwrap();
    let finished = tokio::time::timeout(
        Duration::from_secs(5),
        fixture.store.mutation_gate.clone().lock_owned(),
    )
    .await
    .unwrap();
    let persistent = fixture.store.load_json().unwrap();
    let cache: Value =
        serde_yaml::from_slice(&fs::read(fixture.store.runtime_path()).unwrap()).unwrap();
    let running: Value =
        serde_yaml::from_slice(&fs::read(fixture.root.join("home/startup.yaml")).unwrap()).unwrap();
    let alive = fixture.process.is_running();
    drop(finished);
    fixture.stop();
    assert!(alive);
    assert_eq!(persistent["mode"].as_str(), Some("global"));
    assert_eq!(running["mode"].as_str(), Some("global"));
    assert_eq!(
        cache["mode"].as_str(),
        Some("global"),
        "cancelled waiter reverted the accepted cache"
    );
}

#[tokio::test]
async fn cancelled_public_restart_save_still_rolls_back_after_external_patch_conflict() {
    let PendingRestartSave {
        fixture,
        waiter,
        release_commit,
    } = prepare_pending_restart_save().await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    let external_patch = b"mode: direct\n";
    fs::write(fixture.store.patch_path(), external_patch).unwrap();
    release_commit.send(()).unwrap();
    let finished = tokio::time::timeout(
        Duration::from_secs(5),
        fixture.store.mutation_gate.clone().lock_owned(),
    )
    .await
    .unwrap();
    let persistent = fs::read(fixture.store.patch_path()).unwrap();
    let cache: Value =
        serde_yaml::from_slice(&fs::read(fixture.store.runtime_path()).unwrap()).unwrap();
    let running: Value =
        serde_yaml::from_slice(&fs::read(fixture.root.join("home/startup.yaml")).unwrap()).unwrap();
    let alive = fixture.process.is_running();
    drop(finished);
    fixture.stop();
    assert!(alive);
    assert_eq!(persistent, external_patch);
    assert_eq!(running["mode"].as_str(), Some("rule"));
    assert_eq!(
        cache["mode"].as_str(),
        Some("rule"),
        "cancelled waiter abandoned the required rollback"
    );
}

#[tokio::test]
async fn cancelled_public_profile_restart_finishes_readiness_and_cache_acceptance() {
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let fixture = RestartFixture::with_readiness_pause(Some(ReadinessPause {
        entered,
        release: released,
    }))
    .await;
    let replacement = fixture.root.join("replacement.yaml");
    fs::write(
        &replacement,
        "mixed-port: 0\nmode: global\ntun: {enable: false}\nrules: [MATCH,DIRECT]\n",
    )
    .unwrap();
    let store = fixture.store.clone();
    let process = fixture.process.clone();
    let mut waiter = tokio::spawn(async move {
        store
            .restart_with_overrides(process, replacement, vec![])
            .await
    });
    tokio::select! {
        result = waiting => result.unwrap(),
        result = &mut waiter => panic!("restart never reached readiness: {result:?}"),
        _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("restart did not reach the readiness pause"),
    }
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert!(
        fixture.store.mutation_gate.try_lock().is_err(),
        "admitted readiness lost its mutation owner"
    );
    release.send(()).unwrap();
    let finished = tokio::time::timeout(
        Duration::from_secs(5),
        fixture.store.mutation_gate.clone().lock_owned(),
    )
    .await
    .unwrap();
    let cache: Value =
        serde_yaml::from_slice(&fs::read(fixture.store.runtime_path()).unwrap()).unwrap();
    let running: Value =
        serde_yaml::from_slice(&fs::read(fixture.root.join("home/startup.yaml")).unwrap()).unwrap();
    drop(finished);
    fixture.stop();
    assert_eq!(cache["mode"].as_str(), Some("global"));
    assert_eq!(running["mode"].as_str(), Some("global"));
}

#[tokio::test]
async fn public_restart_requests_cancelled_while_queued_do_not_change_configuration_or_pid() {
    use std::{future::Future, task::Context};

    let fixture = RestartFixture::new().await;
    let before = fs::read(fixture.store.runtime_path()).unwrap();
    let pid = fixture.process.snapshot().pid;
    let mutation = fixture.store.mutation_gate.lock().await;
    let patch = serde_json::json!({"mode":"global"});
    let mut update = Box::pin(fixture.store.apply_json_update_with_restart(
        fixture.process.clone(),
        &fixture.profile,
        &patch,
        vec![],
    ));
    assert!(
        update
            .as_mut()
            .poll(&mut Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
    drop(update);
    let mut restart = Box::pin(fixture.store.restart_with_overrides(
        fixture.process.clone(),
        &fixture.profile,
        vec![],
    ));
    assert!(
        restart
            .as_mut()
            .poll(&mut Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
    drop(restart);
    drop(mutation);
    let after = fs::read(fixture.store.runtime_path()).unwrap();
    let after_pid = fixture.process.snapshot().pid;
    let persistent = fixture.store.load_json().unwrap();
    fixture.stop();
    assert_eq!(after, before);
    assert_eq!(after_pid, pid);
    assert!(persistent.as_object().unwrap().is_empty());
}

#[tokio::test]
async fn public_restart_save_cancelled_while_waiting_for_data_does_not_launch_a_completion() {
    use std::{future::Future, task::Context};

    let fixture = RestartFixture::new().await;
    let before = fs::read(fixture.store.runtime_path()).unwrap();
    let pid = fixture.process.snapshot().pid;
    let lease = crate::data_coordinator::DataWriteLease::exclusive(vec![fixture.root.clone()]);
    let patch = serde_json::json!({"mode":"global"});
    let mut update = Box::pin(fixture.store.apply_json_update_with_restart(
        fixture.process.clone(),
        &fixture.profile,
        &patch,
        vec![],
    ));
    assert!(
        update
            .as_mut()
            .poll(&mut Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
    drop(update);
    drop(lease);
    let after = fs::read(fixture.store.runtime_path()).unwrap();
    let after_pid = fixture.process.snapshot().pid;
    let persistent = fixture.store.load_json().unwrap();
    fixture.stop();
    assert_eq!(after, before);
    assert_eq!(after_pid, pid);
    assert!(persistent.as_object().unwrap().is_empty());
}
