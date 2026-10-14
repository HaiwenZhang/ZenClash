use std::{
    fs,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tokio::{io::AsyncWriteExt, sync::oneshot};
use zenclash_core::{
    ControlledConfigStore, CoreKind, CoreSession, MihomoClient, MihomoEndpoint,
    YamlOverrideCatalog, YamlOverrideRecord, YamlOverrideStore,
};

use super::super::ApplicationCommands;
use super::{OverrideApplication, OverrideFailureKind};

struct Fixture {
    root: PathBuf,
    store: YamlOverrideStore,
    session: CoreSession,
    application: OverrideApplication,
}

impl Fixture {
    fn new(url: &str, active_profile: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "zenclash-override-application-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let profile = root.join("profile.yaml");
        fs::write(&profile, "mixed-port: 0\nrules: [MATCH,DIRECT]\n").unwrap();
        let controlled = ControlledConfigStore::new(root.join("controlled-config"));
        controlled.materialize(&profile).unwrap();
        let store = YamlOverrideStore::new(root.join("yaml-overrides")).unwrap();
        let client = MihomoClient::new(MihomoEndpoint::new(url, "")).unwrap();
        let session = CoreSession::open_with_config(
            CoreKind::Mihomo,
            client,
            active_profile.then_some(profile),
            Vec::new(),
        )
        .unwrap();
        let application = ApplicationCommands::new(session.clone(), Some(store.clone()))
            .override_application(store.clone(), controlled);
        Self {
            root,
            store,
            session,
            application,
        }
    }

    fn import_disabled(&self) -> YamlOverrideRecord {
        let source = self.root.join("existing.yaml");
        fs::write(&source, "mode: direct\n").unwrap();
        let record = self.store.import_paths([source]).unwrap().remove(0);
        self.store.set_enabled(&record.id, false).unwrap();
        self.store.load().unwrap().items.remove(0)
    }

    fn enable_candidate(&self) -> (YamlOverrideCatalog, YamlOverrideCatalog) {
        self.import_disabled();
        let before = self.store.load().unwrap();
        let mut next = before.clone();
        next.items[0].enabled = true;
        (before, next)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[derive(Debug)]
enum Reply {
    Accepted,
    Rejected,
    Lost,
}

struct Controller {
    url: String,
    entered: oneshot::Receiver<String>,
    release: oneshot::Sender<Reply>,
    server: tokio::task::JoinHandle<()>,
}

impl Controller {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (entered_tx, entered) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let (text, body) = zenclash_core::test_support::read_controller_request(&mut stream)
                .await
                .unwrap()
                .into_parts();
            assert!(text.starts_with("PUT /configs?force=true "));
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            entered_tx
                .send(request["payload"].as_str().unwrap().to_owned())
                .unwrap();
            match release_rx.await.unwrap() {
                Reply::Accepted => stream.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").await.unwrap(),
                Reply::Rejected => stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 22\r\nConnection: close\r\n\r\n{\"message\":\"rejected\"}").await.unwrap(),
                Reply::Lost => {},
            }
        });
        Self {
            url,
            entered,
            release,
            server,
        }
    }
}

#[tokio::test]
async fn catalog_change_without_active_profile_returns_a_saved_only_receipt() {
    let fixture = Fixture::new("http://127.0.0.1:9", false);
    let (before, next) = fixture.enable_candidate();
    let receipt = fixture
        .application
        .replace_catalog(before, next.clone())
        .await
        .unwrap();
    assert!(receipt.runtime_outcome().is_none());
    assert_eq!(receipt.into_catalog(), next);
    assert!(fixture.store.load().unwrap().items[0].enabled);
}

#[tokio::test]
async fn import_without_active_profile_returns_its_managed_catalog_and_count() {
    let fixture = Fixture::new("http://127.0.0.1:9", false);
    let source = fixture.root.join("new.yaml");
    fs::write(&source, "mode: direct\n").unwrap();
    let receipt = fixture
        .application
        .import_paths(vec![source])
        .await
        .unwrap();
    assert_eq!(receipt.imported_count(), 1);
    assert!(receipt.runtime_outcome().is_none());
    assert_eq!(receipt.into_catalog(), fixture.store.load().unwrap());
}

#[tokio::test]
async fn cancelled_waiter_keeps_compensation_owned_until_shutdown() {
    let controller = Controller::start().await;
    let fixture = Fixture::new(&controller.url, true);
    let (before, next) = fixture.enable_candidate();
    let expected = before.clone();
    let application = fixture.application.clone();
    let waiter = tokio::spawn(async move { application.replace_catalog(before, next).await });
    let payload = controller.entered.await.unwrap();
    assert!(payload.contains("mode: direct"));
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    let session = fixture.session.clone();
    let mut stopping = tokio::spawn(async move { session.shutdown().await });
    let early = tokio::time::timeout(Duration::from_millis(100), &mut stopping).await;
    let waited = early.is_err();
    controller.release.send(Reply::Rejected).unwrap();
    controller.server.await.unwrap();
    if let Ok(result) = early {
        result.unwrap().unwrap();
    } else {
        stopping.await.unwrap().unwrap();
    }
    assert!(
        waited,
        "shutdown released before override compensation finished"
    );
    assert_eq!(fixture.store.load().unwrap(), expected);
}

#[tokio::test]
async fn compensation_preserves_catalog_changes_from_another_writer() {
    let controller = Controller::start().await;
    let fixture = Fixture::new(&controller.url, true);
    let (before, next) = fixture.enable_candidate();
    let id = next.items[0].id.clone();
    let application = fixture.application.clone();
    let mut changing = tokio::spawn(async move { application.replace_catalog(before, next).await });
    tokio::select! {
        entered = controller.entered => { entered.unwrap(); }
        completed = &mut changing => panic!("application finished before controller request: {completed:?}"),
    }
    fixture.store.set_enabled(&id, false).unwrap();
    controller.release.send(Reply::Rejected).unwrap();
    let failure = changing.await.unwrap().unwrap_err();
    controller.server.await.unwrap();
    assert_eq!(failure.kind(), OverrideFailureKind::CompensationRequired);
    assert!(!fixture.store.load().unwrap().items[0].enabled);
    assert_eq!(
        failure.into_catalog().unwrap(),
        fixture.store.load().unwrap()
    );
}

#[tokio::test]
async fn lost_response_does_not_claim_that_catalog_compensation_confirms_the_runtime() {
    let controller = Controller::start().await;
    let fixture = Fixture::new(&controller.url, true);
    let (before, next) = fixture.enable_candidate();
    let expected = before.clone();
    let application = fixture.application.clone();
    let changing = tokio::spawn(async move { application.replace_catalog(before, next).await });
    controller.entered.await.unwrap();
    controller.release.send(Reply::Lost).unwrap();
    let failure = changing.await.unwrap().unwrap_err();
    controller.server.await.unwrap();
    assert_eq!(failure.kind(), OverrideFailureKind::Compensated);
    assert!(failure.is_runtime_unconfirmed());
    assert!(!failure.to_string().contains("%{error}"));
    assert_eq!(fixture.store.load().unwrap(), expected);
}

#[tokio::test]
async fn rejected_import_removes_only_its_unchanged_managed_copies() {
    let controller = Controller::start().await;
    let fixture = Fixture::new(&controller.url, true);
    let source = fixture.root.join("new.yaml");
    fs::write(&source, "mode: direct\n").unwrap();
    let application = fixture.application.clone();
    let importing = tokio::spawn(async move { application.import_paths(vec![source]).await });
    controller.entered.await.unwrap();
    controller.release.send(Reply::Rejected).unwrap();
    let failure = importing.await.unwrap().unwrap_err();
    controller.server.await.unwrap();
    assert_eq!(failure.kind(), OverrideFailureKind::Compensated);
    assert!(fixture.store.load().unwrap().items.is_empty());
    assert_eq!(
        fs::read_dir(fixture.store.root().join("files"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn incomplete_import_compensation_preserves_the_changed_copy_and_removes_the_rest() {
    let controller = Controller::start().await;
    let fixture = Fixture::new(&controller.url, true);
    let first = fixture.root.join("first.yaml");
    let second = fixture.root.join("second.yaml");
    fs::write(&first, "mode: direct\n").unwrap();
    fs::write(&second, "mode: rule\n").unwrap();
    let application = fixture.application.clone();
    let importing =
        tokio::spawn(async move { application.import_paths(vec![first, second]).await });
    controller.entered.await.unwrap();
    let imported = fixture.store.load().unwrap();
    let preserved = imported.items[0].clone();
    fixture.store.set_enabled(&preserved.id, false).unwrap();
    controller.release.send(Reply::Rejected).unwrap();
    let failure = importing.await.unwrap().unwrap_err();
    controller.server.await.unwrap();
    assert_eq!(failure.kind(), OverrideFailureKind::CompensationRequired);
    let actual = failure.into_catalog().unwrap();
    assert_eq!(actual.items.len(), 1);
    assert_eq!(actual.items[0].id, preserved.id);
    assert!(!actual.items[0].enabled);
    assert_eq!(
        fs::read_dir(fixture.store.root().join("files"))
            .unwrap()
            .count(),
        1
    );
}

#[tokio::test]
async fn stale_catalog_is_rejected_without_runtime_application() {
    let fixture = Fixture::new("http://127.0.0.1:9", true);
    let (before, next) = fixture.enable_candidate();
    let actual = fixture.store.load().unwrap();
    fixture
        .store
        .set_enabled(&actual.items[0].id, true)
        .unwrap();
    let failure = fixture
        .application
        .replace_catalog(before, next)
        .await
        .unwrap_err();
    assert_eq!(failure.kind(), OverrideFailureKind::Rejected);
    assert!(fixture.store.load().unwrap().items[0].enabled);
}

#[tokio::test]
async fn backup_admission_waits_for_an_owned_override_application() {
    let controller = Controller::start().await;
    let fixture = Fixture::new(&controller.url, true);
    let (before, next) = fixture.enable_candidate();
    let application = fixture.application.clone();
    let changing = tokio::spawn(async move { application.replace_catalog(before, next).await });
    controller.entered.await.unwrap();
    let session = fixture.session.clone();
    let mut backup = tokio::spawn(async move { session.begin_backup_restore().await });
    let early = tokio::time::timeout(Duration::from_millis(100), &mut backup).await;
    let waited = early.is_err();
    controller.release.send(Reply::Accepted).unwrap();
    controller.server.await.unwrap();
    let receipt = changing.await.unwrap().unwrap();
    assert!(receipt.runtime_outcome().is_some());
    if let Ok(result) = early {
        drop(result.unwrap().unwrap());
    } else {
        drop(backup.await.unwrap().unwrap());
    }
    assert!(
        waited,
        "backup admission overlapped the override completion"
    );
}
